//! What a run reports about itself, while it happens and once it is done.

/// How the run is going, delivered to [`Options::progress`].
///
/// One document is one request, so there are only two things to say: it went
/// out, and it came back.
///
/// [`Options::progress`]: crate::Options::progress
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Progress {
    /// The document fits and the request is going out.
    Requesting,
    /// Everything is done.
    Finished,
}

/// A finished summary.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Summary {
    /// The summary itself.
    pub markdown: String,
    /// Who wrote it.
    pub provider: crate::Provider,
    /// The model that wrote it.
    pub model: String,
    /// Input tokens billed — the provider's own count, not the estimate a
    /// [`Plan`](crate::Plan) carries.
    pub input_tokens: u64,
    /// Output tokens billed.
    pub output_tokens: u64,
    /// Input tokens served from the provider's cache, of `input_tokens`.
    pub cached_input_tokens: u64,
    /// Wall-clock time for the whole run.
    pub elapsed_ms: u64,
}

impl Summary {
    /// One line saying what happened, ready to print.
    pub fn describe(&self) -> String {
        let cached = if self.cached_input_tokens > 0 {
            format!(" ({} cached)", self.cached_input_tokens)
        } else {
            String::new()
        };
        format!(
            "done: 1 request to {} — {} in{cached}, {} out, {}",
            self.model,
            self.input_tokens,
            self.output_tokens,
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

    fn summary() -> Summary {
        Summary {
            markdown: "text".to_string(),
            provider: crate::Provider::OpenAi,
            model: "gpt-5.6".to_string(),
            input_tokens: 1_000,
            output_tokens: 200,
            cached_input_tokens: 0,
            elapsed_ms: 4_200,
        }
    }

    #[test]
    fn a_run_reports_the_provider_s_own_counts() {
        assert_eq!(
            summary().describe(),
            "done: 1 request to gpt-5.6 — 1000 in, 200 out, 4.2 s"
        );
    }

    #[test]
    fn cached_input_is_called_out_when_there_was_any() {
        let described = Summary {
            cached_input_tokens: 400,
            elapsed_ms: 95_000,
            ..summary()
        }
        .describe();
        assert!(described.contains("(400 cached)"), "{described}");
        assert!(described.contains("1 min 35 s"), "{described}");
    }

    #[test]
    fn durations_read_at_a_glance() {
        assert_eq!(format_duration(999), "999 ms");
        assert_eq!(format_duration(1_500), "1.5 s");
        assert_eq!(format_duration(61_000), "1 min 1 s");
    }
}
