//! The whole API again, synchronously, for a program that is not built around
//! an async runtime.
//!
//! Each call owns a single-threaded runtime for its own duration. That is the
//! right trade for a command-line tool — one summary, then exit — and the wrong
//! one for a server, which should use the async API and its existing runtime.
//!
//! # Do not call these from inside a runtime
//!
//! Starting a runtime inside a runtime panics. If you are already in `async`
//! code, use [`Options::summarize`](crate::Options::summarize) and friends
//! directly; that is what these wrap.

use std::path::{Path, PathBuf};

use crate::{Error, Options, Summary};

/// Blocking [`crate::summarize_markdown_file`].
pub fn summarize_markdown_file(markdown: impl AsRef<Path>) -> Result<PathBuf, Error> {
    on_a_runtime(crate::summarize_markdown_file(markdown))
}

/// Blocking [`crate::summarize_markdown`].
pub fn summarize_markdown(markdown: impl AsRef<Path>) -> Result<String, Error> {
    on_a_runtime(crate::summarize_markdown(markdown))
}

impl Options {
    /// Blocking [`Options::summarize`](crate::Options::summarize).
    pub fn summarize_blocking(&self, markdown: impl AsRef<Path>) -> Result<Summary, Error> {
        on_a_runtime(self.summarize(markdown))
    }

    /// Blocking [`Options::summarize_to_file`](crate::Options::summarize_to_file).
    pub fn summarize_to_file_blocking(
        &self,
        markdown: impl AsRef<Path>,
        summary: impl AsRef<Path>,
    ) -> Result<Summary, Error> {
        on_a_runtime(self.summarize_to_file(markdown, summary))
    }
}

/// Run one future to completion on a runtime built for it.
///
/// Current-thread rather than multi-threaded: the work is one document's worth
/// of HTTP, so there is nothing for extra worker threads to do, and a
/// command-line tool should not stand up a thread pool to make a few requests.
fn on_a_runtime<T>(future: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::io("start a runtime for", Path::new("<tokio>"), error))?
        .block_on(future)
}
