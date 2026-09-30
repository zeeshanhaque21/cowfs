use clap::Parser;
use cowfs_cli::{run, Cli};
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };
    ExitCode::from(u8::try_from(run(cli)).unwrap_or(1))
}
