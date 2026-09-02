//! What a summary can be asked for, and what it costs to ask.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{Error, Progress, Provider};

/// Output tokens allowed per request, unless the caller says otherwise.
///
/// Always sent, never left to the provider or to rig. See
/// [`Provider::truncates_unknown_models`] for the failure that would otherwise
/// be waiting on the Anthropic path.
pub const DEFAULT_MAX_TOKENS: u64 = 50000;

/// Input tokens of source document a request may carry.
///
/// Well under the model's window on purpose: the window has to hold the
/// preamble and the summary as well as the document, and the token estimate
/// here is approximate. A document over this budget is refused — see
/// [`Error::TooLarge`] — rather than split or truncated.
pub const DEFAULT_CONTEXT_TOKENS: usize = 700_000;

/// Characters assumed per token when converting a token budget to a character
/// budget.
///
/// The real answer needs the model's tokenizer, which is not available locally,
/// and the `count_tokens` endpoint would cost a network round trip. This
/// approximation is deliberately pessimistic: English prose runs closer to 4
/// characters per token, so budgeting at 3 leaves roughly a quarter of the
/// window as headroom for the preamble, the response, and any text that
/// tokenizes worse than prose (code, tables, non-Latin scripts).
pub const CHARS_PER_TOKEN: usize = 3;

/// A summary, configured.
///
/// Every field has a working default, so `Options::new().summarize(path)` is
/// the whole API for most callers.
#[derive(Clone)]
pub struct Options {
    pub(crate) provider: Provider,
    /// `None` means "whatever this provider's default is", so that changing the
    /// provider does not leave a model name behind that it has never heard of.
    pub(crate) model: Option<String>,
    pub(crate) max_tokens: u64,
    pub(crate) temperature: Option<f64>,
    pub(crate) context_tokens: usize,
    pub(crate) focus: Option<String>,
    pub(crate) progress: Option<Arc<dyn Fn(Progress) + Send + Sync>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            provider: Provider::default(),
            model: None,
            max_tokens: DEFAULT_MAX_TOKENS,
            temperature: None,
            context_tokens: DEFAULT_CONTEXT_TOKENS,
            focus: None,
            progress: None,
        }
    }
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("provider", &self.provider)
            .field("model", &self.resolved_model())
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .field("context_tokens", &self.context_tokens)
            .field("focus", &self.focus)
            .field("progress", &self.progress.as_ref().map(|_| "<callback>"))
            .finish()
    }
}

impl Options {
    pub fn new() -> Self {
        Self::default()
    }

    /// Which service to summarize with. Defaults to
    /// [`Provider::OpenAi`] — ChatGPT.
    ///
    /// Changing this also changes the default model, so the two stay
    /// consistent unless you set [`Options::model`] as well.
    pub fn provider(mut self, provider: Provider) -> Self {
        self.provider = provider;
        self
    }

    /// The model to summarize with. Defaults to the provider's own default —
    /// see [`Provider::default_model`].
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Output tokens allowed for the summary. Defaults to
    /// [`DEFAULT_MAX_TOKENS`].
    ///
    /// Raise it for a longer summary; a request that hits the cap comes back
    /// truncated rather than failing.
    pub fn max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Sampling temperature. Left unset by default, which takes the provider's.
    pub fn temperature(mut self, temperature: f64) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Input tokens of source document a request may carry. Defaults to
    /// [`DEFAULT_CONTEXT_TOKENS`].
    ///
    /// A document over the budget is refused. Raise this for a model with a
    /// larger window; produce a smaller document for one without.
    pub fn context_tokens(mut self, tokens: usize) -> Self {
        self.context_tokens = tokens;
        self
    }

    /// What the summary should pay attention to, in your own words — appended
    /// to the instructions the model is given.
    ///
    /// ```
    /// # use agent::Options;
    /// Options::new().focus("Keep every figure and date. Skip the front matter.");
    /// ```
    pub fn focus(mut self, focus: impl Into<String>) -> Self {
        self.focus = Some(focus.into());
        self
    }

    /// Be told how the run is going. One request is one round trip, which on a
    /// document this size can still be minutes of silence.
    pub fn progress(mut self, on_progress: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        self.progress = Some(Arc::new(on_progress));
        self
    }

    /// The model this configuration will actually use.
    pub fn resolved_model(&self) -> &str {
        self.model
            .as_deref()
            .unwrap_or_else(|| self.provider.default_model())
    }

    /// Source characters allowed in one request.
    pub(crate) fn context_chars(&self) -> usize {
        self.context_tokens.saturating_mul(CHARS_PER_TOKEN).max(1)
    }

    pub(crate) fn emit(&self, event: Progress) {
        if let Some(callback) = &self.progress {
            callback(event);
        }
    }

    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.resolved_model().trim().is_empty() {
            return Err(Error::Options("no model was named".to_string()));
        }
        if self.max_tokens == 0 {
            return Err(Error::Options(
                "max_tokens must be greater than 0".to_string(),
            ));
        }
        if self.context_tokens == 0 {
            return Err(Error::Options(
                "context_tokens must be greater than 0".to_string(),
            ));
        }
        if let Some(temperature) = self.temperature
            && !temperature.is_finite()
        {
            return Err(Error::Options(
                "temperature must be a finite number".to_string(),
            ));
        }
        Ok(())
    }

    /// What summarizing `markdown` would take, without sending anything.
    ///
    /// Cheap — it reads the file and measures it — and worth doing first,
    /// because the request costs money and because a document over the budget
    /// is refused rather than summarized in part.
    pub fn plan(&self, markdown: impl AsRef<Path>) -> Result<Plan, Error> {
        let path = markdown.as_ref();
        self.validate()?;
        let text = std::fs::read_to_string(path).map_err(|e| Error::io("read", path, e))?;
        Ok(self.plan_text(&text, path))
    }

    pub(crate) fn plan_text(&self, text: &str, path: &Path) -> Plan {
        // Leading and trailing whitespace is not sent, so it is not measured.
        let characters = text.trim().len();
        Plan {
            input: path.to_path_buf(),
            characters,
            estimated_tokens: characters / CHARS_PER_TOKEN,
            context_tokens: self.context_tokens,
            fits: characters <= self.context_chars(),
            provider: self.provider,
            model: self.resolved_model().to_string(),
        }
    }
}

/// What a run would involve, from [`Options::plan`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Plan {
    pub input: PathBuf,
    pub characters: usize,
    /// A rough figure — see [`CHARS_PER_TOKEN`] for why it is rough.
    pub estimated_tokens: usize,
    /// The budget the document was measured against.
    pub context_tokens: usize,
    /// Whether the document fits in one request. `false` means
    /// [`Options::summarize`] would refuse it with [`Error::TooLarge`].
    pub fits: bool,
    pub provider: Provider,
    pub model: String,
}

impl Plan {
    /// One line saying what is about to happen, ready to print.
    pub fn describe(&self) -> String {
        if self.characters == 0 {
            return format!("{}: nothing to summarize", self.input.display());
        }
        let outcome = match self.fits {
            true => format!("1 request to {}", self.model),
            false => format!(
                "over the {}-token budget — will be refused",
                self.context_tokens
            ),
        };
        format!(
            "{}: ~{} tokens, {outcome}",
            self.input.display(),
            self.estimated_tokens,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_that_fits_plans_one_request() {
        let plan = Options::new().plan_text("# Title\n\nbody\n", Path::new("a.md"));
        assert!(plan.fits);
        assert!(plan.describe().contains("1 request"), "{plan:?}");
    }

    #[test]
    fn a_document_over_the_budget_does_not_fit() {
        let text = "# A\n".to_string() + &"word ".repeat(5_000);
        let plan = Options::new()
            .context_tokens(100)
            .plan_text(&text, Path::new("a.md"));
        assert!(!plan.fits, "{plan:?}");
        assert!(plan.describe().contains("over the 100-token budget"));
    }

    #[test]
    fn an_empty_document_plans_nothing() {
        let plan = Options::new().plan_text("\n\n", Path::new("a.md"));
        assert_eq!(plan.characters, 0);
        assert!(plan.describe().contains("nothing to summarize"));
    }

    #[test]
    fn bad_options_are_refused() {
        assert!(Options::new().max_tokens(0).validate().is_err());
        assert!(Options::new().context_tokens(0).validate().is_err());
        assert!(Options::new().model("  ").validate().is_err());
        assert!(Options::new().temperature(f64::NAN).validate().is_err());
        assert!(Options::new().validate().is_ok());
    }
}
