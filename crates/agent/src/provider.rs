//! Which service does the summarizing.
//!
//! Everything else in this crate is provider-agnostic — rig's `CompletionModel`
//! trait is the whole interface the run needs — so a provider is only three
//! facts: how to build the client, what to call if the caller does not name a
//! model, and which environment variable holds the key.

/// The service a summary is sent to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Provider {
    /// OpenAI, through the Responses API. Reads `OPENAI_API_KEY`.
    #[default]
    OpenAi,
    /// Anthropic. Reads `ANTHROPIC_API_KEY`.
    Anthropic,
}

impl Provider {
    /// The model used when the caller does not name one.
    pub fn default_model(self) -> &'static str {
        match self {
            // rig's newest constant, which is what its own examples reach for.
            Self::OpenAi => rig_core::providers::openai::GPT_5_6,
            // rig 0.42's Anthropic constants stop at `claude-opus-4-8`, so this
            // is a literal. `completion_model` takes a `&str`, so a model newer
            // than the crate is not a problem — but see `Options::max_tokens`
            // for the one place where an unrecognized name does bite.
            Self::Anthropic => "claude-opus-5",
        }
    }

    /// The environment variable holding this provider's API key.
    pub fn api_key_env(self) -> &'static str {
        match self {
            Self::OpenAi => "OPENAI_API_KEY",
            Self::Anthropic => "ANTHROPIC_API_KEY",
        }
    }

    /// Whether an unrecognized model name gets a silently tiny output cap.
    ///
    /// rig derives a default `max_tokens` from the model name for Anthropic and
    /// falls back to **2048** for any name it does not know — which is every
    /// model released after the rig in the tree. A summary would come back cut
    /// off mid-sentence with nothing in the response to say so. OpenAI leaves
    /// the field unset instead, which is harmless.
    ///
    /// This crate always sets the cap explicitly, so it is never exposed to
    /// that. It is public because anyone building requests on rig directly is.
    pub const fn truncates_unknown_models(self) -> bool {
        matches!(self, Self::Anthropic)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chatgpt_is_the_default() {
        assert_eq!(Provider::default(), Provider::OpenAi);
        assert!(Provider::default().default_model().starts_with("gpt-"));
    }

    #[test]
    fn every_provider_names_a_model_and_a_key() {
        for provider in [Provider::OpenAi, Provider::Anthropic] {
            assert!(!provider.default_model().is_empty());
            assert!(provider.api_key_env().ends_with("_API_KEY"));
            assert!(!provider.name().is_empty());
        }
    }

    #[test]
    fn only_anthropic_has_the_silent_cap() {
        assert!(Provider::Anthropic.truncates_unknown_models());
        assert!(!Provider::OpenAi.truncates_unknown_models());
    }
}
