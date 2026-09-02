//! What can go wrong, as something a caller can match on.

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

    /// No credentials: the provider's key variable is unset or empty.
    ///
    /// Kept separate from [`Error::Provider`] because it is the one failure
    /// that is always the operator's to fix, and it costs nothing to detect —
    /// it is caught before a single request is sent.
    NoApiKey {
        provider: crate::Provider,
        /// The environment variable that was consulted.
        variable: &'static str,
    },

    /// The provider rejected the request or could not be reached. The string is
    /// rig's own explanation, which names the HTTP status where there was one.
    Provider(String),

    /// The document held nothing to summarize.
    Empty(PathBuf),

    /// The document is bigger than one request may carry.
    ///
    /// Never split and never truncated: a summary of part of a document is
    /// indistinguishable from a summary of the whole one once it is written
    /// down, so the size is reported back to whatever produced the document.
    TooLarge {
        path: PathBuf,
        /// Estimated tokens in the document — see
        /// [`CHARS_PER_TOKEN`](crate::CHARS_PER_TOKEN) for why it is an
        /// estimate.
        estimated_tokens: usize,
        /// The budget it exceeded: [`Options::context_tokens`](crate::Options::context_tokens).
        context_tokens: usize,
    },

    /// The model returned a response with no text in it. Not the same as a
    /// refusal, which arrives as text saying so.
    NoContent,

    /// The options do not describe a summary that can be produced.
    Options(String),

    /// The destination would have overwritten the source document.
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
            Self::NoApiKey { provider, variable } => write!(
                f,
                "no API key — set {variable} to {} API key",
                match provider.name() {
                    name @ ("OpenAI" | "Anthropic") => format!("an {name}"),
                    name => format!("a {name}"),
                }
            ),
            Self::Provider(what) => f.write_str(what),
            Self::Empty(path) => {
                write!(f, "{} has nothing to summarize", path.display())
            }
            Self::TooLarge {
                path,
                estimated_tokens,
                context_tokens,
            } => write!(
                f,
                "{} is ~{estimated_tokens} tokens, over the {context_tokens}-token budget \
                 for one request — summarize a smaller document, or raise context_tokens \
                 if the model's window has room for it",
                path.display()
            ),
            Self::NoContent => f.write_str("the model returned no text"),
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

impl From<rig_core::completion::CompletionError> for Error {
    fn from(error: rig_core::completion::CompletionError) -> Self {
        Self::Provider(error.to_string())
    }
}
