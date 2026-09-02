//! kb-tree — the command that turns documents into Markdown.
//!
//! The conversion itself lives in [`pdf_extractor`]; everything here is the
//! things a library should not decide: what the flags are called, where the
//! output goes, and what gets said on the way.

mod cli;
mod progress;

use std::path::Path;
use std::process::ExitCode;

use pdf_extractor::{Conversion, Error};

use crate::cli::{Destination, Invocation, Parsed};

fn main() -> ExitCode {
    // Before anything else, including the command line: an OCR worker is this
    // same executable, re-invoked with no arguments. If that is what this
    // process is, it does its share of the pages and exits here.
    if let Some(code) = pdf_extractor::run_worker_if_spawned() {
        return code;
    }

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let invocation = match cli::parse(std::env::args().skip(1))? {
        Parsed::Help => {
            println!("{}", cli::USAGE);
            return Ok(());
        }
        Parsed::Version => {
            println!("kb-tree {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Parsed::Run(invocation) => *invocation,
    };

    // Must precede anything that could spawn a thread: it writes process
    // environment variables. A conversion does this itself, but by then this
    // program may have started threads of its own, so do it while there is
    // provably only one.
    pdf_extractor::init();

    // Cheap, and worth it before a run that can take minutes: refuse a
    // destination that would destroy the source now rather than after the work.
    if let Destination::File(path) = &invocation.destination
        && same_file(path, &invocation.input)
    {
        return Err(format!(
            "refusing to overwrite the input {} — pass --output or --stdout",
            invocation.input.display()
        ));
    }

    // Detection is cheap next to extraction, and buys the one thing a caller
    // most wants before a run that can take minutes: whether this is going to
    // be a minutes-long run.
    let survey = invocation
        .options
        .survey(&invocation.input)
        .map_err(describe)?;
    invocation.announce(&survey);

    let conversion = invocation
        .options
        .clone()
        .progress(progress::terminal())
        .convert(&invocation.input)
        .map_err(describe)?;

    eprintln!("{}", conversion.summary());
    for note in conversion.notes() {
        eprintln!("{note}");
    }

    deliver(&invocation, &conversion)
}

/// Put the Markdown where the command line said, once there is something to
/// put. The summary above has already run, so a document that yielded nothing
/// fails with the counts on screen to explain why.
fn deliver(invocation: &Invocation, conversion: &Conversion) -> Result<(), String> {
    let markdown = conversion
        .markdown
        .as_deref()
        .ok_or_else(|| Error::NoText.to_string())?;

    match &invocation.destination {
        Destination::File(path) => {
            std::fs::write(path, markdown)
                .map_err(|error| format!("could not write {}: {error}", path.display()))?;
            eprintln!("wrote {} ({} bytes)", path.display(), markdown.len());
        }
        Destination::Stdout => print!("{markdown}"),
    }
    Ok(())
}

/// Add the part of an explanation that only a command line can give: which
/// flag to reach for.
fn describe(error: Error) -> String {
    match error {
        Error::Encrypted => {
            "the PDF is encrypted — pass --password <pw> if you have one".to_string()
        }
        other => other.to_string(),
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        // The output usually does not exist yet, so fall back to a path compare.
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_output_still_compares_by_path() {
        assert!(same_file(Path::new("book.pdf"), Path::new("book.pdf")));
        assert!(!same_file(Path::new("book.pdf"), Path::new("book.md")));
    }

    #[test]
    fn encryption_gets_the_flag_that_fixes_it() {
        assert!(describe(Error::Encrypted).contains("--password"));
    }
}
