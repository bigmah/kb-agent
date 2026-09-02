//! One LLM request with a fresh context, in each of the roles a knowledge
//! base needs.
//!
//! ```no_run
//! # async fn run() -> Result<(), agent::Error> {
//! let summary = agent::summarize_markdown_file("book.md").await?;   // writes book_summary.md
//! println!("wrote {}", summary.display());
//! # Ok(()) }
//! ```
//!
//! Every role is the same shape: some material goes out under instructions
//! for that role, in one request, into a context holding nothing else, and
//! the model's text comes back read into whatever the role returns. The roles
//! are methods on [`Options`]:
//!
//! | Role | Sees | Returns |
//! | --- | --- | --- |
//! | [`Options::summarize`] | one document | a shorter document |
//! | [`Options::relevant`] | a question and one summary | whether the document is worth reading |
//! | [`Options::ask`] | a question and one document | what that document says, as points |
//! | [`Options::same_point`] | two points | whether they carry the same information |
//! | [`Options::merge_points`] | points judged the same | the one point that carries all of them |
//! | [`Options::answer`] | a question and the distilled points | the answer |
//!
//! The material is sent whole and never split or truncated: a request that
//! does not fit the context budget is refused with [`Error::TooLarge`],
//! because a summary of part of a document reads exactly like a summary of
//! all of it and nothing downstream could tell the difference.
//! [`Options::plan`] measures a document without sending it, so the refusal
//! need not cost a request to discover.
//!
//! Written against [rig](https://docs.rs/rig-core). ChatGPT by default — set
//! `OPENAI_API_KEY` and nothing else is required; see [`Provider`] to point it
//! elsewhere. Requests the provider turns away for being busy are retried
//! with a backoff; see [`Options::retries`].
//!
//! # Anything more than the default
//!
//! ```no_run
//! use agent::Options;
//!
//! # async fn run() -> Result<(), agent::Error> {
//! let plan = Options::new().plan("book.md")?;
//! eprintln!("{}", plan.describe());          // "book.md: ~533000 tokens, 1 request to gpt-5.6"
//!
//! let summary = Options::new()
//!     .focus("Keep every figure and date.")
//!     .summarize_to_file("book.md", "book_summary.md")
//!     .await?;
//!
//! eprintln!("{}", summary.describe());
//! # Ok(()) }
//! ```
//!
//! # If you are not already async
//!
//! The `blocking` feature (on by default) adds a synchronous mirror of the
//! summarizing API, so a program that is not built around a runtime can still
//! call it:
//!
//! ```no_run
//! let summary = agent::blocking::summarize_markdown_file("book.md")?;
//! # Ok::<(), agent::Error>(())
//! ```

mod answer;
mod ask;
mod error;
mod options;
mod provider;
mod reduce;
mod relevance;
mod report;
mod request;
mod summarize;

#[cfg(feature = "blocking")]
pub mod blocking;

use std::path::{Path, PathBuf};

pub use error::Error;
pub use options::{
    CHARS_PER_TOKEN, DEFAULT_CONTEXT_TOKENS, DEFAULT_MAX_TOKENS, DEFAULT_RETRIES, Options, Plan,
};
pub use provider::Provider;
pub use report::{Progress, Reply, Usage, format_duration};

/// Summarize `markdown` into a file beside it, and return where that landed.
///
/// The output is the input's name with `_summary.md` on the end — `book.md`
/// becomes `book_summary.md`. Use [`Options::summarize_to_file`] to put it
/// somewhere else, or [`summarize_markdown`] to keep it in memory.
pub async fn summarize_markdown_file(markdown: impl AsRef<Path>) -> Result<PathBuf, Error> {
    let markdown = markdown.as_ref();
    let summary = default_output(markdown);
    Options::new().summarize_to_file(markdown, &summary).await?;
    Ok(summary)
}

/// Summarize `markdown` and return the summary.
pub async fn summarize_markdown(markdown: impl AsRef<Path>) -> Result<String, Error> {
    Ok(Options::new().summarize(markdown).await?.value)
}

/// Where [`summarize_markdown_file`] writes: the input's name, plus
/// `_summary.md`.
///
/// ```
/// # use std::path::Path;
/// assert_eq!(agent::default_output("notes/book.md"), Path::new("notes/book_summary.md"));
/// ```
pub fn default_output(markdown: impl AsRef<Path>) -> PathBuf {
    let markdown = markdown.as_ref();
    let stem = markdown
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        // A path that is all parent and no file — `.` or `/` — has no stem to
        // build on, so the summary is named for nothing in particular rather
        // than silently landing on top of something.
        .unwrap_or_else(|| "summary".to_string());
    markdown.with_file_name(format!("{stem}_summary.md"))
}

impl Options {
    /// Summarize the document at `markdown` and return the summary with what
    /// it took.
    pub async fn summarize(&self, markdown: impl AsRef<Path>) -> Result<Reply<String>, Error> {
        let path = markdown.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| Error::io("read", path, e))?;
        summarize::summarize(&text, path, self).await
    }

    /// Summarize `text`, calling it `label` in any refusal.
    pub async fn summarize_text(
        &self,
        text: &str,
        label: impl AsRef<Path>,
    ) -> Result<Reply<String>, Error> {
        summarize::summarize(text, label.as_ref(), self).await
    }

    /// Summarize `markdown` and write the summary to `summary`.
    ///
    /// Refuses to write over the source document.
    pub async fn summarize_to_file(
        &self,
        markdown: impl AsRef<Path>,
        summary: impl AsRef<Path>,
    ) -> Result<Reply<String>, Error> {
        let (markdown, destination) = (markdown.as_ref(), summary.as_ref());
        if same_file(markdown, destination) {
            return Err(Error::WouldOverwriteInput(markdown.to_path_buf()));
        }
        let summary = self.summarize(markdown).await?;
        std::fs::write(destination, &summary.value)
            .map_err(|e| Error::io("write", destination, e))?;
        Ok(summary)
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
    fn the_default_output_appends_summary() {
        assert_eq!(default_output("book.md"), Path::new("book_summary.md"));
        assert_eq!(
            default_output("a/b/book.md"),
            Path::new("a/b/book_summary.md")
        );
        // Any extension, not just .md, and the result is always .md.
        assert_eq!(default_output("notes.txt"), Path::new("notes_summary.md"));
        assert_eq!(default_output("README"), Path::new("README_summary.md"));
    }

    #[test]
    fn a_pathological_path_still_names_something() {
        // "." has no file stem to build on. The result must still be a real,
        // relative file name rather than an attempt to write to the directory.
        let output = default_output(".");
        assert!(
            output.ends_with("summary_summary.md"),
            "{}",
            output.display()
        );
        assert!(output.file_name().is_some());
    }

    #[tokio::test]
    async fn summarizing_onto_the_input_is_refused() {
        // Neither path exists, so this exercises the textual fallback — which
        // is the case that matters, since the output usually does not exist.
        let error = Options::new()
            .summarize_to_file("book.md", "book.md")
            .await
            .expect_err("refused");
        assert!(matches!(error, Error::WouldOverwriteInput(_)), "{error:?}");
    }

    #[tokio::test]
    async fn a_missing_input_is_an_io_error() {
        let error = Options::new()
            .summarize("nope/missing.md")
            .await
            .expect_err("failed");
        assert!(matches!(error, Error::Io { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn every_role_checks_for_a_key_before_sending() {
        // With no key in the environment nothing may go out. Each role has to
        // fail the same way, before any request, and without touching the
        // network — which is also what makes this test safe to run anywhere.
        // SAFETY: tests in this crate never read the variable concurrently
        // except through this function, and the value is restored before it
        // returns.
        let saved = std::env::var("OPENAI_API_KEY").ok();
        unsafe { std::env::remove_var("OPENAI_API_KEY") };
        let options = Options::new().provider(Provider::OpenAi);
        let ask = options.ask("q", "doc", "doc.md").await;
        let relevant = options.relevant("q", "summary").await;
        let same = options.same_point("a", "b").await;
        let merge = options.merge_points(&["a".to_string(), "b".to_string()]).await;
        let answer = options.answer("q", "- p [x]").await;
        let summary = options.summarize_text("doc", "doc.md").await;
        if let Some(key) = saved {
            unsafe { std::env::set_var("OPENAI_API_KEY", key) };
        }
        for outcome in [
            ask.map(|_| ()),
            relevant.map(|_| ()),
            same.map(|_| ()),
            merge.map(|_| ()),
            answer.map(|_| ()),
            summary.map(|_| ()),
        ] {
            let error = outcome.expect_err("no key");
            assert!(matches!(error, Error::NoApiKey { .. }), "{error:?}");
        }
    }
}
