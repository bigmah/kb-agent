//! Building the library: PDFs into Markdown, Markdown into summaries.
//!
//! Both steps are idempotent over the files. A source that already has its
//! output is skipped, so a build that was interrupted — or one run after ten
//! more books were dropped in — does only the work that is new. `force`
//! redoes everything.
//!
//! Conversion runs one PDF at a time: the extractor fans OCR out across
//! processes on its own, and two of those fan-outs at once would just fight
//! for the same cores. Summarizing runs many at a time, because it is all
//! waiting on the network.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use agent::Usage;

use crate::report::{Progress, Stage, count, format_duration};
use crate::{Corpus, Error, fanout};

/// How to convert, from [`Corpus::convert`].
#[derive(Clone)]
pub struct ConvertOptions {
    pub(crate) extractor: pdf_extractor::Options,
    pub(crate) force: bool,
    pub(crate) progress: Option<Arc<dyn Fn(Progress) + Send + Sync>>,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            extractor: pdf_extractor::Options::new(),
            force: false,
            progress: None,
        }
    }
}

impl ConvertOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// How each PDF is converted — page selection, OCR settings, and the
    /// extractor's own progress callback for OCR pages.
    pub fn extractor(mut self, options: pdf_extractor::Options) -> Self {
        self.extractor = options;
        self
    }

    /// Convert every PDF, including ones that already have Markdown.
    pub fn force(mut self, yes: bool) -> Self {
        self.force = yes;
        self
    }

    pub fn progress(mut self, on_progress: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        self.progress = Some(Arc::new(on_progress));
        self
    }

    fn emit(&self, event: Progress) {
        if let Some(callback) = &self.progress {
            callback(event);
        }
    }
}

/// What a conversion did, from [`Corpus::convert`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ConvertReport {
    /// Sources converted this run.
    pub converted: Vec<String>,
    /// Sources that already had Markdown and were left alone.
    pub skipped: usize,
    /// Sources whose PDF could not be converted, with the extractor's reason.
    /// The rest of the build carries on without them.
    pub failed: Vec<(String, String)>,
    pub elapsed_ms: u64,
}

impl ConvertReport {
    /// One line, ready to print.
    pub fn describe(&self) -> String {
        let mut parts = vec![format!("{} converted", count(self.converted.len(), "PDF"))];
        if self.skipped > 0 {
            parts.push(format!("{} already had Markdown", self.skipped));
        }
        if !self.failed.is_empty() {
            parts.push(format!("{} failed", self.failed.len()));
        }
        format!(
            "convert: {}, {}",
            parts.join(", "),
            format_duration(self.elapsed_ms)
        )
    }
}

impl Corpus {
    /// Convert every PDF that has no Markdown yet, and index what that made.
    ///
    /// Synchronous and CPU-bound; see the module docs for why it is not
    /// parallel at this level.
    pub fn convert(&mut self, options: &ConvertOptions) -> Result<ConvertReport, Error> {
        let started = Instant::now();
        let mut report = ConvertReport::default();

        let pending: Vec<(String, PathBuf, PathBuf)> = self
            .sources()
            .filter_map(|source| {
                let pdf = source.pdf.clone()?;
                if source.markdown.is_some() && !options.force {
                    report.skipped += 1;
                    return None;
                }
                Some((source.name.clone(), pdf, source.markdown_path(self.root())))
            })
            .collect();

        let total = pending.len();
        for (done, (name, pdf, markdown)) in pending.into_iter().enumerate() {
            options.emit(Progress::Converting {
                name: name.clone(),
                done,
                total,
            });
            match options.extractor.convert_to_file(&pdf, &markdown) {
                Ok(_) => {
                    if let Some(source) = self.source_mut(&name) {
                        source.markdown = Some(markdown);
                    }
                    report.converted.push(name);
                }
                Err(error) => report.failed.push((name, error.to_string())),
            }
        }

        report.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(report)
    }
}

/// How to summarize, from [`Corpus::summarize`].
#[derive(Clone)]
pub struct SummarizeOptions {
    pub(crate) agent: agent::Options,
    pub(crate) concurrency: usize,
    pub(crate) force: bool,
    pub(crate) progress: Option<Arc<dyn Fn(Progress) + Send + Sync>>,
}

/// Requests in flight at once, unless the caller says otherwise. Enough to
/// keep a provider busy; few enough that its rate limit is a retry rather
/// than a wall.
pub const DEFAULT_CONCURRENCY: usize = 8;

impl Default for SummarizeOptions {
    fn default() -> Self {
        Self {
            agent: agent::Options::new(),
            concurrency: DEFAULT_CONCURRENCY,
            force: false,
            progress: None,
        }
    }
}

impl SummarizeOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Provider, model, budget and the rest — see [`agent::Options`]. The
    /// context budget decides which documents fit; those that do not are
    /// left unsummarized and named in the report.
    pub fn agent(mut self, options: agent::Options) -> Self {
        self.agent = options;
        self
    }

    /// Summaries in flight at once. Defaults to [`DEFAULT_CONCURRENCY`].
    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    /// Summarize every document, including ones that already have a summary.
    pub fn force(mut self, yes: bool) -> Self {
        self.force = yes;
        self
    }

    pub fn progress(mut self, on_progress: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        self.progress = Some(Arc::new(on_progress));
        self
    }

    fn emit(&self, event: Progress) {
        if let Some(callback) = &self.progress {
            callback(event);
        }
    }
}

/// What summarizing did, from [`Corpus::summarize`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct SummarizeReport {
    /// Sources summarized this run.
    pub summarized: Vec<String>,
    /// Sources that already had a summary and were left alone.
    pub skipped: usize,
    /// Sources over the context budget, with their estimated size in tokens.
    /// They stay in the library but a question cannot reach them until they
    /// fit — a bigger budget, or a smaller document.
    pub too_large: Vec<(String, usize)>,
    /// Sources whose Markdown was empty.
    pub empty: Vec<String>,
    /// Sources whose request failed after retries, with the reason. The rest
    /// of the run carried on without them; running the build again picks
    /// them up.
    pub failed: Vec<(String, String)>,
    pub stage: Stage,
}

impl SummarizeReport {
    /// One line, ready to print.
    pub fn describe(&self) -> String {
        let mut parts = vec![format!(
            "{} summarized",
            count(self.summarized.len(), "document")
        )];
        if self.skipped > 0 {
            parts.push(format!("{} already had one", self.skipped));
        }
        if !self.too_large.is_empty() {
            parts.push(format!("{} too large", self.too_large.len()));
        }
        if !self.empty.is_empty() {
            parts.push(format!("{} empty", self.empty.len()));
        }
        if !self.failed.is_empty() {
            parts.push(format!("{} failed", self.failed.len()));
        }
        format!("summarize: {} — {}", parts.join(", "), self.stage.describe())
    }
}

enum Outcome {
    Done(PathBuf, Usage),
    TooLarge(usize),
    Empty,
    Failed(String),
}

impl Corpus {
    /// Summarize every document that has no summary yet, and index what that
    /// made.
    ///
    /// Documents over the budget are left out and named in the report, as
    /// are ones whose request failed; a missing API key stops the run before
    /// anything is sent.
    pub async fn summarize(
        &mut self,
        options: &SummarizeOptions,
    ) -> Result<SummarizeReport, Error> {
        let started = Instant::now();
        let mut report = SummarizeReport::default();

        let pending: Vec<(String, PathBuf, PathBuf)> = self
            .sources()
            .filter_map(|source| {
                let markdown = source.markdown.clone()?;
                if source.summary.is_some() && !options.force {
                    report.skipped += 1;
                    return None;
                }
                Some((source.name.clone(), markdown, source.summary_path(self.root())))
            })
            .collect();
        if pending.is_empty() && report.skipped == 0 {
            return Err(Error::NoSources(self.root().to_path_buf()));
        }

        let outcomes = fanout::fan_out(
            pending,
            options.concurrency,
            |done, total| options.emit(Progress::Summarizing { done, total }),
            |(name, markdown, summary)| async move {
                let outcome = match options.agent.plan(&markdown) {
                    Err(error) => Outcome::Failed(error.to_string()),
                    Ok(plan) if plan.characters == 0 => Outcome::Empty,
                    Ok(plan) if !plan.fits => Outcome::TooLarge(plan.estimated_tokens),
                    Ok(_) => match options.agent.summarize_to_file(&markdown, &summary).await {
                        Ok(reply) => Outcome::Done(summary, reply.usage),
                        // Every other request would fail the same way; say so
                        // once instead of once per document.
                        Err(error @ agent::Error::NoApiKey { .. }) => {
                            return Err(Error::agent(format!("summarizing {name}"))(error));
                        }
                        Err(error) => Outcome::Failed(error.to_string()),
                    },
                };
                Ok((name, outcome))
            },
        )
        .await?;

        let mut usage = Usage::default();
        for (name, outcome) in outcomes {
            match outcome {
                Outcome::Done(path, used) => {
                    usage += used;
                    if let Some(source) = self.source_mut(&name) {
                        source.summary = Some(path);
                    }
                    report.summarized.push(name);
                }
                Outcome::TooLarge(tokens) => report.too_large.push((name, tokens)),
                Outcome::Empty => report.empty.push(name),
                Outcome::Failed(why) => report.failed.push((name, why)),
            }
        }
        report.stage = Stage::from(started, usage);
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::tests::Scratch;

    #[test]
    fn a_report_names_what_did_not_happen_and_why() {
        let report = SummarizeReport {
            summarized: vec!["a".into()],
            skipped: 2,
            too_large: vec![("b".into(), 900_000)],
            empty: vec![],
            failed: vec![("c".into(), "429".into())],
            stage: Stage::default(),
        };
        let line = report.describe();
        assert!(line.starts_with("summarize: 1 document summarized, 2 already had one, 1 too large, 1 failed"), "{line}");
    }

    #[test]
    fn converting_skips_sources_that_already_have_markdown() {
        let dir = Scratch::new("convert");
        dir.write("done.pdf", "%PDF");
        dir.write("done.md", "# Done");
        let mut corpus = Corpus::scan(&dir.0).unwrap();
        let report = corpus.convert(&ConvertOptions::new()).unwrap();
        assert_eq!(report.skipped, 1);
        assert!(report.converted.is_empty());
        assert!(report.failed.is_empty());
    }

    #[test]
    fn a_pdf_that_cannot_be_converted_is_reported_not_fatal() {
        let dir = Scratch::new("badpdf");
        dir.write("broken.pdf", "this is not a pdf");
        let mut corpus = Corpus::scan(&dir.0).unwrap();
        let report = corpus.convert(&ConvertOptions::new()).unwrap();
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert_eq!(report.failed[0].0, "broken");
        assert!(!corpus.get("broken").unwrap().is_readable());
        assert!(report.describe().contains("1 failed"));
    }

    #[tokio::test]
    async fn summarizing_with_nothing_to_do_and_nothing_done_is_an_error() {
        let dir = Scratch::new("nothing");
        dir.write("only.pdf", "%PDF");
        let mut corpus = Corpus::scan(&dir.0).unwrap();
        let error = corpus
            .summarize(&SummarizeOptions::new())
            .await
            .expect_err("nothing to summarize");
        assert!(matches!(error, Error::NoSources(_)), "{error:?}");
    }

    #[tokio::test]
    async fn summarizing_sorts_documents_before_sending_any() {
        // No key, so a request that got as far as the network would fail with
        // NoApiKey. The oversized and empty documents must be sorted out
        // before that point and reported, and the one that would be sent must
        // be the one that stops the run.
        let dir = Scratch::new("sort");
        dir.write("big.md", &"word ".repeat(2_000));
        dir.write("empty.md", "  \n");
        let saved = std::env::var("OPENAI_API_KEY").ok();
        unsafe { std::env::remove_var("OPENAI_API_KEY") };
        let mut corpus = Corpus::scan(&dir.0).unwrap();
        let options = SummarizeOptions::new()
            .agent(agent::Options::new().context_tokens(100))
            .concurrency(1);
        let report = corpus.summarize(&options).await;
        if let Some(key) = saved {
            unsafe { std::env::set_var("OPENAI_API_KEY", key) };
        }
        let report = report.expect("nothing was sent");
        assert_eq!(report.too_large.len(), 1, "{report:?}");
        assert_eq!(report.too_large[0].0, "big");
        assert_eq!(report.empty, ["empty"]);
        assert!(report.summarized.is_empty());
    }
}
