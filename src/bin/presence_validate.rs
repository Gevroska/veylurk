use clap::{Parser, Subcommand};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    process::ExitCode,
};
use veylurk_probe::presence_validation::{self, ReferenceInput, SimulationConfig};

#[derive(Debug, Parser)]
#[command(
    name = "veylurk-presence-validate",
    about = "Bounded offline Community model validation; no network or database access"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Debug, Subcommand)]
enum Command {
    /// Construct independent truth paths and reuse unchanged production inference.
    Simulate {
        #[arg(long,default_value_t=200,value_parser=clap::value_parser!(u32).range(2..=200))]
        trajectories: u32,
        #[arg(long,default_value_t=120,value_parser=clap::value_parser!(u32).range(3..=120))]
        observations: u32,
        #[arg(long,default_value_t=120,value_parser=clap::value_parser!(u32).range(1..=120))]
        max_seconds: u32,
        /// Fresh output file; existing files are never overwritten.
        #[arg(long)]
        output: PathBuf,
    },
    /// Evaluate frozen opaque reference JSON, or report missing independent reference.
    Reference {
        #[arg(long)]
        input: Option<PathBuf>,
        /// Fresh output file; existing files are never overwritten.
        #[arg(long)]
        output: PathBuf,
    },
}
fn run(args: Args) -> Result<bool, Box<dyn std::error::Error>> {
    // Reserve the destination before expensive work; failed parsing leaves an empty
    // reserved file, never an apparently complete old report.
    let output = match &args.command {
        Command::Simulate { output, .. } | Command::Reference { output, .. } => output,
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    let report = match args.command {
        Command::Simulate {
            trajectories,
            observations,
            max_seconds,
            ..
        } => presence_validation::simulate(SimulationConfig {
            trajectories: trajectories as usize,
            observations: observations as usize,
            max_seconds: max_seconds as u64,
        })?,
        Command::Reference {
            input: Some(input), ..
        } => {
            let mut bytes = Vec::new();
            File::open(input)?
                .take(8 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 8 * 1024 * 1024 {
                return Err("reference JSON exceeds 8 MiB".into());
            }
            let input: ReferenceInput = serde_json::from_slice(&bytes)?;
            presence_validation::evaluate_reference(input)?
        }
        Command::Reference { input: None, .. } => presence_validation::missing_reference(),
    };
    serde_json::to_writer_pretty(&mut file, &report)?;
    writeln!(file)?;
    file.flush()?;
    println!("{}", serde_json::to_string(&report)?);
    // Missing evidence is a successfully recorded factual status. Runtime caps fail
    // the command while retaining the structured incomplete report.
    Ok(report["status"] != "incomplete_runtime_cap")
}
fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("Offline presence validation failed: {error}");
            ExitCode::FAILURE
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cli_bounds_and_missing_reference_mode() {
        for args in [
            vec![
                "validate",
                "simulate",
                "--output",
                "new",
                "--trajectories",
                "201",
            ],
            vec![
                "validate",
                "simulate",
                "--output",
                "new",
                "--observations",
                "121",
            ],
            vec![
                "validate",
                "simulate",
                "--output",
                "new",
                "--max-seconds",
                "121",
            ],
        ] {
            assert!(Args::try_parse_from(args).is_err());
        }
        assert!(Args::try_parse_from(["validate", "reference", "--output", "new"]).is_ok());
    }
}
