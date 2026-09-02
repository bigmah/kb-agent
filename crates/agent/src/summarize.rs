//! The run itself: one document, one request.
//!
//! The document is sent whole or not at all. A document too large for the
//! context budget is refused — see [`Error::TooLarge`] — rather than split into
//! sections or truncated to fit. Splitting produces a summary of summaries and
//! truncating produces a summary of chapter one labelled as a summary of the
//! book; both hide the problem instead of reporting it. Whatever produced the
//! artifact is where the size gets fixed.
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
use std::time::Instant;

use rig_core::client::{CompletionClient, ProviderClient};
use rig_core::completion::{AssistantContent, CompletionModel};
use rig_core::providers::{anthropic, openai};

use crate::{Error, Options, Plan, Progress, Provider, Summary};

/// How the model is told to behave.
const PREAMBLE: &str = "\
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

    // The same measurement `Options::plan` reports, so what a plan promises and
    // what a run does cannot drift apart.
    accept(&options.plan_text(text, path), path)?;

    let started = Instant::now();
    let model = options.resolved_model().to_string();
    let document = text.trim().to_string();
    options.emit(Progress::Requesting);

    // The one place the provider is named. See the module docs for why it is a
    // match rather than a boxed trait object.
    let (markdown, usage) = match options.provider {
        Provider::OpenAi => {
            let client = openai::Client::from_env().map_err(client_error)?;
            complete(&client.completion_model(&model), options, document).await?
        }
        Provider::Anthropic => {
            let client = anthropic::Client::from_env().map_err(client_error)?;
            complete(&client.completion_model(&model), options, document).await?
        }
    };
    options.emit(Progress::Finished);

    Ok(Summary {
        markdown,
        provider: options.provider,
        model,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

/// The rule this crate exists to enforce: the document fits, or nothing runs.
fn accept(plan: &Plan, path: &Path) -> Result<(), Error> {
    if plan.characters == 0 {
        return Err(Error::Empty(path.to_path_buf()));
    }
    if !plan.fits {
        return Err(Error::TooLarge {
            path: path.to_path_buf(),
            estimated_tokens: plan.estimated_tokens,
            context_tokens: plan.context_tokens,
        });
    }
    Ok(())
}

fn client_error(error: impl std::fmt::Display) -> Error {
    Error::Provider(error.to_string())
}

/// The request, with the caps this crate insists on.
async fn complete<M: CompletionModel + Clone>(
    model: &M,
    options: &Options,
    prompt: String,
) -> Result<(String, rig_core::completion::Usage), Error> {
    let preamble = match &options.focus {
        Some(focus) => format!("{PREAMBLE}\n\nAlso, from the person asking:\n{focus}"),
        None => PREAMBLE.to_string(),
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

    fn plan(text: &str, context_tokens: usize) -> Plan {
        Options::new()
            .context_tokens(context_tokens)
            .plan_text(text, Path::new("book.md"))
    }

    #[test]
    fn a_document_that_fits_is_accepted() {
        assert!(accept(&plan("# Title\n\nbody\n", 1_000), Path::new("book.md")).is_ok());
    }

    #[test]
    fn an_oversized_document_is_refused_rather_than_split() {
        let text = "word ".repeat(5_000);
        let error = accept(&plan(&text, 100), Path::new("book.md")).expect_err("refused");
        assert!(matches!(error, Error::TooLarge { .. }), "{error:?}");
        // The message has to name the knob that moves, since the caller's other
        // option is to produce a smaller document upstream.
        assert!(error.to_string().contains("context_tokens"), "{error}");
    }

    #[test]
    fn a_document_with_nothing_in_it_is_refused() {
        let error = accept(&plan("   \n\n", 1_000), Path::new("book.md")).expect_err("refused");
        assert!(matches!(error, Error::Empty(_)), "{error:?}");
    }

    #[test]
    fn the_preamble_forbids_a_code_fence() {
        // Bare Markdown only; a fenced response would land in the output file
        // as a literal code block.
        assert!(PREAMBLE.contains("No preamble"));
        assert!(PREAMBLE.contains("code fence"));
    }
}
