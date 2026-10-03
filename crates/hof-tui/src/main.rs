//! `hofvarpnir-tui` — terminal client for the Hofvarpnir API.
//!
//! Thin binary; everything lives in the `hof_tui` library so integration
//! tests can drive the app.

use std::process::ExitCode;

use hof_tui::run::{Exit, RunError, run};

#[tokio::main]
async fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()).await {
        Ok(Exit::Quit) => ExitCode::SUCCESS,
        Ok(Exit::Help(usage)) => {
            println!("{usage}");
            ExitCode::SUCCESS
        }
        Err(RunError::Config(e)) => {
            eprintln!("error: {e}\n\nusage: hofvarpnir-tui [--api-url URL] [--token TOKEN]");
            ExitCode::FAILURE
        }
        Err(RunError::Other(e)) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
