//! The reducing roles: do two points say the same thing, and if so, what is
//! the one point that says it?
//!
//! A question fanned out across a library comes back as one long list with a
//! great deal of repetition in it — every book on the subject makes the same
//! three points. The comparison sees exactly two points and nothing else, so
//! its verdict on a pair is the same whether the pair is the first it has seen
//! or the ten-thousandth; the merge sees exactly the points that were judged
//! the same, and writes the one that carries everything they did.

use std::path::Path;

use crate::relevance::parse_verdict;
use crate::{Error, Options, Reply};

const COMPARE_PREAMBLE: &str = "\
You compare two points, each drawn from a different source, and say whether \
they carry essentially the same information.

They are the SAME when a reader who had one would learn nothing of substance \
from the other: the same claim about the same thing, with figures that agree. \
Differences of wording, of emphasis, of precision (\"about 20\" against \
\"roughly 20\"), or of a qualifier that does not change what a reader would \
conclude (\"on modern venues\" against \"on modern equity venues\") do not \
make them different — the two will be merged into one point that keeps every \
qualifier, so nothing is lost by saying SAME.

They are DIFFERENT when one adds something of substance the other lacks — a \
distinct fact, figure, mechanism, cause, example or condition — or when they \
disagree. Two points on the same topic that say different things about it are \
DIFFERENT.

Reply with exactly one word: SAME or DIFFERENT.";

const MERGE_PREAMBLE: &str = "\
You are given several points, from different sources, that were judged to \
carry the same information. Write the one point that carries all of it with \
none of the repetition.

Requirements:
- Keep every figure, name, date, condition and qualification any of them has.
- Where they differ on a detail, keep both versions and say that they differ.
- State it so that it makes sense on its own.
- Return only the point, as a single Markdown bullet on one line starting \
with \"- \". No preamble, no sign-off, no code fence.";

impl Options {
    /// Whether points `a` and `b` carry the same information.
    ///
    /// One request with a fresh context. Told that wording, precision and
    /// harmless qualifiers do not make two points different, because the
    /// merge that follows keeps every qualifier — so the cost of a false
    /// "same" is nearly nothing, and the cost of a false "different" is a
    /// list that still repeats itself.
    pub async fn same_point(&self, a: &str, b: &str) -> Result<Reply<bool>, Error> {
        let prompt = format!("Point A:\n{a}\n\nPoint B:\n{b}");
        self.complete("comparison", COMPARE_PREAMBLE, &prompt, Path::new("<pair>"))
            .await?
            .try_map(|text| parse_verdict(&text, "SAME", "DIFFERENT", "comparison"))
    }

    /// One point carrying everything in `points`, which were judged the same.
    ///
    /// One request with a fresh context. Two or more points; one point is
    /// already merged and is returned as it is, without a request.
    pub async fn merge_points(&self, points: &[String]) -> Result<Reply<String>, Error> {
        match points {
            [] => {
                return Err(Error::Options(
                    "merge_points needs at least one point".to_string(),
                ));
            }
            [only] => {
                return Ok(Reply {
                    value: only.clone(),
                    provider: self.provider,
                    model: self.resolved_model().to_string(),
                    usage: crate::Usage::default(),
                    elapsed_ms: 0,
                });
            }
            _ => {}
        }
        let prompt = points
            .iter()
            .enumerate()
            .map(|(i, point)| format!("Point {}:\n{point}", i + 1))
            .collect::<Vec<_>>()
            .join("\n\n");
        Ok(self
            .complete("merge", MERGE_PREAMBLE, &prompt, Path::new("<points>"))
            .await?
            .map(|text| one_line_point(&text)))
    }
}

/// A merged point as one line: the marker stripped, wrapped lines joined.
pub(crate) fn one_line_point(text: &str) -> String {
    let joined = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    joined
        .strip_prefix("- ")
        .or_else(|| joined.strip_prefix("* "))
        .unwrap_or(&joined)
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_merged_point_comes_back_as_one_line_without_its_marker() {
        assert_eq!(one_line_point("- A point."), "A point.");
        assert_eq!(one_line_point("* A point."), "A point.");
        assert_eq!(one_line_point("A point."), "A point.");
        assert_eq!(
            one_line_point("- A point that\n  wraps.\n"),
            "A point that wraps."
        );
    }

    #[tokio::test]
    async fn a_single_point_merges_to_itself_without_a_request() {
        // No key, no network: the result has to come from the short-circuit.
        let reply = Options::new()
            .merge_points(&["only".to_string()])
            .await
            .expect("no request needed");
        assert_eq!(reply.value, "only");
        assert_eq!(reply.usage.requests, 0);
    }

    #[tokio::test]
    async fn merging_nothing_is_refused() {
        let error = Options::new().merge_points(&[]).await.expect_err("refused");
        assert!(matches!(error, Error::Options(_)), "{error:?}");
    }

    #[test]
    fn the_comparison_knows_that_merging_is_lossless() {
        assert!(COMPARE_PREAMBLE.contains("nothing is lost by saying SAME"));
        assert!(COMPARE_PREAMBLE.contains("DIFFERENT when one adds something of substance"));
    }
}
