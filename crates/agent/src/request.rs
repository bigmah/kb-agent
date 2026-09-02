//! The request itself: one prompt, one fresh context, one response.
//!
//! Every role in this crate comes through here. A role is a preamble that
//! says how to behave and a prompt that carries the material; this module
//! checks the key, measures the prompt against the budget, names the provider,
//! sends, and sends again when the provider was merely busy.
//!
//! # Why the prompt is measured, not split
//!
//! The prompt is sent whole or not at all. A prompt too large for the context
//! budget is refused — see [`Error::TooLarge`] — rather than split or
//! truncated. Splitting produces a summary of summaries and truncating
//! produces an answer from chapter one labelled as an answer from the book;
//! both hide the problem instead of reporting it. Whatever produced the input
//! is where the size gets fixed.
//!
//! # Why `max_tokens` is always set
//!
//! On the Anthropic path rig picks a default `max_tokens` from the model name
//! and falls back to **2048** for any name it does not recognize, which is
//! every model released after the version of rig in the tree. A reply that
//! hit that cap would come back cut off mid-sentence with nothing in the
//! response to say so. Every request built here carries an explicit cap; see
//! [`DEFAULT_MAX_TOKENS`](crate::DEFAULT_MAX_TOKENS).
//!
//! # Why there are retries
//!
//! A knowledge base fans one question out across every document in it, which
//! is many requests in flight at once, which is exactly when a provider
//! answers 429. A rate limit is not a failure of the request, so it is not
//! reported as one until the backoff runs out. Only transient answers are
//! retried — see [`retryable`] — a bad key or a malformed request fails at
//! once.
//!
//! # Provider dispatch
//!
//! The two providers hand back different model types, and `CompletionModel` is
//! not object-safe — its methods are `async` — so there is no boxing them into
//! one variable. The provider is therefore matched exactly once, here, and
//! everything past that point is generic over the model.

use std::path::Path;
use std::time::{Duration, Instant};

use rig_core::client::{CompletionClient, ProviderClient};
use rig_core::completion::{AssistantContent, CompletionError, CompletionModel};
use rig_core::http_client;
use rig_core::providers::{anthropic, openai};

use crate::{Error, Options, Plan, Progress, Provider, Reply, Usage};

/// First pause before a retry; each retry after that waits twice as long, up
/// to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

impl Options {
    /// Send `prompt` under `preamble` and return the model's text.
    ///
    /// `role` names the caller for error messages; `label` is what the prompt
    /// is called when it is measured, which is the document's path where
    /// there is one.
    pub(crate) async fn complete(
        &self,
        role: &'static str,
        preamble: &str,
        prompt: &str,
        label: &Path,
    ) -> Result<Reply<String>, Error> {
        self.validate()?;

        // Checked before anything is sent: a missing key is the one failure
        // that is certain in advance, and it should not cost a request to
        // discover.
        let variable = self.provider.api_key_env();
        if !std::env::var(variable).is_ok_and(|key| !key.trim().is_empty()) {
            return Err(Error::NoApiKey {
                provider: self.provider,
                variable,
            });
        }

        // The same measurement `Options::plan` reports, so what a plan
        // promises and what a run does cannot drift apart.
        accept(&self.plan_text(prompt, label), label)?;

        let started = Instant::now();
        let model = self.resolved_model().to_string();
        let prompt = prompt.trim();
        self.emit(Progress::Requesting);

        // The one place the provider is named. See the module docs for why it
        // is a match rather than a boxed trait object.
        let (text, usage) = match self.provider {
            Provider::OpenAi => {
                let client = openai::Client::from_env().map_err(client_error)?;
                self.send(&client.completion_model(&model), preamble, prompt)
                    .await?
            }
            Provider::Anthropic => {
                let client = anthropic::Client::from_env().map_err(client_error)?;
                self.send(&client.completion_model(&model), preamble, prompt)
                    .await?
            }
        };
        self.emit(Progress::Finished);

        let _ = role;
        Ok(Reply {
            value: text,
            provider: self.provider,
            model,
            usage,
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        })
    }

    /// The request, with the caps this crate insists on, sent until it is
    /// answered or the retries run out.
    async fn send<M: CompletionModel + Clone>(
        &self,
        model: &M,
        preamble: &str,
        prompt: &str,
    ) -> Result<(String, Usage), Error> {
        let mut attempt: u32 = 0;
        let response = loop {
            let request = model
                .completion_request(prompt)
                .preamble(preamble.to_string())
                // Never left to the default — see the module docs.
                .max_tokens(self.max_tokens)
                .temperature_opt(self.temperature)
                .build();

            match model.completion(request).await {
                Ok(response) => break response,
                Err(error) if attempt < self.retries && retryable(&error) => {
                    attempt += 1;
                    let pause = backoff(attempt);
                    self.emit(Progress::Retrying {
                        attempt,
                        after_ms: u64::try_from(pause.as_millis()).unwrap_or(u64::MAX),
                    });
                    tokio::time::sleep(pause).await;
                }
                Err(error) => return Err(error.into()),
            }
        };

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
        Ok((text.trim().to_string(), Usage::from_rig(&response.usage)))
    }
}

/// The rule this crate exists to enforce: the input fits, or nothing runs.
fn accept(plan: &Plan, label: &Path) -> Result<(), Error> {
    if plan.characters == 0 {
        return Err(Error::Empty(label.to_path_buf()));
    }
    if !plan.fits {
        return Err(Error::TooLarge {
            path: label.to_path_buf(),
            estimated_tokens: plan.estimated_tokens,
            context_tokens: plan.context_tokens,
        });
    }
    Ok(())
}

fn client_error(error: impl std::fmt::Display) -> Error {
    Error::Provider(error.to_string())
}

/// Whether an error is the provider being busy rather than the request being
/// wrong.
///
/// Rate limits, overloads, gateway errors and dropped connections all clear
/// on their own; a bad key, a bad model name or a malformed request never
/// will, and retrying those would only delay the report.
fn retryable(error: &CompletionError) -> bool {
    match error {
        CompletionError::HttpError(http) => match http {
            http_client::Error::InvalidStatusCode(status)
            | http_client::Error::InvalidStatusCodeWithMessage(status, _) => {
                transient_status(status.as_u16())
            }
            http_client::Error::InvalidStatusCodeWithDetails { status, .. } => {
                transient_status(status.as_u16())
            }
            // The transport itself failed: a reset, a timeout, a DNS hiccup.
            http_client::Error::Instance(_) => true,
            _ => false,
        },
        CompletionError::ProviderResponse(response) => response
            .status
            .is_some_and(|status| transient_status(status.as_u16())),
        // Some provider layers flatten the response to text before this crate
        // sees it, so the status is gone and the wording is all that is left.
        CompletionError::ProviderError(text) => {
            let text = text.to_ascii_lowercase();
            text.contains("overloaded")
                || text.contains("rate limit")
                || text.contains("rate_limit")
                || text.contains("too many requests")
        }
        _ => false,
    }
}

/// HTTP statuses that mean "not now" rather than "not ever".
fn transient_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504 | 529)
}

/// How long to wait before retry number `attempt` (counting from 1).
///
/// Doubles each time from [`FIRST_BACKOFF`], capped at [`MAX_BACKOFF`], with
/// up to half a second of jitter so that a hundred requests refused together
/// do not all come back together. The jitter comes from the clock rather than
/// a random number crate, which is all it needs to be.
fn backoff(attempt: u32) -> Duration {
    let doubled = FIRST_BACKOFF.saturating_mul(1u32 << attempt.saturating_sub(1).min(16));
    let jitter_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.subsec_nanos() as u64 % 500)
        .unwrap_or(0);
    doubled.min(MAX_BACKOFF) + Duration::from_millis(jitter_ms)
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
    fn a_prompt_that_fits_is_accepted() {
        assert!(accept(&plan("# Title\n\nbody\n", 1_000), Path::new("book.md")).is_ok());
    }

    #[test]
    fn an_oversized_prompt_is_refused_rather_than_split() {
        let text = "word ".repeat(5_000);
        let error = accept(&plan(&text, 100), Path::new("book.md")).expect_err("refused");
        assert!(matches!(error, Error::TooLarge { .. }), "{error:?}");
        // The message has to name the knob that moves, since the caller's other
        // option is to produce a smaller document upstream.
        assert!(error.to_string().contains("context_tokens"), "{error}");
    }

    #[test]
    fn a_prompt_with_nothing_in_it_is_refused() {
        let error = accept(&plan("   \n\n", 1_000), Path::new("book.md")).expect_err("refused");
        assert!(matches!(error, Error::Empty(_)), "{error:?}");
    }

    #[test]
    fn only_a_busy_provider_is_retried() {
        assert!(transient_status(429));
        assert!(transient_status(503));
        assert!(transient_status(529));
        assert!(!transient_status(400));
        assert!(!transient_status(401));
        assert!(!transient_status(404));

        assert!(retryable(&CompletionError::ProviderError(
            "Overloaded: try again".to_string()
        )));
        assert!(!retryable(&CompletionError::ProviderError(
            "invalid model name".to_string()
        )));
        assert!(!retryable(&CompletionError::ResponseError(
            "garbled".to_string()
        )));
    }

    #[test]
    fn the_backoff_doubles_and_then_stops_growing() {
        let strip = |d: Duration| Duration::from_secs(d.as_secs());
        assert_eq!(strip(backoff(1)), Duration::from_secs(1));
        assert_eq!(strip(backoff(2)), Duration::from_secs(2));
        assert_eq!(strip(backoff(3)), Duration::from_secs(4));
        assert!(backoff(10) <= MAX_BACKOFF + Duration::from_millis(500));
        assert!(backoff(40) <= MAX_BACKOFF + Duration::from_millis(500));
    }
}
