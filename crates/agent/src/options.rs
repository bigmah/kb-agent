//! What a summary can be asked for, and what it costs to ask.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{Error, Progress, Provider, chunk};

/// Output tokens allowed per request, unless the caller says otherwise.
///
/// Always sent, never left to the provider or to rig. See
/// [`Provider::truncates_unknown_models`] for the failure that would otherwise
/// be waiting on the Anthropic path.
pub const DEFAULT_MAX_TOKENS: u64 = 50000;

/// Input tokens of source document per request.
///
/// Well under the model's window on purpose. The window is the ceiling on what
/// *fits*; this is a judgement about what one request should be asked to read
/// closely. Sections are summarized independently, so a smaller number means
/// more, more attentive passes and a longer, more expensive run.
pub const DEFAULT_SECTION_TOKENS: usize = 700_000;

/// Section summaries requested at once.
///
/// Sections do not depend on each other, so they can run concurrently; the cap
/// keeps a long document from opening fifty connections and being rate-limited
/// for it.
pub const DEFAULT_CONCURRENCY: usize = 4;

/// A summary, configured.
///
/// Every field has a working default, so `Options::new().summarize_file(path)`
/// is the whole API for most callers.
#[derive(Clone)]
pub struct Options {
    pub(crate) provider: Provider,
    /// `None` means "whatever this provider's default is", so that changing the
    /// provider does not leave a model name behind that it has never heard of.
    pub(crate) model: Option<String>,
    pub(crate) max_tokens: u64,
    pub(crate) temperature: Option<f64>,
    pub(crate) section_tokens: usize,
    pub(crate) concurrency: usize,
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
            section_tokens: DEFAULT_SECTION_TOKENS,
            concurrency: DEFAULT_CONCURRENCY,
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
            .field("section_tokens", &self.section_tokens)
            .field("concurrency", &self.concurrency)
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

    /// Output tokens allowed per request. Defaults to [`DEFAULT_MAX_TOKENS`].
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

    /// Input tokens of source document per request. Defaults to
    /// [`DEFAULT_SECTION_TOKENS`]; see [`Options::plan`] to find out what a
    /// given document would cost before running it.
    pub fn section_tokens(mut self, tokens: usize) -> Self {
        self.section_tokens = tokens;
        self
    }

    /// How many sections to summarize at once. Defaults to
    /// [`DEFAULT_CONCURRENCY`]; 1 runs them one after another.
    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
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

    /// Be told how a multi-section run is going. A document that fits in one
    /// request never reports progress; a long one can take minutes.
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
    pub(crate) fn section_chars(&self) -> usize {
        self.section_tokens
            .saturating_mul(chunk::CHARS_PER_TOKEN)
            .max(1)
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
        if self.section_tokens == 0 {
            return Err(Error::Options(
                "section_tokens must be greater than 0".to_string(),
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
    /// Cheap — it reads the file and splits it — and worth doing first, because
    /// every request after the first costs money and a long document is not
    /// obviously a long document from its name.
    pub fn plan(&self, markdown: impl AsRef<Path>) -> Result<Plan, Error> {
        let path = markdown.as_ref();
        self.validate()?;
        let text = std::fs::read_to_string(path).map_err(|e| Error::io("read", path, e))?;
        Ok(self.plan_text(&text, path))
    }

    pub(crate) fn plan_text(&self, text: &str, path: &Path) -> Plan {
        let sections = chunk::split(text, self.section_chars()).len();
        Plan {
            input: path.to_path_buf(),
            characters: text.len(),
            estimated_tokens: text.len() / chunk::CHARS_PER_TOKEN,
            sections,
            // One request per section, plus one to fuse them.
            requests: if sections > 1 { sections + 1 } else { sections },
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
    /// A rough figure — see [`chunk::CHARS_PER_TOKEN`] for why it is rough.
    pub estimated_tokens: usize,
    /// Sections the document splits into. 1 means a single request.
    pub sections: usize,
    /// Requests this would send, including the one that fuses the sections.
    pub requests: usize,
    pub provider: Provider,
    pub model: String,
}

impl Plan {
    /// One line saying what is about to happen, ready to print.
    pub fn describe(&self) -> String {
        let scale = match self.sections {
            0 => return format!("{}: nothing to summarize", self.input.display()),
            1 => "1 request".to_string(),
            n => format!("{n} sections, {} requests", self.requests),
        };
        format!(
            "{}: ~{} tokens, {scale} to {}",
            self.input.display(),
            self.estimated_tokens,
            self.model
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_document_plans_one_request() {
        let plan = Options::new().plan_text("# Title\n\nbody\n", Path::new("a.md"));
        assert_eq!(plan.sections, 1);
        assert_eq!(plan.requests, 1);
    }

    #[test]
    fn a_long_document_plans_a_fusing_request_too() {
        let text = "# A\n".to_string() + &"word ".repeat(5_000);
        let plan = Options::new()
            .section_tokens(100)
            .plan_text(&text, Path::new("a.md"));
        assert!(plan.sections > 1, "{plan:?}");
        assert_eq!(plan.requests, plan.sections + 1);
    }

    #[test]
    fn an_empty_document_plans_nothing() {
        let plan = Options::new().plan_text("\n\n", Path::new("a.md"));
        assert_eq!(plan.sections, 0);
        assert_eq!(plan.requests, 0);
        assert!(plan.describe().contains("nothing to summarize"));
    }

    #[test]
    fn bad_options_are_refused() {
        assert!(Options::new().max_tokens(0).validate().is_err());
        assert!(Options::new().section_tokens(0).validate().is_err());
        assert!(Options::new().model("  ").validate().is_err());
        assert!(Options::new().temperature(f64::NAN).validate().is_err());
        assert!(Options::new().validate().is_ok());
    }

    #[test]
    fn concurrency_is_never_zero() {
        assert_eq!(Options::new().concurrency(0).concurrency, 1);
    }
}
