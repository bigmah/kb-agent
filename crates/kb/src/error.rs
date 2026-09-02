//! What can go wrong, as something a caller can match on.

use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A file or directory could not be read, written or walked.
    Io {
        /// What was being attempted, e.g. `"read"`.
        action: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },

    /// The root is not a directory, or holds nothing this crate recognizes.
    NoSources(PathBuf),

    /// A request in one of the roles failed. `source` names the document
    /// being read where there was one.
    Agent {
        source: agent::Error,
        about: String,
    },

    /// Nothing in the knowledge base has a summary yet, so there is nothing
    /// to judge a question against. Build it first.
    NotBuilt(PathBuf),

    /// The options do not describe a run that can be made.
    Options(String),
}

impl Error {
    pub(crate) fn io(action: &'static str, path: impl AsRef<Path>, source: std::io::Error) -> Self {
        Self::Io {
            action,
            path: path.as_ref().to_path_buf(),
            source,
        }
    }

    pub(crate) fn agent(about: impl Into<String>) -> impl FnOnce(agent::Error) -> Self {
        let about = about.into();
        move |source| Self::Agent { source, about }
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
            Self::NoSources(root) => write!(
                f,
                "{} holds no PDFs and no Markdown — nothing to index",
                root.display()
            ),
            Self::Agent { source, about } => write!(f, "{about}: {source}"),
            Self::NotBuilt(root) => write!(
                f,
                "nothing in {} has been summarized yet — build the knowledge base first",
                root.display()
            ),
            Self::Options(what) => f.write_str(what),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Agent { source, .. } => Some(source),
            _ => None,
        }
    }
}
