//! The index: every document in the directory, by path, with whatever has
//! been made from it so far.
//!
//! Nothing is stored but the files. A source is a name — its path under the
//! root, without the extension — and the three files that may exist for it:
//!
//! | File | Made by |
//! | --- | --- |
//! | `name.pdf` | whoever put it there |
//! | `name.md` | converting the PDF, or whoever put it there |
//! | `name_summary.md` | summarizing the Markdown |
//!
//! A scan of the directory is the whole index, so there is no database to
//! fall out of step with the files, a build that stops half-way has lost
//! nothing, and adding a document is copying a file in.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::Error;
use crate::report::count;

/// The suffix that marks a Markdown file as a summary of the one beside it.
/// Matches [`agent::default_output`].
const SUMMARY_SUFFIX: &str = "_summary";

/// One document, and what exists of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// The path under the root without its extension, with `/` separators
    /// whatever the platform: `finance/johnson-algorithmic-trading`. Unique
    /// within the corpus, and how everything else refers to the document.
    pub name: String,
    pub pdf: Option<PathBuf>,
    pub markdown: Option<PathBuf>,
    pub summary: Option<PathBuf>,
}

impl Source {
    fn empty(name: String) -> Self {
        Self {
            name,
            pdf: None,
            markdown: None,
            summary: None,
        }
    }

    /// Whether the document can be read in full — there is Markdown for it.
    pub fn is_readable(&self) -> bool {
        self.markdown.is_some()
    }

    /// Whether the document can be judged for relevance — there is a summary
    /// for it.
    pub fn is_summarized(&self) -> bool {
        self.summary.is_some()
    }

    /// Where the Markdown for this source goes, whether or not it exists.
    pub(crate) fn markdown_path(&self, root: &Path) -> PathBuf {
        root.join(format!("{}.md", self.name))
    }

    /// Where the summary for this source goes, whether or not it exists.
    pub(crate) fn summary_path(&self, root: &Path) -> PathBuf {
        root.join(format!("{}{SUMMARY_SUFFIX}.md", self.name))
    }
}

/// A directory of documents, indexed.
#[derive(Clone, Debug)]
pub struct Corpus {
    root: PathBuf,
    sources: BTreeMap<String, Source>,
}

impl Corpus {
    /// Index everything under `root`.
    ///
    /// Walks the directory once. Hidden files and directories are skipped,
    /// so a query's own output can live under `.kb-agent/` without becoming
    /// part of the library it was asked about.
    pub fn scan(root: impl AsRef<Path>) -> Result<Self, Error> {
        let root = root.as_ref();
        if !root.is_dir() {
            return Err(Error::io(
                "read directory",
                root,
                std::io::Error::new(std::io::ErrorKind::NotADirectory, "not a directory"),
            ));
        }
        let mut sources: BTreeMap<String, Source> = BTreeMap::new();

        let walk = WalkDir::new(root)
            .follow_links(true)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| entry.depth() == 0 || !is_hidden(entry.file_name()));
        for entry in walk {
            let entry = entry.map_err(|error| {
                let path = error
                    .path()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| root.to_path_buf());
                Error::io("walk", path, error.into())
            })?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some((name, kind)) = classify(root, path) else {
                continue;
            };
            let source = sources
                .entry(name.clone())
                .or_insert_with(|| Source::empty(name));
            let slot = match kind {
                Kind::Pdf => &mut source.pdf,
                Kind::Markdown => &mut source.markdown,
                Kind::Summary => &mut source.summary,
            };
            // `book.pdf` and `book.PDF` in one directory: the first wins, and
            // the sort above makes "first" deterministic.
            slot.get_or_insert_with(|| path.to_path_buf());
        }

        Ok(Self {
            root: root.to_path_buf(),
            sources,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every source, in name order.
    pub fn sources(&self) -> impl Iterator<Item = &Source> {
        self.sources.values()
    }

    pub fn get(&self, name: &str) -> Option<&Source> {
        self.sources.get(name)
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub(crate) fn source_mut(&mut self, name: &str) -> Option<&mut Source> {
        self.sources.get_mut(name)
    }

    /// How much of the library is built.
    pub fn status(&self) -> Status {
        let mut status = Status::default();
        for source in self.sources() {
            status.sources += 1;
            if source.pdf.is_some() {
                status.pdfs += 1;
                if source.markdown.is_none() {
                    status.unconverted += 1;
                }
            }
            match (&source.markdown, &source.summary) {
                (Some(_), Some(_)) => status.summarized += 1,
                (Some(_), None) => status.unsummarized += 1,
                (None, Some(_)) => status.orphan_summaries += 1,
                (None, None) => {}
            }
        }
        status
    }
}

/// What exists for how many sources, from [`Corpus::status`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Status {
    pub sources: usize,
    pub pdfs: usize,
    /// PDFs with no Markdown beside them yet.
    pub unconverted: usize,
    /// Sources with Markdown and a summary: the ones a question can reach.
    pub summarized: usize,
    /// Sources with Markdown and no summary yet.
    pub unsummarized: usize,
    /// Summaries whose Markdown is gone. They can be judged relevant but not
    /// read, so a query leaves them out.
    pub orphan_summaries: usize,
}

impl Status {
    /// One line, ready to print.
    pub fn describe(&self) -> String {
        if self.sources == 0 {
            return "no sources".to_string();
        }
        let mut parts = vec![format!(
            "{} ready to query",
            count(self.summarized, "source")
        )];
        if self.unconverted > 0 {
            parts.push(format!("{} to convert", count(self.unconverted, "PDF")));
        }
        if self.unsummarized > 0 {
            parts.push(format!(
                "{} to summarize",
                count(self.unsummarized, "document")
            ));
        }
        if self.orphan_summaries > 0 {
            parts.push(format!(
                "{} without its document",
                count(self.orphan_summaries, "summary")
            ));
        }
        format!("{}: {}", count(self.sources, "source"), parts.join(", "))
    }
}

enum Kind {
    Pdf,
    Markdown,
    Summary,
}

/// The source name and what kind of file this is, or `None` for a file this
/// crate has no use for.
fn classify(root: &Path, path: &Path) -> Option<(String, Kind)> {
    let relative = path.strip_prefix(root).ok()?;
    let extension = relative.extension()?.to_str()?.to_ascii_lowercase();
    let stem = relative.file_stem()?.to_str()?;
    let (stem, kind) = match extension.as_str() {
        "pdf" => (stem, Kind::Pdf),
        "md" => match stem.strip_suffix(SUMMARY_SUFFIX) {
            Some(base) if !base.is_empty() => (base, Kind::Summary),
            _ => (stem, Kind::Markdown),
        },
        _ => return None,
    };
    let mut name = relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| {
            parent
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
                + "/"
        })
        .unwrap_or_default();
    name.push_str(stem);
    Some((name, kind))
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| name.starts_with('.'))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fresh directory under the system temp dir, removed on drop.
    pub(crate) struct Scratch(pub PathBuf);

    impl Scratch {
        pub(crate) fn new(label: &str) -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "kb-test-{label}-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub(crate) fn write(&self, relative: &str, text: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_directory_scans_into_sources_by_name() {
        let dir = Scratch::new("scan");
        dir.write("book.pdf", "%PDF");
        dir.write("book.md", "# Book");
        dir.write("book_summary.md", "A book.");
        dir.write("papers/one.md", "# One");
        dir.write("papers/two.PDF", "%PDF");
        dir.write("papers/notes.txt", "ignored");
        dir.write(".kb-agent/queries/x/points.md", "- hidden");
        dir.write("orphan_summary.md", "no document");

        let corpus = Corpus::scan(&dir.0).unwrap();
        let names: Vec<_> = corpus.sources().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["book", "orphan", "papers/one", "papers/two"]);

        let book = corpus.get("book").unwrap();
        assert!(book.pdf.is_some() && book.markdown.is_some() && book.summary.is_some());
        let one = corpus.get("papers/one").unwrap();
        assert!(one.pdf.is_none() && one.is_readable() && !one.is_summarized());
        let two = corpus.get("papers/two").unwrap();
        assert!(two.pdf.is_some() && !two.is_readable());
        let orphan = corpus.get("orphan").unwrap();
        assert!(!orphan.is_readable() && orphan.is_summarized());

        let status = corpus.status();
        assert_eq!(
            status,
            Status {
                sources: 4,
                pdfs: 2,
                unconverted: 1,
                summarized: 1,
                unsummarized: 1,
                orphan_summaries: 1,
            }
        );
        assert_eq!(
            status.describe(),
            "4 sources: 1 source ready to query, 1 PDF to convert, 1 document to summarize, \
             1 summary without its document"
        );
    }

    #[test]
    fn output_paths_sit_beside_the_source() {
        let source = Source::empty("papers/one".to_string());
        assert_eq!(
            source.markdown_path(Path::new("/lib")),
            Path::new("/lib/papers/one.md")
        );
        assert_eq!(
            source.summary_path(Path::new("/lib")),
            Path::new("/lib/papers/one_summary.md")
        );
    }

    #[test]
    fn a_file_named_only_summary_is_a_document_not_a_summary_of_nothing() {
        let root = Path::new("/lib");
        assert!(matches!(
            classify(root, Path::new("/lib/_summary.md")),
            Some((name, Kind::Markdown)) if name == "_summary"
        ));
    }

    #[test]
    fn an_empty_directory_is_an_empty_corpus() {
        let dir = Scratch::new("empty");
        let corpus = Corpus::scan(&dir.0).unwrap();
        assert!(corpus.is_empty());
        assert_eq!(corpus.status().describe(), "no sources");
    }

    #[test]
    fn a_missing_root_is_an_error() {
        assert!(Corpus::scan("/definitely/not/here").is_err());
    }
}
