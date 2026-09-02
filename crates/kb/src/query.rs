//! A question, put to the whole library.
//!
//! Four stages, each a fan-out of fresh-context requests, each public so a
//! caller can keep what a stage produced before paying for the next:
//!
//! 1. [`Corpus::mask`] — every summary is judged for relevance to the
//!    question, one request each. The result is a mask over the library.
//! 2. [`Corpus::ask`] — every source the mask let through is read in full
//!    and asked the question, one request each. The result is one long list
//!    of points, each naming the source it came from.
//! 3. [`reduce`] — every pair of points is compared and the groups judged the
//!    same are merged. The result is the refined list.
//! 4. [`answer`] — the question is answered from the refined list, in one
//!    request, with sources named.
//!
//! [`Corpus::query`] runs the four in a row and returns a [`Distillation`]
//! carrying all of it. The refined list is the product as much as the answer
//! is: it is what a library of a few hundred books had to say about the
//! question, in a form that fits in someone else's context.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use agent::Usage;

use crate::build::DEFAULT_CONCURRENCY;
use crate::report::{Progress, Stage, count};
use crate::{Corpus, Error, fanout};

pub use crate::reduce::{pairs_for, reduce};

/// How a question is run, for every stage.
#[derive(Clone)]
pub struct QueryOptions {
    pub(crate) agent: agent::Options,
    pub(crate) concurrency: usize,
    pub(crate) reduce: bool,
    pub(crate) answer: bool,
    pub(crate) progress: Option<Arc<dyn Fn(Progress) + Send + Sync>>,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            agent: agent::Options::new(),
            concurrency: DEFAULT_CONCURRENCY,
            reduce: true,
            answer: true,
            progress: None,
        }
    }
}

impl QueryOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Provider, model, budget and the rest — see [`agent::Options`]. One
    /// configuration drives every role; the context budget decides which
    /// documents can be read at all.
    pub fn agent(mut self, options: agent::Options) -> Self {
        self.agent = options;
        self
    }

    /// Requests in flight at once. Defaults to [`DEFAULT_CONCURRENCY`].
    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    /// Whether [`Corpus::query`] runs the reduction. On by default; off, the
    /// answer is written from the raw list, which is cheaper by the square of
    /// its length and worse by however much the library repeats itself.
    pub fn reduce(mut self, yes: bool) -> Self {
        self.reduce = yes;
        self
    }

    /// Whether [`Corpus::query`] writes an answer. On by default; off, the
    /// run stops at the list, for a caller who wants the list itself — to
    /// carry into another context, say — and not this crate's reading of it.
    pub fn answer(mut self, yes: bool) -> Self {
        self.answer = yes;
        self
    }

    pub fn progress(mut self, on_progress: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        self.progress = Some(Arc::new(on_progress));
        self
    }

    pub(crate) fn emit(&self, event: Progress) {
        if let Some(callback) = &self.progress {
            callback(event);
        }
    }
}

/// One thing the library says, and who says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Point {
    /// Self-contained: it makes sense without the others and without the
    /// document.
    pub text: String,
    /// Source names — see [`Source::name`](crate::Source::name) — that this
    /// point came from. One for a point straight from the reading; several
    /// once points have been merged.
    pub sources: Vec<String>,
}

impl Point {
    /// The list as Markdown, one bullet per point with its sources in
    /// brackets on the end: the form [`answer`] reads.
    pub fn render_list(points: &[Point]) -> String {
        let mut out = String::new();
        for point in points {
            out.push_str("- ");
            out.push_str(&point.text);
            out.push_str(" [");
            out.push_str(&point.sources.join("; "));
            out.push_str("]\n");
        }
        out
    }
}

/// A source a question could not reach, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Excluded {
    pub name: String,
    pub why: String,
}

/// Which sources a question will be judged against, from
/// [`Corpus::plan_query`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct QueryPlan {
    /// Sources with a summary and a document that fits the budget: the ones
    /// the mask will judge.
    pub reachable: Vec<String>,
    pub excluded: Vec<Excluded>,
}

impl QueryPlan {
    /// One line, ready to print.
    pub fn describe(&self) -> String {
        let mut line = format!("{} to judge", count(self.reachable.len(), "source"));
        if !self.excluded.is_empty() {
            line.push_str(&format!(", {} excluded", self.excluded.len()));
        }
        line
    }
}

/// The relevance mask, from [`Corpus::mask`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Mask {
    pub question: String,
    /// Every source judged, with its verdict, in name order.
    pub judged: Vec<(String, bool)>,
    pub excluded: Vec<Excluded>,
    pub stage: Stage,
}

impl Mask {
    /// The sources let through, in name order.
    pub fn relevant(&self) -> impl Iterator<Item = &str> {
        self.judged
            .iter()
            .filter(|(_, relevant)| *relevant)
            .map(|(name, _)| name.as_str())
    }

    pub fn relevant_count(&self) -> usize {
        self.relevant().count()
    }

    /// The mask as Markdown, for the record.
    pub fn render(&self) -> String {
        let mut out = format!("# Mask\n\nQuestion: {}\n\n", self.question);
        let (yes, no): (Vec<_>, Vec<_>) = self.judged.iter().partition(|(_, r)| *r);
        out.push_str(&format!("## Relevant ({})\n\n", yes.len()));
        for (name, _) in yes {
            out.push_str(&format!("- {name}\n"));
        }
        out.push_str(&format!("\n## Not relevant ({})\n\n", no.len()));
        for (name, _) in no {
            out.push_str(&format!("- {name}\n"));
        }
        if !self.excluded.is_empty() {
            out.push_str(&format!("\n## Excluded ({})\n\n", self.excluded.len()));
            for Excluded { name, why } in &self.excluded {
                out.push_str(&format!("- {name} — {why}\n"));
            }
        }
        out
    }

    /// One line, ready to print.
    pub fn describe(&self) -> String {
        let mut line = format!(
            "mask: {} of {} relevant",
            self.relevant_count(),
            count(self.judged.len(), "source")
        );
        if !self.excluded.is_empty() {
            line.push_str(&format!(" ({} excluded)", self.excluded.len()));
        }
        format!("{line} — {}", self.stage.describe())
    }
}

/// What the reading produced, from [`Corpus::ask`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Reading {
    /// Every point from every source read, in source order.
    pub points: Vec<Point>,
    /// How many points each source contributed. Zero is a source that was
    /// judged relevant and, read in full, had nothing to say.
    pub per_source: Vec<(String, usize)>,
    pub stage: Stage,
}

impl Reading {
    /// One line, ready to print.
    pub fn describe(&self) -> String {
        let silent = self.per_source.iter().filter(|(_, n)| *n == 0).count();
        let mut line = format!(
            "read: {} from {}",
            count(self.points.len(), "point"),
            count(self.per_source.len(), "source")
        );
        if silent > 0 {
            line.push_str(&format!(" ({silent} had nothing to say)"));
        }
        format!("{line} — {}", self.stage.describe())
    }
}

/// What the reduction produced, from [`reduce`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Reduction {
    /// The refined list, in the order the reading produced it.
    pub points: Vec<Point>,
    /// Points before reduction.
    pub before: usize,
    /// Pairs compared — one request each.
    pub pairs: usize,
    /// Groups merged into one point — one request each.
    pub merged: usize,
    pub compare: Stage,
    pub merge: Stage,
}

impl Reduction {
    /// One line, ready to print.
    pub fn describe(&self) -> String {
        format!(
            "reduce: {} pairs compared, {} → {} points ({} merged) — compare {}; merge {}",
            self.pairs,
            self.before,
            self.points.len(),
            self.merged,
            self.compare.describe(),
            self.merge.describe()
        )
    }
}

/// The answer, from [`answer`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Answer {
    pub markdown: String,
    pub stage: Stage,
}

impl Answer {
    /// One line, ready to print.
    pub fn describe(&self) -> String {
        format!("answer: {}", self.stage.describe())
    }
}

/// Everything a question produced, from [`Corpus::query`] — or assembled by
/// a caller that ran the stages itself, which is why it can be constructed.
#[derive(Clone, Debug)]
pub struct Distillation {
    pub question: String,
    pub mask: Mask,
    pub reading: Reading,
    /// Absent when the options turned reduction off.
    pub reduction: Option<Reduction>,
    /// Absent when the options turned the answer off.
    pub answer: Option<Answer>,
}

impl Distillation {
    /// The refined list where there is one, the raw list otherwise: what the
    /// answer was written from.
    pub fn points(&self) -> &[Point] {
        match &self.reduction {
            Some(reduction) => &reduction.points,
            None => &self.reading.points,
        }
    }

    /// Every stage's cost added up.
    pub fn total(&self) -> Stage {
        let mut total = self.mask.stage + self.reading.stage;
        if let Some(reduction) = &self.reduction {
            total = total + reduction.compare + reduction.merge;
        }
        if let Some(answer) = &self.answer {
            total = total + answer.stage;
        }
        total
    }

    /// One line per stage and a total, ready to print.
    pub fn describe(&self) -> String {
        let mut lines = vec![self.mask.describe(), self.reading.describe()];
        if let Some(reduction) = &self.reduction {
            lines.push(reduction.describe());
        }
        if let Some(answer) = &self.answer {
            lines.push(answer.describe());
        }
        lines.push(format!("total: {}", self.total().describe()));
        lines.join("\n")
    }

    /// Write everything to files under `dir`, creating it, and return what
    /// was written: `question.md`, `mask.md`, `points.raw.md`, `points.md`
    /// (only when there was a reduction), `answer.md` (only when there was
    /// an answer) and `report.md`.
    pub fn write_to(&self, dir: impl AsRef<Path>) -> Result<Vec<PathBuf>, Error> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir).map_err(|e| Error::io("create", dir, e))?;
        let mut written = Vec::new();
        let mut write = |name: &str, text: String| -> Result<(), Error> {
            let path = dir.join(name);
            std::fs::write(&path, text).map_err(|e| Error::io("write", &path, e))?;
            written.push(path);
            Ok(())
        };
        write("question.md", format!("{}\n", self.question))?;
        write("mask.md", self.mask.render())?;
        write("points.raw.md", Point::render_list(&self.reading.points))?;
        if let Some(reduction) = &self.reduction {
            write("points.md", Point::render_list(&reduction.points))?;
        }
        if let Some(answer) = &self.answer {
            write("answer.md", format!("{}\n", answer.markdown))?;
        }
        write("report.md", format!("{}\n", self.describe()))?;
        Ok(written)
    }
}

impl Corpus {
    /// Which sources a question can reach, without sending anything.
    ///
    /// A source needs a summary to be judged and a document under the budget
    /// to be read; anything short of that is excluded, with the reason.
    pub fn plan_query(&self, options: &QueryOptions) -> QueryPlan {
        let mut plan = QueryPlan::default();
        let budget_tokens = options.agent.plan_text("", "").context_tokens;
        let budget_bytes = budget_tokens.saturating_mul(agent::CHARS_PER_TOKEN);
        for source in self.sources() {
            let name = source.name.clone();
            let why = match (&source.markdown, &source.summary) {
                (None, None) => "not converted yet".to_string(),
                (Some(_), None) => "not summarized yet".to_string(),
                (None, Some(_)) => "summary without its document".to_string(),
                (Some(markdown), Some(_)) => {
                    let bytes = std::fs::metadata(markdown).map(|m| m.len()).unwrap_or(0) as usize;
                    if bytes > budget_bytes {
                        format!(
                            "document is ~{} tokens, over the {budget_tokens}-token budget",
                            bytes / agent::CHARS_PER_TOKEN
                        )
                    } else {
                        plan.reachable.push(name);
                        continue;
                    }
                }
            };
            plan.excluded.push(Excluded { name, why });
        }
        plan
    }

    /// Stage one: judge every reachable summary against the question.
    pub async fn mask(&self, question: &str, options: &QueryOptions) -> Result<Mask, Error> {
        let plan = self.plan_query(options);
        if plan.reachable.is_empty() {
            return Err(Error::NotBuilt(self.root().to_path_buf()));
        }
        let started = Instant::now();
        let judged = fanout::fan_out(
            plan.reachable,
            options.concurrency,
            |done, total| options.emit(Progress::Masking { done, total }),
            |name| async move {
                let path = self.summary_of(&name)?;
                let summary =
                    std::fs::read_to_string(&path).map_err(|e| Error::io("read", &path, e))?;
                let reply = options
                    .agent
                    .relevant(question, &summary)
                    .await
                    .map_err(Error::agent(format!("judging {name}")))?;
                Ok((name, reply))
            },
        )
        .await?;
        let usage = judged.iter().fold(Usage::default(), |sum, (_, r)| sum + r.usage);
        Ok(Mask {
            question: question.to_string(),
            judged: judged.into_iter().map(|(name, r)| (name, r.value)).collect(),
            excluded: plan.excluded,
            stage: Stage::from(started, usage),
        })
    }

    /// Stage two: read every source the mask let through, in full, and ask
    /// it the question.
    pub async fn ask(
        &self,
        question: &str,
        mask: &Mask,
        options: &QueryOptions,
    ) -> Result<Reading, Error> {
        let started = Instant::now();
        let names: Vec<String> = mask.relevant().map(str::to_string).collect();
        let replies = fanout::fan_out(
            names,
            options.concurrency,
            |done, total| options.emit(Progress::Asking { done, total }),
            |name| async move {
                let path = self.markdown_of(&name)?;
                let text =
                    std::fs::read_to_string(&path).map_err(|e| Error::io("read", &path, e))?;
                let reply = options
                    .agent
                    .ask(question, &text, &path)
                    .await
                    .map_err(Error::agent(format!("reading {name}")))?;
                Ok((name, reply))
            },
        )
        .await?;
        let mut reading = Reading::default();
        let mut usage = Usage::default();
        for (name, reply) in replies {
            usage += reply.usage;
            reading.per_source.push((name.clone(), reply.value.len()));
            reading.points.extend(reply.value.into_iter().map(|text| Point {
                text,
                sources: vec![name.clone()],
            }));
        }
        reading.stage = Stage::from(started, usage);
        Ok(reading)
    }

    /// All four stages in a row.
    pub async fn query(
        &self,
        question: &str,
        options: &QueryOptions,
    ) -> Result<Distillation, Error> {
        let mask = self.mask(question, options).await?;
        let reading = self.ask(question, &mask, options).await?;
        let reduction = if options.reduce {
            Some(reduce(reading.points.clone(), options).await?)
        } else {
            None
        };
        let points = reduction
            .as_ref()
            .map(|r| r.points.as_slice())
            .unwrap_or(&reading.points);
        let answer = if options.answer {
            Some(answer(question, points, options).await?)
        } else {
            None
        };
        Ok(Distillation {
            question: question.to_string(),
            mask,
            reading,
            reduction,
            answer,
        })
    }

    fn summary_of(&self, name: &str) -> Result<PathBuf, Error> {
        self.get(name)
            .and_then(|source| source.summary.clone())
            .ok_or_else(|| Error::Options(format!("{name} has no summary")))
    }

    fn markdown_of(&self, name: &str) -> Result<PathBuf, Error> {
        self.get(name)
            .and_then(|source| source.markdown.clone())
            .ok_or_else(|| Error::Options(format!("{name} has no document")))
    }
}

/// Stage four: answer the question from the list, in one request.
pub async fn answer(
    question: &str,
    points: &[Point],
    options: &QueryOptions,
) -> Result<Answer, Error> {
    let started = Instant::now();
    options.emit(Progress::Answering);
    let rendered = if points.is_empty() {
        // The library was read and had nothing to say. That is an answer,
        // and the model should be told so rather than handed an empty list.
        "- (no source in the library had anything to say on this question) [none]\n".to_string()
    } else {
        Point::render_list(points)
    };
    let reply = options
        .agent
        .answer(question, &rendered)
        .await
        .map_err(Error::agent("answering"))?;
    Ok(Answer {
        markdown: reply.value,
        stage: Stage::from(started, reply.usage),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::tests::Scratch;

    fn point(text: &str, sources: &[&str]) -> Point {
        Point {
            text: text.to_string(),
            sources: sources.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn points_render_with_their_sources_on_the_end() {
        let rendered = Point::render_list(&[
            point("Spreads widen at the open.", &["a/book"]),
            point("Merged.", &["a/book", "b/paper"]),
        ]);
        assert_eq!(
            rendered,
            "- Spreads widen at the open. [a/book]\n- Merged. [a/book; b/paper]\n"
        );
    }

    #[test]
    fn a_plan_says_why_each_source_is_out_of_reach() {
        let dir = Scratch::new("plan");
        dir.write("ready.md", "# Ready");
        dir.write("ready_summary.md", "ready");
        dir.write("raw.pdf", "%PDF");
        dir.write("unsummarized.md", "# U");
        dir.write("orphan_summary.md", "o");
        dir.write("huge.md", &"x".repeat(10_000));
        dir.write("huge_summary.md", "h");
        let corpus = Corpus::scan(&dir.0).unwrap();
        let options = QueryOptions::new().agent(agent::Options::new().context_tokens(1_000));
        let plan = corpus.plan_query(&options);
        assert_eq!(plan.reachable, ["ready"]);
        let reasons: Vec<(&str, &str)> = plan
            .excluded
            .iter()
            .map(|e| (e.name.as_str(), e.why.as_str()))
            .collect();
        assert_eq!(
            reasons,
            [
                ("huge", "document is ~3333 tokens, over the 1000-token budget"),
                ("orphan", "summary without its document"),
                ("raw", "not converted yet"),
                ("unsummarized", "not summarized yet"),
            ]
        );
        assert_eq!(plan.describe(), "1 source to judge, 4 excluded");
    }

    #[tokio::test]
    async fn a_library_with_nothing_built_cannot_be_asked() {
        let dir = Scratch::new("unbuilt");
        dir.write("book.md", "# Book");
        let corpus = Corpus::scan(&dir.0).unwrap();
        let error = corpus
            .mask("anything", &QueryOptions::new())
            .await
            .expect_err("not built");
        assert!(matches!(error, Error::NotBuilt(_)), "{error:?}");
    }

    #[test]
    fn a_distillation_writes_its_files_and_reports_each_stage() {
        let dir = Scratch::new("write");
        let stage = Stage {
            usage: Usage {
                requests: 1,
                input_tokens: 10,
                output_tokens: 5,
                cached_input_tokens: 0,
            },
            elapsed_ms: 100,
        };
        let distillation = Distillation {
            question: "why?".to_string(),
            mask: Mask {
                question: "why?".to_string(),
                judged: vec![("a".into(), true), ("b".into(), false)],
                excluded: vec![Excluded {
                    name: "c".into(),
                    why: "not summarized yet".into(),
                }],
                stage,
            },
            reading: Reading {
                points: vec![point("one", &["a"]), point("one again", &["a"])],
                per_source: vec![("a".into(), 2)],
                stage,
            },
            reduction: Some(Reduction {
                points: vec![point("one, twice", &["a"])],
                before: 2,
                pairs: 1,
                merged: 1,
                compare: stage,
                merge: stage,
            }),
            answer: Some(Answer {
                markdown: "Because.".to_string(),
                stage,
            }),
        };
        let out = dir.0.join("out");
        let written = distillation.write_to(&out).unwrap();
        let names: Vec<_> = written
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["question.md", "mask.md", "points.raw.md", "points.md", "answer.md", "report.md"]
        );
        let mask = std::fs::read_to_string(out.join("mask.md")).unwrap();
        assert!(mask.contains("## Relevant (1)\n\n- a\n"), "{mask}");
        assert!(mask.contains("- c — not summarized yet"), "{mask}");
        assert_eq!(
            std::fs::read_to_string(out.join("points.md")).unwrap(),
            "- one, twice [a]\n"
        );
        assert_eq!(distillation.points().len(), 1);
        assert_eq!(distillation.total().usage.requests, 5);
        let report = distillation.describe();
        assert!(report.starts_with("mask: 1 of 2 sources relevant (1 excluded)"), "{report}");
        assert!(report.contains("read: 2 points from 1 source"), "{report}");
        assert!(report.contains("reduce: 1 pairs compared, 2 → 1 points (1 merged)"), "{report}");
        assert!(report.ends_with("total: 5 requests, 50 in, 25 out, 500 ms"), "{report}");
    }
}
