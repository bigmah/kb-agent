//! What a build or a query reports about itself, while it happens and once it
//! is done.

use agent::Usage;

pub use agent::format_duration;

/// How a run is going, delivered to the `progress` callback on the options
/// for that run.
///
/// Every stage is a count of requests landed out of requests to make, since
/// every stage is a fan-out; the one exception is the final answer, which is
/// one request and simply says it has started.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Progress {
    /// A PDF is about to be converted. `done` counts conversions finished so
    /// far; OCR reports its own pages through the extractor's callback.
    Converting {
        name: String,
        done: usize,
        total: usize,
    },
    /// A summary landed.
    Summarizing { done: usize, total: usize },
    /// A relevance verdict landed.
    Masking { done: usize, total: usize },
    /// A source finished being read.
    Asking { done: usize, total: usize },
    /// A pair of points was compared.
    Comparing { done: usize, total: usize },
    /// A cluster of same-information points was merged into one.
    Merging { done: usize, total: usize },
    /// The one request that writes the answer is going out.
    Answering,
}

/// What one stage of a run cost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stage {
    /// The provider's own token counts, summed over the stage's requests.
    pub usage: Usage,
    /// Wall-clock time for the stage, with all its requests in flight at
    /// once — not the sum of their latencies.
    pub elapsed_ms: u64,
}

impl Stage {
    pub(crate) fn from(started: std::time::Instant, usage: Usage) -> Self {
        Self {
            usage,
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }

    /// `12 requests, 400000 in, 2000 out, 1 min 3 s`.
    pub fn describe(&self) -> String {
        format!(
            "{}, {}, {}",
            self.usage.requests_phrase(),
            self.usage.describe(),
            format_duration(self.elapsed_ms)
        )
    }
}

impl std::ops::Add for Stage {
    type Output = Stage;
    fn add(self, other: Stage) -> Stage {
        Stage {
            usage: self.usage + other.usage,
            elapsed_ms: self.elapsed_ms + other.elapsed_ms,
        }
    }
}

/// `1 source` or `n sources`, and the same for anything else.
pub(crate) fn count(n: usize, singular: &str) -> String {
    match n {
        1 => format!("1 {singular}"),
        n => format!("{n} {singular}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stage_reads_as_one_clause() {
        let stage = Stage {
            usage: Usage {
                requests: 12,
                input_tokens: 400_000,
                output_tokens: 2_000,
                cached_input_tokens: 0,
            },
            elapsed_ms: 63_000,
        };
        assert_eq!(stage.describe(), "12 requests, 400000 in, 2000 out, 1 min 3 s");
    }

    #[test]
    fn counts_agree_in_number() {
        assert_eq!(count(1, "source"), "1 source");
        assert_eq!(count(0, "source"), "0 sources");
        assert_eq!(count(7, "point"), "7 points");
    }
}
