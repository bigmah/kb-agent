//! The run itself: split, summarize each section, fuse the section summaries.
//!
//! A document that fits in one request gets one request. Anything longer is
//! summarized in sections and the section summaries are then summarized —
//! see [`crate::chunk`] for why the alternative, truncating, is not on the
//! table.
//!
//! # Why `max_tokens` is always set
//!
//! On the Anthropic path rig picks a default `max_tokens` from the model name
//! and falls back to **2048** for any name it does not recognize, which is
//! every model released after the version of rig in the tree. A summary that
//! hit that cap would come back cut off mid-sentence with nothing in the
//! response to say so. Every request built here carries an explicit cap; see
//! [`DEFAULT_MAX_TOKENS`](crate::DEFAULT_MAX_TOKENS).
//!
//! # Provider dispatch
//!
//! The two providers hand back different model types, and `CompletionModel` is
//! not object-safe — its methods are `async` — so there is no boxing them into
//! one variable. The provider is therefore matched exactly once, at the top,
//! and everything past that point is generic over the model.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use futures::stream::{StreamExt, TryStreamExt, iter};
use rig_core::client::{CompletionClient, ProviderClient};
use rig_core::completion::{AssistantContent, CompletionModel};
use rig_core::providers::{anthropic, openai};

use crate::{Error, Options, Progress, Provider, Summary, chunk};

/// How the model is told to behave when it can see the whole document.
const WHOLE: &str = "\
You summarize documents. You are given a document in Markdown and you return a \
summary of it in Markdown.

Requirements:
- Open with a single paragraph saying what the document is and what it covers.
- Then the substance, under `##` headings that follow the document's own \
structure. Preserve the author's terminology, names, figures and dates exactly.
- Record what the document actually says, including conclusions you find \
unconvincing. Do not add commentary, praise, or information from outside it.
- If part of the document is unreadable or looks garbled, say so in one line \
rather than guessing at it.
- Return only the summary. No preamble, no sign-off, no code fence around it.";

/// The same, for one section of a document too long to read at once.
const SECTION: &str = "\
You summarize documents. You are given ONE SECTION of a longer Markdown \
document and you return a summary of that section in Markdown.

Requirements:
- Cover only what is in this section. Do not speculate about the rest of the \
document or refer to it.
- Use `##` headings that follow the section's own structure. Preserve the \
author's terminology, names, figures and dates exactly.
- Be denser than you would be for a whole document: this summary will be read \
alongside the other sections' summaries and fused into one.
- Return only the summary. No preamble, no sign-off, no code fence around it.";

/// And the fusing pass.
const FUSE: &str = "\
You are given summaries of consecutive sections of one Markdown document, in \
document order, separated by `---`. Fuse them into a single coherent summary of \
the whole document, in Markdown.

Requirements:
- Open with a single paragraph saying what the document is and what it covers.
- Then the substance, under `##` headings. Merge material that the section \
summaries repeat, and keep the document's overall order.
- Preserve terminology, names, figures and dates exactly as given. Add nothing \
that is not in the section summaries.
- Do not mention the sections, the summarizing, or that you were given parts.
- Return only the summary. No preamble, no sign-off, no code fence around it.";

/// Between section summaries handed to the fusing pass.
const SEPARATOR: &str = "\n\n---\n\n";

/// A ceiling on repeated fusing rounds. Each round strictly shrinks the input,
/// so this is a guard against a bug rather than an expected limit.
const MAX_ROUNDS: usize = 8;

/// Running totals across every request a summary took.
#[derive(Default)]
struct Totals {
    requests: usize,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
}

impl Totals {
    fn add(&mut self, usage: &rig_core::completion::Usage) {
        self.requests += 1;
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.cached_input_tokens += usage.cached_input_tokens;
    }
}

pub(crate) async fn summarize(
    text: &str,
    path: &Path,
    options: &Options,
) -> Result<Summary, Error> {
    options.validate()?;

    // Checked before anything is sent: a missing key is the one failure that is
    // certain in advance, and it should not cost a request to discover.
    let variable = options.provider.api_key_env();
    if !std::env::var(variable).is_ok_and(|key| !key.trim().is_empty()) {
        return Err(Error::NoApiKey {
            provider: options.provider,
            variable,
        });
    }

    let sections = chunk::split(text, options.section_chars());
    if sections.is_empty() {
        return Err(Error::Empty(path.to_path_buf()));
    }
    if sections.len() > 1 {
        can_converge(options)?;
    }

    let started = Instant::now();
    let model = options.resolved_model().to_string();

    // The one place the provider is named. See the module docs for why it is a
    // match rather than a boxed trait object.
    let (markdown, totals) = match options.provider {
        Provider::OpenAi => {
            let client = openai::Client::from_env().map_err(client_error)?;
            run(&client.completion_model(&model), options, sections.clone()).await?
        }
        Provider::Anthropic => {
            let client = anthropic::Client::from_env().map_err(client_error)?;
            run(&client.completion_model(&model), options, sections.clone()).await?
        }
    };
    options.emit(Progress::Finished);

    Ok(Summary {
        markdown,
        provider: options.provider,
        model,
        sections: sections.len(),
        requests: totals.requests,
        input_tokens: totals.input_tokens,
        output_tokens: totals.output_tokens,
        cached_input_tokens: totals.cached_input_tokens,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

fn client_error(error: impl std::fmt::Display) -> Error {
    Error::Provider(error.to_string())
}

/// Refuse a configuration whose fusing pass could never finish.
///
/// A section summary is up to `max_tokens` long, and fusing works by packing
/// section summaries into a request. If two summaries cannot fit in one
/// request, packing them makes no progress and the run would spend money on
/// section summaries before discovering it. Checked once the document is known
/// to need more than one section, which is the only case it can bite.
fn can_converge(options: &Options) -> Result<(), Error> {
    let summary_chars = (options.max_tokens as usize).saturating_mul(chunk::CHARS_PER_TOKEN);
    let needed = summary_chars
        .saturating_mul(2)
        .saturating_add(SEPARATOR.len());
    if needed > options.section_chars() {
        return Err(Error::Options(format!(
            "this document needs more than one section, but two section summaries \
             (~{needed} characters at max_tokens {}) would not fit in one \
             {}-character section — raise section_tokens above {} or lower max_tokens",
            options.max_tokens,
            options.section_chars(),
            needed / chunk::CHARS_PER_TOKEN,
        )));
    }
    Ok(())
}

/// Everything after the provider is chosen.
async fn run<M: CompletionModel + Clone>(
    model: &M,
    options: &Options,
    sections: Vec<String>,
) -> Result<(String, Totals), Error> {
    let mut totals = Totals::default();
    let markdown = if sections.len() == 1 {
        options.emit(Progress::Starting { total: 1 });
        let (text, usage) = complete(model, options, WHOLE, sections[0].clone()).await?;
        totals.add(&usage);
        options.emit(Progress::Section { done: 1, total: 1 });
        text
    } else {
        let parts = map(model, options, sections, &mut totals).await?;
        reduce(model, options, parts, &mut totals).await?
    };
    Ok((markdown, totals))
}

/// Summarize every section, `concurrency` at a time, in document order.
async fn map<M: CompletionModel + Clone>(
    model: &M,
    options: &Options,
    sections: Vec<String>,
    totals: &mut Totals,
) -> Result<Vec<String>, Error> {
    let total = sections.len();
    options.emit(Progress::Starting { total });

    // `buffered` keeps results in document order however they finish, so the
    // count has to be its own thing rather than an index.
    let done = AtomicUsize::new(0);
    let results: Vec<(String, rig_core::completion::Usage)> = iter(
        sections
            .into_iter()
            .map(|section| summarize_one(model, options, SECTION, section, &done, total)),
    )
    .buffered(options.concurrency)
    .try_collect()
    .await?;

    Ok(results
        .into_iter()
        .map(|(text, usage)| {
            totals.add(&usage);
            text
        })
        .collect())
}

/// One section summary that reports itself finished.
async fn summarize_one<M: CompletionModel + Clone>(
    model: &M,
    options: &Options,
    preamble: &str,
    section: String,
    done: &AtomicUsize,
    total: usize,
) -> Result<(String, rig_core::completion::Usage), Error> {
    let result = complete(model, options, preamble, section).await?;
    options.emit(Progress::Section {
        done: done.fetch_add(1, Ordering::Relaxed) + 1,
        total,
    });
    Ok(result)
}

/// Fuse section summaries into one, in as many rounds as it takes.
///
/// Normally one round: the section summaries are each capped at `max_tokens`,
/// so a handful of them fit in a single request comfortably. A document with
/// enough sections that even their summaries overflow gets fused in groups
/// first, and the group summaries fused again.
async fn reduce<M: CompletionModel + Clone>(
    model: &M,
    options: &Options,
    mut parts: Vec<String>,
    totals: &mut Totals,
) -> Result<String, Error> {
    let budget = options.section_chars();

    for _ in 0..MAX_ROUNDS {
        let joined = parts.join(SEPARATOR);
        if joined.len() <= budget {
            options.emit(Progress::Fusing { total: parts.len() });
            let (text, usage) = complete(model, options, FUSE, joined).await?;
            totals.add(&usage);
            return Ok(text);
        }

        // Too much even for the fusing pass: fuse in groups and try again.
        let groups = group(&parts, budget);
        if groups.len() >= parts.len() {
            return Err(Error::Options(format!(
                "a single section summary exceeds the {budget}-character section budget; \
                 raise section_tokens or lower max_tokens"
            )));
        }
        options.emit(Progress::Fusing {
            total: groups.len(),
        });

        let done = AtomicUsize::new(0);
        let total = groups.len();
        let results: Vec<(String, rig_core::completion::Usage)> = iter(
            groups
                .into_iter()
                .map(|group| summarize_one(model, options, FUSE, group, &done, total)),
        )
        .buffered(options.concurrency)
        .try_collect()
        .await?;

        parts = results
            .into_iter()
            .map(|(text, usage)| {
                totals.add(&usage);
                text
            })
            .collect();
    }
    Err(Error::Options(
        "the document did not reduce to a single summary; raise section_tokens".to_string(),
    ))
}

/// Pack `parts` into as few groups as fit the budget, keeping them in order.
fn group(parts: &[String], budget: usize) -> Vec<String> {
    let mut groups: Vec<String> = Vec::new();
    let mut current = String::new();
    for part in parts {
        if current.is_empty() {
            current = part.clone();
        } else if current.len() + SEPARATOR.len() + part.len() <= budget {
            current.push_str(SEPARATOR);
            current.push_str(part);
        } else {
            groups.push(std::mem::take(&mut current));
            current = part.clone();
        }
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
}

/// One request, with the caps this crate insists on.
async fn complete<M: CompletionModel + Clone>(
    model: &M,
    options: &Options,
    preamble: &str,
    prompt: String,
) -> Result<(String, rig_core::completion::Usage), Error> {
    let preamble = match &options.focus {
        Some(focus) => format!("{preamble}\n\nAlso, from the person asking:\n{focus}"),
        None => preamble.to_string(),
    };

    let request = model
        .completion_request(prompt)
        .preamble(preamble)
        // Never left to the default — see the module docs.
        .max_tokens(options.max_tokens)
        .temperature_opt(options.temperature)
        .build();

    let response = model.completion(request).await?;

    let text: String = response
        .choice
        .iter()
        .filter_map(|content| match content {
            AssistantContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    if text.trim().is_empty() {
        return Err(Error::NoContent);
    }
    Ok((text.trim().to_string(), response.usage))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping_packs_in_order_and_shrinks() {
        let parts: Vec<String> = ["aaaa", "bbbb", "cccc", "dddd"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let groups = group(&parts, 20);
        assert!(groups.len() < parts.len(), "{groups:?}");
        let flattened: String = groups.join("");
        for part in &parts {
            assert!(flattened.contains(part.as_str()), "lost {part}");
        }
        // Order is preserved.
        assert!(groups[0].starts_with("aaaa"));
    }

    #[test]
    fn grouping_cannot_shrink_oversized_parts() {
        // Each part alone exceeds the budget, so grouping makes no progress —
        // the case `reduce` turns into an actionable error rather than looping.
        let parts: Vec<String> = ["x".repeat(50), "y".repeat(50)].into_iter().collect();
        assert_eq!(group(&parts, 10).len(), parts.len());
    }

    #[test]
    fn the_preambles_forbid_a_code_fence() {
        // Each preamble must say to return bare Markdown; a fenced response
        // would land in the output file as a literal code block.
        for preamble in [WHOLE, SECTION, FUSE] {
            assert!(preamble.contains("No preamble"), "{preamble}");
            assert!(preamble.contains("code fence"), "{preamble}");
        }
    }
}
