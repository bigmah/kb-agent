//! What a run reports about itself, while it happens and once it is done.

/// How a multi-section run is going, delivered to [`Options::progress`].
///
/// A document that fits in one request emits nothing: it finishes before a
/// progress line would have been worth printing.
///
/// [`Options::progress`]: crate::Options::progress
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Progress {
    /// The document was split, and `total` section summaries are about to run.
    Starting { total: usize },
    /// `done` of `total` sections have come back.
    Section { done: usize, total: usize },
    /// `total` section summaries are being fused. A document with more
    /// summaries than fit in one request fuses in rounds, so this can arrive
    /// more than once with a falling `total`.
    Fusing { total: usize },
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
    /// Sections the source was split into. 1 means it fit in one request.
    pub sections: usize,
    /// Requests actually sent.
    pub requests: usize,
    /// Input tokens billed, summed across requests — the provider's own count,
    /// not the estimate a [`Plan`](crate::Plan) carries.
    pub input_tokens: u64,
    /// Output tokens billed, summed across requests.
    pub output_tokens: u64,
    /// Input tokens served from the provider's cache, of `input_tokens`.
    pub cached_input_tokens: u64,
    /// Wall-clock time for the whole run.
    pub elapsed_ms: u64,
}

impl Summary {
    /// One line saying what happened, ready to print.
    pub fn describe(&self) -> String {
        let shape = match self.sections {
            0 | 1 => "1 request".to_string(),
            n => format!("{n} sections in {} requests", self.requests),
        };
        let cached = if self.cached_input_tokens > 0 {
            format!(" ({} cached)", self.cached_input_tokens)
        } else {
            String::new()
        };
        format!(
            "done: {shape} to {} — {} in{cached}, {} out, {}",
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
            sections: 1,
            requests: 1,
            input_tokens: 1_000,
            output_tokens: 200,
            cached_input_tokens: 0,
            elapsed_ms: 4_200,
        }
    }

    #[test]
    fn a_single_request_run_says_so() {
        assert_eq!(
            summary().describe(),
            "done: 1 request to gpt-5.6 — 1000 in, 200 out, 4.2 s"
        );
    }

    #[test]
    fn a_sectioned_run_counts_its_requests() {
        let described = Summary {
            sections: 5,
            requests: 6,
            cached_input_tokens: 400,
            elapsed_ms: 95_000,
            ..summary()
        }
        .describe();
        assert!(
            described.contains("5 sections in 6 requests"),
            "{described}"
        );
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
