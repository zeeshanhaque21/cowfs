use clap::Parser;
use cowfs_cli::{run, usage_error, Cli};
use std::ffi::OsString;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    let code = match Cli::try_parse_from(&args) {
        Ok(cli) => run(cli),
        Err(e) => {
            if matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                e.print().ok();
                return ExitCode::from(0);
            }
            let json = args.iter().any(|a| a == "--json");
            if e.kind() == clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand && !json
            {
                e.print().ok();
                return ExitCode::from(2);
            }
            usage_error(json, first_line(&e.render().to_string()))
        }
    };
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn first_line(msg: &str) -> &str {
    msg.lines().next().unwrap_or("bad arguments")
}
