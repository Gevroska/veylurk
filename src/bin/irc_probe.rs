use clap::Parser;
use std::process::ExitCode;
use std::time::{Duration, Instant};
use veylurk_probe::irc::{normalize_channels, observe};

#[derive(Debug, Parser)]
#[command(
    name = "veylurk-irc-probe",
    about = "Bounded receive-only anonymous Twitch IRC membership observations",
    long_about = "Uses one anonymous WebSocket IRC connection for up to three channels. Prints aggregate observations only; account names stay in memory. Completeness is unknown, especially in large rooms. Accounts are chat-connected observations, not video viewers or watch-time evidence. A NAMES terminator does not establish complete or empty membership. JOIN/PART counts include bootstrap, duplicates, and possible delayed roster delivery; they are not exact arrivals or departures. No browser or extension is required."
)]
struct Args {
    #[arg(short, long, required = true, action = clap::ArgAction::Append)]
    channel: Vec<String>,
    /// Observe JOIN/PART traffic after all initial NAMES terminators (0-300 seconds).
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(0..=300))]
    window: u64,
    /// Overall deadline including setup and up to four seconds reserved for shutdown (15-600 seconds).
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(15..=600))]
    timeout: u64,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let started = Instant::now();
    let result = async {
        let channels = normalize_channels(&args.channel)?;
        observe(
            &channels,
            Duration::from_secs(args.window),
            Duration::from_secs(args.timeout),
        )
        .await
    }
    .await;
    match result {
        Ok(observations) => {
            for observation in observations {
                println!(
                    "{}",
                    serde_json::json!({ "event": "irc_membership_observation", "elapsed_ms": started.elapsed().as_millis(), "observation": observation })
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("IRC probe failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cli_requires_channel_and_bounds_time() {
        assert!(Args::try_parse_from(["probe"]).is_err());
        assert!(Args::try_parse_from(["probe", "-c", "test", "--window", "301"]).is_err());
        assert!(Args::try_parse_from(["probe", "-c", "test", "--timeout", "14"]).is_err());
        assert!(Args::try_parse_from(["probe", "-c", "test", "--window", "0"]).is_ok());
    }
}
