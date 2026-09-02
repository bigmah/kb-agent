//! What can go wrong, as something a caller can match on.
//!
//! The CLI used to carry every failure as a `String`, which is fine when the
//! only consumer prints it. A library has callers that need to *decide* —
//! prompt for a password, fall back to the text layer, retry without OCR — so
//! each of those cases is a variant rather than a sentence to grep.

use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A file could not be read or written.
    Io {
        /// What was being attempted, e.g. `"read"`.
        action: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },

    /// The PDF is encrypted and [`Options::password`] was absent or wrong.
    ///
    /// [`Options::password`]: crate::Options::password
    Encrypted,

    /// The extraction pipeline failed. `hint`, when present, is an actionable
    /// next step — a missing shared library, a model set that cannot be
    /// downloaded — worth showing alongside the message.
    Pdf {
        message: String,
        hint: Option<String>,
    },

    /// The document produced no text at all: every page was empty, or it was
    /// entirely scanned and OCR was [`Ocr::Off`].
    ///
    /// [`Ocr::Off`]: crate::Ocr::Off
    NoText,

    /// An OCR worker process failed. The string is the worker's own last words,
    /// which say more than its exit status does.
    Worker(String),

    /// The options do not describe a conversion that can be run.
    Options(String),

    /// The destination would have overwritten the source PDF.
    WouldOverwriteInput(PathBuf),
}

impl Error {
    pub(crate) fn io(action: &'static str, path: impl AsRef<Path>, source: std::io::Error) -> Self {
        Self::Io {
            action,
            path: path.as_ref().to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                action,
                path,
                source,
            } => write!(f, "could not {action} {}: {source}", path.display()),
            Self::Encrypted => f.write_str("the PDF is encrypted — a password is required"),
            Self::Pdf { message, hint } => match hint {
                Some(hint) => write!(f, "{message}\n       {hint}"),
                None => f.write_str(message),
            },
            Self::NoText => f.write_str("no text was recovered from this PDF"),
            Self::Worker(what) => write!(f, "an OCR worker failed: {what}"),
            Self::Options(what) => f.write_str(what),
            Self::WouldOverwriteInput(path) => {
                write!(f, "refusing to overwrite the input {}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Flatten an error and everything it wraps into one line.
///
/// Wrappers that re-`Display` their source would otherwise repeat the same
/// sentence at every level, so only genuinely new text is appended.
pub(crate) fn error_chain(error: &dyn std::error::Error) -> String {
    use std::fmt::Write as _;

    let mut out = error.to_string();
    let mut source = error.source();
    while let Some(inner) = source {
        let message = inner.to_string();
        if !out.contains(&message) {
            let _ = write!(out, ": {message}");
        }
        source = inner.source();
    }
    out
}
