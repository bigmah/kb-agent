//! The smallest useful program built on this library.
//!
//! ```text
//! cargo run --release --example summarize -- notes.md
//! ```
//!
//! Prints the plan first, because the next thing it does costs money — and
//! because the plan is where a document too big to summarize is caught.

use agent::Options;

fn main() -> std::process::ExitCode {
    let Some(input) = std::env::args().nth(1) else {
        eprintln!("usage: summarize <input.md>");
        return std::process::ExitCode::FAILURE;
    };

    match run(&Options::new(), &input) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(options: &Options, input: &str) -> Result<(), agent::Error> {
    eprintln!("{}", options.plan(input)?.describe());

    let output = agent::default_output(input);
    let summary = options.summarize_to_file_blocking(input, &output)?;

    eprintln!("{}", summary.describe());
    eprintln!(
        "wrote {} ({} bytes)",
        output.display(),
        summary.markdown.len()
    );
    Ok(())
}
