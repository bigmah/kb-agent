//! What a request reports about itself, while it happens and once it is done.

use std::ops::{Add, AddAssign};

/// How a request is going, delivered to [`Options::progress`].
///
/// One role is one request, so there is little to say: it went out, it may
/// have had to go out again, and it came back.
///
/// [`Options::progress`]: crate::Options::progress
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Progress {
    /// The prompt fits and the request is going out.
    Requesting,
    /// The provider turned the request away with something transient — a rate
    /// limit, an overload, a dropped connection — and it is going out again
    /// after a pause. `attempt` counts from 1.
    Retrying { attempt: u32, after_ms: u64 },
    /// Everything is done.
    Finished,
}

/// What the provider billed, in its own counts rather than an estimate.
///
/// Adds up, so a run of many requests can report itself with the same type one
/// request does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Requests that came back with a response. A retried request counts once.
    pub requests: u64,
    /// Input tokens billed.
    pub input_tokens: u64,
    /// Output tokens billed.
    pub output_tokens: u64,
    /// Input tokens served from the provider's cache, of `input_tokens`.
    pub cached_input_tokens: u64,
}

impl Usage {
    pub(crate) fn from_rig(usage: &rig_core::completion::Usage) -> Self {
        Self {
            requests: 1,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_input_tokens: usage.cached_input_tokens,
        }
    }

    /// The token counts, ready to drop into a sentence: `1000 in, 200 out`,
    /// with the cached share called out when there was one.
    pub fn describe(&self) -> String {
        let cached = if self.cached_input_tokens > 0 {
            format!(" ({} cached)", self.cached_input_tokens)
        } else {
            String::new()
        };
        format!("{} in{cached}, {} out", self.input_tokens, self.output_tokens)
    }

    /// `1 request` or `n requests`.
    pub fn requests_phrase(&self) -> String {
        match self.requests {
            1 => "1 request".to_string(),
            n => format!("{n} requests"),
        }
    }
}

impl Add for Usage {
    type Output = Usage;
    fn add(self, other: Usage) -> Usage {
        Usage {
            requests: self.requests + other.requests,
            input_tokens: self.input_tokens + other.input_tokens,
            output_tokens: self.output_tokens + other.output_tokens,
            cached_input_tokens: self.cached_input_tokens + other.cached_input_tokens,
        }
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, other: Usage) {
        *self = *self + other;
    }
}

/// What one role produced, with what it took to produce it.
///
/// `T` is whatever the role returns once the model's text has been read:
/// Markdown for a summary or an answer, a list of points from one source, a
/// verdict from a comparison.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Reply<T> {
    /// The role's result.
    pub value: T,
    /// Who produced it.
    pub provider: crate::Provider,
    /// The model that produced it.
    pub model: String,
    /// The provider's own token counts, not the estimate a
    /// [`Plan`](crate::Plan) carries.
    pub usage: Usage,
    /// Wall-clock time for the request, retries included.
    pub elapsed_ms: u64,
}

impl<T> Reply<T> {
    /// The same reply with its value read differently, for a role that parses
    /// the text it got back.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Reply<U> {
        Reply {
            value: f(self.value),
            provider: self.provider,
            model: self.model,
            usage: self.usage,
            elapsed_ms: self.elapsed_ms,
        }
    }

    /// The same, where reading the value can fail.
    pub fn try_map<U, E>(self, f: impl FnOnce(T) -> Result<U, E>) -> Result<Reply<U>, E> {
        Ok(Reply {
            value: f(self.value)?,
            provider: self.provider,
            model: self.model,
            usage: self.usage,
            elapsed_ms: self.elapsed_ms,
        })
    }

    /// One line saying what happened, ready to print.
    pub fn describe(&self) -> String {
        format!(
            "done: {} to {} — {}, {}",
            self.usage.requests_phrase(),
            self.model,
            self.usage.describe(),
            format_duration(self.elapsed_ms),
        )
    }
}

/// Render a millisecond count at a granularity a human reads at a glance.
pub fn format_duration(ms: u64) -> String {
    match ms {
        0..1_000 => format!("{ms} ms"),
        1_000..60_000 => format!("{:.1} s", ms as f64 / 1_000.0),
        _ => {
            let seconds = ms / 1_000;
            format!("{} min {} s", seconds / 60, seconds % 60)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply() -> Reply<String> {
        Reply {
            value: "text".to_string(),
            provider: crate::Provider::OpenAi,
            model: "gpt-5.6".to_string(),
            usage: Usage {
                requests: 1,
                input_tokens: 1_000,
                output_tokens: 200,
                cached_input_tokens: 0,
            },
            elapsed_ms: 4_200,
        }
    }

    #[test]
    fn a_reply_reports_the_provider_s_own_counts() {
        assert_eq!(
            reply().describe(),
            "done: 1 request to gpt-5.6 — 1000 in, 200 out, 4.2 s"
        );
    }

    #[test]
    fn cached_input_is_called_out_when_there_was_any() {
        let mut reply = reply();
        reply.usage.cached_input_tokens = 400;
        reply.elapsed_ms = 95_000;
        let described = reply.describe();
        assert!(described.contains("(400 cached)"), "{described}");
        assert!(described.contains("1 min 35 s"), "{described}");
    }

    #[test]
    fn usage_adds_up() {
        let a = reply().usage;
        let mut total = a + a;
        total += a;
        assert_eq!(total.requests, 3);
        assert_eq!(total.input_tokens, 3_000);
        assert_eq!(total.requests_phrase(), "3 requests");
        assert_eq!(Usage::default().requests_phrase(), "0 requests");
    }

    #[test]
    fn a_reply_maps_its_value_and_keeps_the_rest() {
        let mapped = reply().map(|text| text.len());
        assert_eq!(mapped.value, 4);
        assert_eq!(mapped.usage.input_tokens, 1_000);
        let failed: Result<Reply<()>, &str> = reply().try_map(|_| Err("no"));
        assert!(failed.is_err());
    }

    #[test]
    fn durations_read_at_a_glance() {
        assert_eq!(format_duration(999), "999 ms");
        assert_eq!(format_duration(1_500), "1.5 s");
        assert_eq!(format_duration(61_000), "1 min 1 s");
    }
}
