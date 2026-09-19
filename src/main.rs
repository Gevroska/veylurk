use clap::Parser;
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE, ORIGIN, RETRY_AFTER, USER_AGENT};
use serde::Serialize;
use std::collections::HashSet;
use std::io::Read;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};
use veylurk_probe::{overlap, parse_response, request_body, ProbeError, TWITCH_WEB_CLIENT_ID};

#[derive(Debug, Parser)]
#[command(
    name = "veylurk-probe",
    about = "Bounded feasibility probe for Twitch community samples",
    long_about = "Runs a small, sequential, anonymous transport experiment and prints aggregate JSON lines. It never prints or persists account names. This is not the Veylurk application."
)]
struct Args {
    /// Twitch channel login. Repeat the option to probe more than one channel.
    #[arg(short, long, required = true, action = clap::ArgAction::Append)]
    channel: Vec<String>,

    /// Number of samples per channel (1-20).
    #[arg(short, long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(1..=20))]
    samples: u8,

    /// Delay between samples in seconds (15-3600).
    #[arg(short, long, default_value_t = 15, value_parser = clap::value_parser!(u64).range(15..=3600))]
    interval: u64,

    /// Hard wall-clock budget for the complete run in seconds (15-3600).
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(15..=3600))]
    timeout: u64,
}

#[derive(Serialize)]
struct Output<'a> {
    event: &'a str,
    channel: &'a str,
    sample: u8,
    elapsed_ms: u128,
    http_ms: u128,
    reported_chatters: u64,
    unique_rows: usize,
    overlap_previous: Option<usize>,
    cumulative_unique: usize,
    fingerprint: &'a str,
    stream_metadata_present: bool,
    stream_id: Option<&'a str>,
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("probe failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<(), String> {
    let channels = normalized_channels(&args.channel)?;
    let run_started = Instant::now();
    let budget = Duration::from_secs(args.timeout);
    let client = build_client()?;
    let mut successful = 0usize;

    for channel in &channels {
        let mut previous = None;
        let mut cumulative = HashSet::new();
        for sample_number in 1..=args.samples {
            if run_started.elapsed() >= budget {
                return Err(format!(
                    "global timeout reached after {successful} successful sample(s)"
                ));
            }
            if sample_number > 1 {
                let delay = Duration::from_secs(args.interval);
                if run_started.elapsed().saturating_add(delay) >= budget {
                    return Err(format!(
                        "global timeout would be exceeded after {successful} successful sample(s)"
                    ));
                }
                thread::sleep(delay);
            }
            let request_started = Instant::now();
            let response = send_with_bounded_retry(&client, channel, run_started, budget)?;
            let status = response.status();
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            if status.as_u16() == 429 {
                return Err(format!(
                    "rate limited (HTTP 429); stopped without retry{}",
                    retry_after
                        .map(|v| format!(", Retry-After={v}"))
                        .unwrap_or_default()
                ));
            }
            if matches!(status.as_u16(), 401 | 403) {
                return Err(format!("authorization challenge (HTTP {status}); stopped"));
            }
            if !status.is_success() {
                return Err(format!("unexpected HTTP status {status}"));
            }
            const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
            if response
                .content_length()
                .is_some_and(|length| length > MAX_RESPONSE_BYTES)
            {
                return Err("response exceeds the 2 MiB safety limit".into());
            }
            let mut bytes = Vec::new();
            response
                .take(MAX_RESPONSE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("response read failed: {e}"))?;
            if bytes.len() as u64 > MAX_RESPONSE_BYTES {
                return Err("response exceeds the 2 MiB safety limit".into());
            }
            let parsed = parse_response(channel, &bytes).map_err(|error| match error {
                ProbeError::IntegrityChallenge => {
                    format!("{error}; stopped without bypass attempt")
                }
                ProbeError::RateLimited | ProbeError::Unauthorized => format!("{error}; stopped"),
                _ => error.to_string(),
            })?;
            let previous_overlap = previous.as_ref().map(|p| overlap(p, &parsed.unique_logins));
            cumulative.extend(parsed.unique_logins.iter().cloned());
            let output = Output {
                event: "sample",
                channel,
                sample: sample_number,
                elapsed_ms: run_started.elapsed().as_millis(),
                http_ms: request_started.elapsed().as_millis(),
                reported_chatters: parsed.reported_count,
                unique_rows: parsed.unique_logins.len(),
                overlap_previous: previous_overlap,
                cumulative_unique: cumulative.len(),
                fingerprint: &parsed.fingerprint,
                stream_metadata_present: parsed.stream_id.is_some(),
                stream_id: parsed.stream_id.as_deref(),
            };
            println!(
                "{}",
                serde_json::to_string(&output).map_err(|e| e.to_string())?
            );
            previous = Some(parsed.unique_logins);
            successful += 1;
        }
    }
    if successful == 0 {
        return Err("no successful samples".into());
    }
    Ok(())
}

fn normalized_channels(input: &[String]) -> Result<Vec<String>, String> {
    let mut seen = HashSet::new();
    let mut channels = Vec::new();
    for raw in input {
        let channel = raw.trim().to_ascii_lowercase();
        if channel.is_empty()
            || channel.len() > 25
            || !channel
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(format!("invalid Twitch channel login: {raw:?}"));
        }
        if seen.insert(channel.clone()) {
            channels.push(channel);
            if channels.len() > 10 {
                return Err("at most 10 distinct channels are allowed in a probe".into());
            }
        }
    }
    Ok(channels)
}

fn build_client() -> Result<Client, String> {
    let mut headers = HeaderMap::new();
    headers.insert("Client-ID", HeaderValue::from_static(TWITCH_WEB_CLIENT_ID));
    headers.insert(ORIGIN, HeaderValue::from_static("https://www.twitch.tv"));
    headers.insert(USER_AGENT, HeaderValue::from_static("Veylurk-Stage2/0.1"));
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain;charset=UTF-8"),
    );
    Client::builder()
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client setup failed: {e}"))
}

fn send_with_bounded_retry(
    client: &Client,
    channel: &str,
    started: Instant,
    budget: Duration,
) -> Result<Response, String> {
    let mut last_error = None;
    for attempt in 0..=2u32 {
        let Some(remaining) = budget.checked_sub(started.elapsed()) else {
            break;
        };
        if remaining.is_zero() {
            break;
        }
        match client
            .post("https://gql.twitch.tv/gql")
            .timeout(remaining.min(Duration::from_secs(15)))
            .body(request_body(channel).to_string())
            .send()
        {
            Ok(response) => return Ok(response),
            Err(error) => last_error = Some(error),
        }
        if attempt < 2 {
            let delay = Duration::from_secs(1 << attempt);
            if started.elapsed().saturating_add(delay) >= budget {
                break;
            }
            thread::sleep(delay);
        }
    }
    Err(format!(
        "transport failed after at most 3 attempts: {}",
        last_error
            .map(|e| e.to_string())
            .unwrap_or_else(|| "global timeout".into())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_configuration_is_case_insensitively_deduplicated() {
        let input = vec!["OuaisEddy".into(), "ouaiseddy".into(), "dofla".into()];
        assert_eq!(normalized_channels(&input).unwrap(), ["ouaiseddy", "dofla"]);
    }

    #[test]
    fn invalid_channel_is_rejected() {
        assert!(normalized_channels(&["not/a/channel".into()]).is_err());
    }

    #[test]
    fn more_than_ten_channels_are_rejected() {
        let input: Vec<_> = (0..11).map(|n| format!("channel{n}")).collect();
        assert!(normalized_channels(&input).is_err());
    }
}
