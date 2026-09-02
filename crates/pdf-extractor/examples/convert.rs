//! The smallest useful program built on this library.
//!
//! ```text
//! cargo run --release --example convert -- book.pdf
//! ```
//!
//! Note the first four lines of `main`. Without them the OCR fan-out has no
//! worker to spawn — see [`pdf_extractor::run_worker_if_spawned`].

use std::process::ExitCode;

fn main() -> ExitCode {
    if let Some(code) = pdf_extractor::run_worker_if_spawned() {
        return code;
    }

    let Some(input) = std::env::args().nth(1) else {
        eprintln!("usage: convert <input.pdf>");
        return ExitCode::FAILURE;
    };

    match pdf_extractor::pdf_to_markdown_file(&input) {
        Ok(markdown) => {
            println!("wrote {}", markdown.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
