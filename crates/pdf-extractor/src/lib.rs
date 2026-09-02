//! Turn a PDF into Markdown, whatever kind of PDF it is.
//!
//! ```no_run
//! let markdown = pdf_extractor::pdf_to_markdown_file("book.pdf")?;
//! println!("wrote {}", markdown.display());
//! # Ok::<(), pdf_extractor::Error>(())
//! ```
//!
//! There is one code path. Pages with a usable text layer are read from that
//! layer; pages without one — a scan, text drawn as vectors, a broken font
//! encoding — are rendered and OCR'd, and the two are fused into a single
//! document. Callers never have to know or decide which kind of PDF they have.
//! The source is only ever opened for reading.
//!
//! # Anything more than the default
//!
//! [`Options`] is the whole of it: a page selection, Markdown shape, a
//! password, and how much OCR to do.
//!
//! ```no_run
//! use pdf_extractor::{Ocr, Options, Progress};
//!
//! let conversion = Options::new()
//!     .pages(1..=20)
//!     .ocr(Ocr::Auto)
//!     .ocr_dpi(150.0)
//!     .progress(|event| {
//!         if let Progress::OcrPage { done, total } = event {
//!             eprintln!("OCR: page {done} of {total}");
//!         }
//!     })
//!     .convert("book.pdf")?;
//!
//! eprintln!("{}", conversion.summary());
//! # Ok::<(), pdf_extractor::Error>(())
//! ```
//!
//! # OCR costs nothing until it is needed
//!
//! PDFium and ONNX Runtime are `dlopen`ed, and the ~31 MB PP-OCRv6 Small model
//! set is downloaded, only when a page is actually routed to OCR. A born-digital
//! PDF converts in milliseconds and touches neither. [`init`] finds the two
//! libraries so nothing has to be exported first; `build.rs` fetches them.
//!
//! # If you want the OCR fan-out
//!
//! OCR of a long scan is spread across worker processes, which are this same
//! executable re-invoked. That only works if the host binary gives the library
//! first refusal on startup:
//!
//! ```no_run
//! use std::process::ExitCode;
//!
//! fn main() -> ExitCode {
//!     if let Some(code) = pdf_extractor::run_worker_if_spawned() {
//!         return code;
//!     }
//!     // ... the program's own command line, from here on.
//!     ExitCode::SUCCESS
//! }
//! ```
//!
//! Skip it and everything still works, single-process, once
//! [`Options::ocr_jobs`] is set to `Some(1)`; leave it at the default and a
//! spawned worker will report that the host never gave it its turn, by name.
//! `src/worker.rs` has the protocol, and the reasons it is shaped this way.

mod codec;
mod convert;
mod error;
mod options;
mod report;
mod runtime;
mod worker;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub use error::Error;
pub use options::{DEFAULT_OCR_DPI, Ocr, Options};
pub use report::{Conversion, PdfType, Progress, Survey, format_duration, summarize_pages};
pub use runtime::init;

/// Convert `pdf` to a Markdown file beside it, and return where that landed.
///
/// The output takes the input's name with a `.md` extension. Use
/// [`Options::convert_to_file`] to put it somewhere else, or
/// [`pdf_to_markdown`] to keep it in memory.
pub fn pdf_to_markdown_file(pdf: impl AsRef<Path>) -> Result<PathBuf, Error> {
    let pdf = pdf.as_ref();
    let markdown = default_output(pdf);
    Options::new().convert_to_file(pdf, &markdown)?;
    Ok(markdown)
}

/// Convert `pdf` and return the Markdown.
///
/// Fails with [`Error::NoText`] if the document yields nothing at all.
pub fn pdf_to_markdown(pdf: impl AsRef<Path>) -> Result<String, Error> {
    Options::new().convert(pdf)?.markdown.ok_or(Error::NoText)
}

/// Where [`pdf_to_markdown_file`] writes: the input's name, with `.md`.
pub fn default_output(pdf: impl AsRef<Path>) -> PathBuf {
    pdf.as_ref().with_extension("md")
}

/// Do this process's turn as an OCR worker, if that is what it was spawned as.
///
/// Returns `None` in the ordinary case — nothing spawned this, carry on — and
/// `Some(code)` when the process was a worker, in which case it has done its
/// work and `main` should return that code and stop.
///
/// Call it as the *first* thing `main` does. A worker is this same executable
/// with no arguments and one environment variable, so a host that parses its
/// own command line first will reject the invocation before the library ever
/// sees it. See [the crate docs](crate) for the four lines this takes.
pub fn run_worker_if_spawned() -> Option<ExitCode> {
    worker::run_if_spawned()
}

impl Options {
    /// What this PDF is, and which of its pages have no usable text layer —
    /// without extracting anything.
    ///
    /// Cheap next to a conversion, and worth running first for the one thing a
    /// caller most wants before a run that can take minutes: whether this is
    /// going to be a minutes-long run.
    pub fn survey(&self, pdf: impl AsRef<Path>) -> Result<Survey, Error> {
        convert::survey(pdf.as_ref(), self)
    }

    /// Convert `pdf` and return the Markdown along with what it took.
    pub fn convert(&self, pdf: impl AsRef<Path>) -> Result<Conversion, Error> {
        convert::convert(pdf.as_ref(), self)
    }

    /// Convert `pdf` and write the Markdown to `markdown`.
    ///
    /// Refuses to write over the source, and fails with [`Error::NoText`]
    /// rather than leaving an empty file behind.
    pub fn convert_to_file(
        &self,
        pdf: impl AsRef<Path>,
        markdown: impl AsRef<Path>,
    ) -> Result<Conversion, Error> {
        let (pdf, markdown) = (pdf.as_ref(), markdown.as_ref());
        if same_file(pdf, markdown) {
            return Err(Error::WouldOverwriteInput(pdf.to_path_buf()));
        }
        let conversion = self.convert(pdf)?;
        let text = conversion.markdown.as_deref().ok_or(Error::NoText)?;
        std::fs::write(markdown, text).map_err(|error| Error::io("write", markdown, error))?;
        Ok(conversion)
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
    fn the_default_output_swaps_the_extension() {
        assert_eq!(default_output("a/book.pdf"), Path::new("a/book.md"));
        assert_eq!(default_output("no-extension"), Path::new("no-extension.md"));
    }

    #[test]
    fn converting_onto_the_input_is_refused() {
        // Neither path exists, so this exercises the textual fallback — which
        // is the case that matters, since the output usually does not exist.
        let error = Options::new()
            .convert_to_file("book.pdf", "book.pdf")
            .expect_err("refused");
        assert!(matches!(error, Error::WouldOverwriteInput(_)), "{error:?}");
    }

    #[test]
    fn a_missing_input_is_an_io_error_not_a_panic() {
        let error = Options::new()
            .convert("definitely/not/here.pdf")
            .expect_err("failed");
        assert!(matches!(error, Error::Io { .. }), "{error:?}");
    }
}
