use clap::Parser;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};
use veylurk_probe::browser::{find_browser, normalize_channels, BrowserSession};
use veylurk_probe::overlap;

#[derive(Debug, Parser)]
#[command(
    name = "veylurk-browser-probe",
    about = "Bounded ordinary-UI feasibility probe for Twitch chat-connected samples",
    long_about = "Launches a clean dedicated Brave profile, opens Twitch's ordinary popout chat, and reads only rendered viewer-panel DOM through local browser automation. It never copies a user profile, credentials, cookies, integrity tokens, or private network responses. Account names remain in memory and only aggregate JSON lines are printed."
)]
struct Args {
    /// Twitch channel login. Repeat for up to three concurrent tabs sampled round-robin.
    #[arg(short, long, required = true, action = clap::ArgAction::Append)]
    channel: Vec<String>,

    /// Number of samples per channel (1-4).
    #[arg(short, long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=4))]
    samples: u8,

    /// Delay between samples in seconds (15-3600).
    #[arg(short, long, default_value_t = 15, value_parser = clap::value_parser!(u64).range(15..=3600))]
    interval: u64,

    /// Preflight wall-clock budget checked before each sample and wait (30-3600 seconds).
    /// An in-flight browser protocol operation has its own 25-second cap.
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(30..=3600))]
    timeout: u64,

    /// Explicit Brave Browser executable.
    #[arg(long)]
    browser_path: Option<PathBuf>,

    /// Save the current public page as a local PNG if DOM collection fails.
    #[arg(long, value_name = "PNG_PATH")]
    failure_screenshot: Option<PathBuf>,
}

#[derive(Serialize)]
struct Output<'a> {
    event: &'static str,
    channel: &'a str,
    sample: u8,
    elapsed_ms: u128,
    rendered_unique_rows: usize,
    overlap_previous: Option<usize>,
    cumulative_unique: usize,
    role_lists: usize,
    scroll_rounds: usize,
    reached_scroll_end: bool,
    fingerprint: String,
    evidence_scope: &'static str,
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("browser probe failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<(), String> {
    let channels = normalize_channels(&args.channel)?;
    let browser = find_browser(args.browser_path.as_deref()).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let budget = Duration::from_secs(args.timeout);
    let mut session =
        BrowserSession::launch(&browser, Duration::from_secs(10)).map_err(|e| e.to_string())?;
    let mut success_count = 0usize;
    let targets: Vec<_> = channels
        .iter()
        .map(|channel| session.open_channel(channel).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let mut previous: Vec<Option<HashSet<String>>> = vec![None; channels.len()];
    let mut cumulative: Vec<HashSet<String>> = vec![HashSet::new(); channels.len()];

    for sample_number in 1..=args.samples {
        let round_started = Instant::now();
        for (index, (channel, target)) in channels.iter().zip(&targets).enumerate() {
            if started.elapsed().saturating_add(Duration::from_secs(22)) >= budget {
                return Err(format!(
                    "global timeout leaves insufficient room for another bounded UI sample after {success_count} success(es)"
                ));
            }
            let sample = match session.collect(target) {
                Ok(sample) => sample,
                Err(error) => {
                    if let Some(path) = args.failure_screenshot.as_deref() {
                        match session.capture_failure_screenshot(target, path) {
                            Ok(()) => eprintln!("failure screenshot saved to {}", path.display()),
                            Err(capture_error) => {
                                eprintln!("failure screenshot unavailable: {capture_error}")
                            }
                        }
                    }
                    return Err(format!(
                        "{error}; browser automation stopped without challenge handling or fallback transport"
                    ));
                }
            };
            let current: HashSet<_> = sample.usernames.into_iter().collect();
            let previous_overlap = previous[index].as_ref().map(|set| overlap(set, &current));
            cumulative[index].extend(current.iter().cloned());
            let mut normalized: Vec<_> = current.iter().collect();
            normalized.sort_unstable();
            let fingerprint = format!(
                "{:x}",
                Sha256::digest(
                    normalized
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                        .as_bytes()
                )
            );
            let output = Output {
                event: "rendered_dom_sample",
                channel,
                sample: sample_number,
                elapsed_ms: started.elapsed().as_millis(),
                rendered_unique_rows: current.len(),
                overlap_previous: previous_overlap,
                cumulative_unique: cumulative[index].len(),
                role_lists: sample.role_lists,
                scroll_rounds: sample.scroll_rounds,
                reached_scroll_end: sample.reached_end,
                fingerprint,
                evidence_scope:
                    "one reopened ordinary viewer panel; rendered rows only; no independence claim",
            };
            println!(
                "{}",
                serde_json::to_string(&output).map_err(|e| e.to_string())?
            );
            previous[index] = Some(current);
            success_count += 1;
        }
        if sample_number < args.samples {
            let delay = Duration::from_secs(args.interval).saturating_sub(round_started.elapsed());
            if started
                .elapsed()
                .saturating_add(delay + Duration::from_secs(25))
                >= budget
            {
                return Err(format!(
                    "global timeout would be exceeded after {success_count} success(es)"
                ));
            }
            thread::sleep(delay);
        }
    }
    for target in targets {
        session.close_channel(target);
    }
    if success_count == 0 {
        return Err("no successful rendered DOM samples".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_enforces_minimum_interval_and_channel_presence() {
        assert!(Args::try_parse_from(["probe", "--channel", "x", "--interval", "14"]).is_err());
        assert!(Args::try_parse_from(["probe"]).is_err());
        assert!(Args::try_parse_from(["probe", "--channel", "x", "--samples", "5"]).is_err());
        let args = Args::try_parse_from([
            "probe",
            "--channel",
            "x",
            "--failure-screenshot",
            "failure.png",
        ])
        .unwrap();
        assert_eq!(args.failure_screenshot, Some(PathBuf::from("failure.png")));
    }
}
