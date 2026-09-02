//! Splitting a Markdown document into pieces that each fit one request.
//!
//! The rule this module exists to enforce is that nothing is ever dropped. A
//! document too large for one request is summarized in sections and the section
//! summaries are then summarized — expensive, but it reads the whole document.
//! Truncating to the first N characters would be cheaper and would silently
//! produce a summary of chapter one labelled as a summary of the book.
//!
//! Splits are taken at the best boundary available, in order: a Markdown
//! heading, then a blank line, then a line. A fenced code block is never split,
//! because half a fence turns the rest of the document into code.
//!
//! # On measuring in characters
//!
//! Chunk sizes here are in characters, and the budget is derived from a token
//! budget by [`CHARS_PER_TOKEN`]. That ratio is an approximation — the real
//! answer needs the model's tokenizer, which is not available locally, and the
//! `count_tokens` endpoint would cost a network round trip per candidate split.
//! The approximation is deliberately pessimistic: English prose runs closer to
//! 4 characters per token, so budgeting at 3 leaves roughly a quarter of the
//! window as headroom for the preamble, the response, and any text that
//! tokenizes worse than prose (code, tables, non-Latin scripts).

/// Characters assumed per token when converting a token budget to a character
/// budget. Deliberately low; see the module docs.
pub const CHARS_PER_TOKEN: usize = 3;

/// Split `markdown` into chunks of at most `budget` characters.
///
/// Returns one chunk for a document that already fits. Never returns an empty
/// chunk, and never returns nothing for a document with content in it.
pub fn split(markdown: &str, budget: usize) -> Vec<String> {
    let budget = budget.max(1);
    if markdown.len() <= budget {
        return match markdown.trim().is_empty() {
            true => Vec::new(),
            false => vec![markdown.trim().to_string()],
        };
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for section in sections(markdown) {
        // A section that fits after the current one just joins it.
        if !current.is_empty() && current.len() + 1 + section.len() <= budget {
            current.push('\n');
            current.push_str(&section);
            continue;
        }
        push(&mut chunks, std::mem::take(&mut current));

        if section.len() <= budget {
            current = section;
            continue;
        }
        // One section is too big on its own: break it at paragraphs, and any
        // paragraph that is still too big at lines.
        for piece in break_up(&section, budget) {
            if !current.is_empty() && current.len() + 1 + piece.len() <= budget {
                current.push('\n');
                current.push_str(&piece);
            } else {
                push(&mut chunks, std::mem::take(&mut current));
                current = piece;
            }
        }
    }
    push(&mut chunks, current);
    chunks
}

fn push(chunks: &mut Vec<String>, chunk: String) {
    let chunk = chunk.trim();
    if !chunk.is_empty() {
        chunks.push(chunk.to_string());
    }
}

/// Cut the document at each heading that is not inside a fenced code block.
///
/// Each section carries its own heading, so a chunk that starts mid-document
/// still says what it is about.
fn sections(markdown: &str) -> Vec<String> {
    let mut sections = Vec::new();
    let mut current = String::new();
    let mut fence: Option<&str> = None;

    for line in markdown.lines() {
        let trimmed = line.trim_start();
        match fence {
            // Inside a fence: only the matching marker closes it. A ``` block
            // may legally contain ~~~ and vice versa.
            Some(marker) => {
                if trimmed.starts_with(marker) {
                    fence = None;
                }
            }
            None => {
                if let Some(marker) = opening_fence(trimmed) {
                    fence = Some(marker);
                } else if is_heading(trimmed) && !current.trim().is_empty() {
                    sections.push(std::mem::take(&mut current));
                }
            }
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        sections.push(current);
    }
    sections
}

fn opening_fence(trimmed: &str) -> Option<&'static str> {
    if trimmed.starts_with("```") {
        Some("```")
    } else if trimmed.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

/// An ATX heading: one to six `#` followed by a space, or a bare `#` line.
///
/// The space matters. `#hashtag` is not a heading, and treating it as one would
/// cut the document at arbitrary places.
fn is_heading(trimmed: &str) -> bool {
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes)
        && matches!(
            trimmed.as_bytes().get(hashes),
            None | Some(b' ') | Some(b'\t')
        )
}

/// Break one oversized section into pieces, at paragraphs where possible and
/// at lines where not.
fn break_up(section: &str, budget: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    for paragraph in paragraphs(section) {
        if paragraph.len() <= budget {
            pieces.push(paragraph);
            continue;
        }
        pieces.extend(hard_split(&paragraph, budget));
    }
    pieces
}

/// Split on blank lines, keeping fenced blocks whole.
fn paragraphs(section: &str) -> Vec<String> {
    let mut paragraphs = Vec::new();
    let mut current = String::new();
    let mut fence: Option<&str> = None;

    for line in section.lines() {
        let trimmed = line.trim_start();
        match fence {
            Some(marker) => {
                if trimmed.starts_with(marker) {
                    fence = None;
                }
            }
            None => {
                if let Some(marker) = opening_fence(trimmed) {
                    fence = Some(marker);
                } else if trimmed.is_empty() && !current.trim().is_empty() {
                    paragraphs.push(std::mem::take(&mut current));
                    continue;
                }
            }
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        paragraphs.push(current);
    }
    paragraphs
}

/// The last resort: cut at line boundaries, and inside a line that is itself
/// over budget, at character boundaries.
fn hard_split(paragraph: &str, budget: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut current = String::new();

    for line in paragraph.lines() {
        if !current.is_empty() && current.len() + 1 + line.len() > budget {
            pieces.push(std::mem::take(&mut current));
        }
        if line.len() <= budget {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
            continue;
        }
        // A single line longer than the budget — a minified table row, a base64
        // blob. Walk it in character steps so a multi-byte character is never
        // cut down the middle.
        let mut taken = 0;
        for character in line.chars() {
            if taken + character.len_utf8() > budget {
                pieces.push(std::mem::take(&mut current));
                taken = 0;
            }
            current.push(character);
            taken += character.len_utf8();
        }
    }
    if !current.trim().is_empty() {
        pieces.push(current);
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything that went in comes out, in order — the property the whole
    /// module exists for.
    fn assert_lossless(markdown: &str, chunks: &[String]) {
        let squash = |text: &str| {
            text.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        };
        assert_eq!(
            squash(&chunks.join("\n")),
            squash(markdown),
            "content was lost or reordered"
        );
    }

    #[test]
    fn a_document_that_fits_is_one_chunk() {
        let chunks = split("# Title\n\nA short body.\n", 1000);
        assert_eq!(chunks, ["# Title\n\nA short body."]);
    }

    #[test]
    fn an_empty_document_yields_no_chunks() {
        assert!(split("", 100).is_empty());
        assert!(split("   \n\n  \n", 100).is_empty());
    }

    #[test]
    fn splits_at_headings_and_keeps_them_with_their_sections() {
        let markdown = "# One\n\nfirst body text here\n\n# Two\n\nsecond body text here\n";
        let chunks = split(markdown, 30);
        assert!(chunks.len() >= 2, "{chunks:?}");
        assert!(chunks[0].starts_with("# One"));
        assert!(chunks.iter().any(|c| c.starts_with("# Two")));
        assert_lossless(markdown, &chunks);
    }

    #[test]
    fn a_fenced_block_is_never_cut() {
        // The heading inside the fence must not be treated as a heading.
        let markdown = "# Real\n\n```\n# not a heading\nmore code\n```\n\n# Also real\n\nbody\n";
        let chunks = split(markdown, 40);
        for chunk in &chunks {
            let fences = chunk.matches("```").count();
            assert_eq!(fences % 2, 0, "split inside a fence: {chunk:?}");
        }
        assert_lossless(markdown, &chunks);
    }

    #[test]
    fn a_tilde_fence_is_closed_only_by_tildes() {
        let markdown = "~~~\n```\n# inside\n```\n~~~\n\n# outside\n\nbody\n";
        let chunks = split(markdown, 200);
        assert_eq!(chunks.len(), 1, "{chunks:?}");
    }

    #[test]
    fn a_hashtag_is_not_a_heading() {
        let markdown = "#hashtag one\n#hashtag two\n#hashtag three\n";
        let chunks = split(markdown, 1000);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn one_huge_section_is_broken_at_paragraphs() {
        let markdown = format!(
            "# Big\n\n{}\n\n{}\n\n{}\n",
            "a".repeat(50),
            "b".repeat(50),
            "c".repeat(50)
        );
        let chunks = split(&markdown, 70);
        assert!(chunks.len() >= 3, "{chunks:?}");
        assert!(
            chunks.iter().all(|c| c.len() <= 70),
            "{:?}",
            chunks.iter().map(String::len).collect::<Vec<_>>()
        );
        assert_lossless(&markdown, &chunks);
    }

    #[test]
    fn one_huge_line_is_cut_on_character_boundaries() {
        // Multi-byte characters: a naive byte split would produce invalid UTF-8.
        let markdown = "é".repeat(100);
        let chunks = split(&markdown, 20);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.len() <= 20));
        assert_lossless(&markdown, &chunks);
    }

    #[test]
    fn a_zero_budget_does_not_hang_or_panic() {
        let chunks = split("# A\n\nbody\n", 0);
        assert!(!chunks.is_empty());
        assert_lossless("# A\n\nbody\n", &chunks);
    }

    #[test]
    fn sections_pack_together_up_to_the_budget() {
        // Three 6-character sections and a budget that fits two of them with
        // the joining newline, so the result must be two chunks, not three.
        let markdown = "# A\nx\n# B\ny\n# C\nz\n";
        let chunks = split(markdown, 14);
        assert_eq!(chunks.len(), 2, "sections should pack: {chunks:?}");
        assert_lossless(markdown, &chunks);
    }
}
