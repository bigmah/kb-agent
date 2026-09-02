//! kb-agent — the command that turns a directory of documents into something
//! you can ask.
//!
//! The work lives in the libraries: [`pdf_extractor`] converts, [`agent`]
//! makes the requests, [`kb`] decides which document goes to which request.
//! Everything here is the things a library should not decide: what the flags
//! are called, where the output goes, and what gets said on the way.

mod build;
mod cli;
mod convert;
mod progress;
mod query;
mod status;

use std::process::ExitCode;

use crate::cli::Command;

fn main() -> ExitCode {
    // Before anything else, including the command line: an OCR worker is this
    // same executable, re-invoked with no arguments. If that is what this
    // process is, it does its share of the pages and exits here.
    if let Some(code) = pdf_extractor::run_worker_if_spawned() {
        return code;
    }

    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, String> {
    match cli::parse(std::env::args().skip(1))? {
        Command::Help(usage) => {
            println!("{usage}");
            Ok(ExitCode::SUCCESS)
        }
        Command::Version => {
            println!("kb-agent {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        Command::Convert(invocation) => convert::run(*invocation).map(|()| ExitCode::SUCCESS),
        Command::Build(args) => build::run(*args),
        Command::Status(dir) => status::run(&dir).map(|()| ExitCode::SUCCESS),
        Command::Query(args) => query::run(*args).map(|()| ExitCode::SUCCESS),
    }
}

/// Run one future to completion on a runtime built for it.
///
/// Current-thread: the work is HTTP requests, many at once, and a single
/// thread drives any number of those. A command-line tool should not stand
/// up a thread pool to wait on the network.
pub(crate) fn block_on<T>(future: impl Future<Output = T>) -> Result<T, String> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime: {error}"))?
        .block_on(future))
}
