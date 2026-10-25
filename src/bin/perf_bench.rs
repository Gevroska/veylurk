use clap::Parser;
use std::{fs::OpenOptions, io::Write, path::PathBuf, process::ExitCode};
use veylurk_probe::benchmark::{run, BenchmarkConfig};

#[derive(Debug, Parser)]
#[command(
    name = "veylurk-perf-bench",
    about = "Short, bounded offline synthetic SQLite performance measurement",
    long_about = "Creates a fresh isolated directory and database; refuses every existing output directory. Uses only the real local Store API with deterministic synthetic JOIN/NAMES/PART receipts. Opens no network connection and reads no Twitch credentials or existing database. JSON timings are synthetic workload measurements, never live Twitch throughput or reliability evidence. The elapsed-time budget is checked between operations; an in-flight durable filesystem operation is allowed to finish."
)]
struct Args {
    /// New directory whose parent already exists. Never an existing database directory.
    #[arg(long)]
    output_dir: PathBuf,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=50))]
    channels: u32,
    #[arg(long, default_value_t = 2200, value_parser = clap::value_parser!(u32).range(200..=20000))]
    events_per_channel: u32,
    /// Global mixed-room transaction size; production currently uses 512.
    #[arg(long, default_value_t = 512, value_parser = clap::value_parser!(u32).range(1..=10000))]
    batch_size: u32,
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..=20))]
    query_repetitions: u32,
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u32).range(1..=600))]
    max_seconds: u32,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = Args::parse();
    let config = BenchmarkConfig {
        channels: args.channels as usize,
        events_per_channel: args.events_per_channel as usize,
        batch_size: args.batch_size as usize,
        query_repetitions: args.query_repetitions as usize,
        max_seconds: args.max_seconds as u64,
    };
    let result = async {
        let report = run(&args.output_dir, config).await?;
        let json = serde_json::to_string_pretty(&report)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(args.output_dir.join("report.json"))?;
        writeln!(output, "{json}")?;
        println!("{json}");
        Ok::<_, Box<dyn std::error::Error>>(report.success)
    }
    .await;
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("Offline benchmark failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_requires_fresh_destination_and_bounds_work() {
        assert!(Args::try_parse_from(["bench"]).is_err());
        for pair in [
            ["--channels", "51"],
            ["--events-per-channel", "20001"],
            ["--batch-size", "10001"],
            ["--max-seconds", "601"],
            ["--query-repetitions", "0"],
        ] {
            assert!(
                Args::try_parse_from(["bench", "--output-dir", "new", pair[0], pair[1]]).is_err()
            );
        }
        assert!(
            Args::try_parse_from(["bench", "--output-dir", "new", "--batch-size", "2048"]).is_ok()
        );
    }
}
