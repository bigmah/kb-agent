//! What a conversion reports about itself, while it runs and once it is done.

pub use pdf_inspector::PdfType;

/// How the OCR pass is going, delivered to [`Options::progress`].
///
/// A native-text run emits nothing at all: it finishes before a progress bar
/// would have been worth drawing.
///
/// [`Options::progress`]: crate::Options::progress
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Progress {
    /// OCR is about to begin on `total` pages, spread over `workers`
    /// processes. This is the exact count, unlike the estimate a [`Survey`]
    /// supports.
    OcrStarting { total: usize, workers: usize },
    /// `done` of `total` pages have been recognized.
    OcrPage { done: usize, total: usize },
    /// The OCR pass is over. Assembly and the caller's own writing follow.
    OcrFinished,
}

/// The cheap detection pass: what this PDF is, without extracting it.
///
/// Worth running before a conversion that could take minutes, so a caller can
/// say what is about to happen. See [`Options::survey`](crate::Options::survey).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Survey {
    /// Born-digital, scanned, or a mixture.
    pub pdf_type: PdfType,
    /// Pages in the document, whatever the page selection is.
    pub page_count: u32,
    /// How sure the detector is, 0–1.
    pub confidence: f32,
    /// 1-indexed pages with no usable text layer, narrowed to the selection.
    pub pages_needing_ocr: Vec<u32>,
}

impl Survey {
    /// Pages that [`Ocr`](crate::Ocr) `mode` would actually route to OCR,
    /// given this survey. The count the plan line wants.
    pub fn pages_to_ocr(&self, mode: crate::Ocr, selected: Option<&[u32]>) -> usize {
        match mode {
            crate::Ocr::Off => 0,
            crate::Ocr::Auto => self.pages_needing_ocr.len(),
            crate::Ocr::Force => selected.map_or(self.page_count as usize, <[u32]>::len),
        }
    }
}

/// A finished conversion.
///
/// The fields are the facts; [`summary`](Self::summary) and
/// [`notes`](Self::notes) are the same facts as the lines a command-line tool
/// would print, so a front end does not have to invent the phrasing.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Conversion {
    /// The Markdown, or `None` when nothing in the document could be turned
    /// into text — an empty document, or a fully scanned one with
    /// [`Ocr::Off`](crate::Ocr::Off).
    pub markdown: Option<String>,
    /// Pages in the document.
    pub page_count: u32,
    /// Pages actually converted; equal to `page_count` without a selection.
    pub converted_pages: u32,
    /// Pages whose Markdown came from OCR rather than the text layer.
    pub ocr_pages: u32,
    /// Pages that were OCR'd but whose native text fusion preferred anyway.
    /// A normal outcome on a page with a good text layer, not a failure.
    pub ocr_pages_kept_native: u32,
    /// Worker processes used. 1 means everything ran in this process.
    pub workers: usize,
    /// Page rendering time, summed across workers.
    pub render_ms: u64,
    /// Recognition time, summed across workers — so on a parallel run it
    /// exceeds `elapsed_ms` rather than fitting inside it.
    pub ocr_ms: u64,
    /// Wall-clock time for the whole conversion.
    pub elapsed_ms: u64,
    /// Pages holding a table.
    pub pages_with_tables: Vec<u32>,
    /// Pages laid out in columns.
    pub pages_with_columns: Vec<u32>,
    /// Pages where local OCR was weak enough that a hosted parser would do
    /// better.
    pub pages_low_confidence: Vec<u32>,
    /// Per-page warnings raised by the pipeline, in page order.
    pub warnings: Vec<String>,
}

impl Conversion {
    /// One line saying what happened, ready to print.
    pub fn summary(&self) -> String {
        let converted = if self.converted_pages == self.page_count {
            format!("{} page(s)", self.page_count)
        } else {
            format!("{} of {} page(s)", self.converted_pages, self.page_count)
        };
        if self.ocr_pages == 0 {
            return format!(
                "done: {converted} from the text layer in {}",
                format_duration(self.elapsed_ms)
            );
        }
        // Render and recognize are summed across workers, so on a parallel run
        // they exceed the wall-clock total. Say so rather than look wrong.
        let cpu = if self.workers > 1 {
            format!(" across {} workers", self.workers)
        } else {
            String::new()
        };
        format!(
            "done: {converted}, {} OCR'd — render {}, recognize {}{cpu}, total {}",
            self.ocr_pages,
            format_duration(self.render_ms),
            format_duration(self.ocr_ms),
            format_duration(self.elapsed_ms),
        )
    }

    /// Anything else worth saying: layout the converter found hard, pages OCR
    /// struggled with, and the first page warning.
    pub fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if !self.pages_with_tables.is_empty() || !self.pages_with_columns.is_empty() {
            notes.push(format!(
                "note: complex layout — tables on {} page(s), columns on {} page(s)",
                self.pages_with_tables.len(),
                self.pages_with_columns.len()
            ));
        }
        if !self.pages_low_confidence.is_empty() {
            notes.push(format!(
                "note: local OCR was low-confidence on {} — a hosted parser would do better",
                summarize_pages(&self.pages_low_confidence)
            ));
        }
        if self.ocr_pages_kept_native > 0 {
            notes.push(format!(
                "note: {} OCR'd page(s) kept their native text — OCR did not improve on it",
                self.ocr_pages_kept_native
            ));
        }
        if let Some(first) = self.warnings.first() {
            notes.push(match self.warnings.len() {
                1 => format!("warning: {first}"),
                n => format!("warning: {first} (and {} more page warning(s))", n - 1),
            });
        }
        notes
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

/// Render a page list without dumping hundreds of numbers into a note.
pub fn summarize_pages(pages: &[u32]) -> String {
    match pages {
        [] => "none".to_string(),
        [first, .., last] if pages.len() > 8 => {
            format!("{} page(s), {first}–{last}", pages.len())
        }
        _ => pages
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(","),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversion() -> Conversion {
        Conversion {
            markdown: Some("text".to_string()),
            page_count: 10,
            converted_pages: 10,
            ocr_pages: 0,
            ocr_pages_kept_native: 0,
            workers: 1,
            render_ms: 0,
            ocr_ms: 0,
            elapsed_ms: 1_500,
            pages_with_tables: Vec::new(),
            pages_with_columns: Vec::new(),
            pages_low_confidence: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn a_native_run_summarizes_without_ocr_timings() {
        assert_eq!(
            conversion().summary(),
            "done: 10 page(s) from the text layer in 1.5 s"
        );
    }

    #[test]
    fn a_parallel_run_says_the_timings_are_summed() {
        let summary = Conversion {
            ocr_pages: 10,
            workers: 4,
            render_ms: 2_000,
            ocr_ms: 240_000,
            elapsed_ms: 61_000,
            ..conversion()
        }
        .summary();
        assert_eq!(
            summary,
            "done: 10 page(s), 10 OCR'd — render 2.0 s, recognize 4 min 0 s across 4 workers, \
             total 1 min 1 s"
        );
    }

    #[test]
    fn a_partial_selection_says_so() {
        let summary = Conversion {
            converted_pages: 3,
            ..conversion()
        }
        .summary();
        assert!(summary.starts_with("done: 3 of 10 page(s)"), "{summary}");
    }

    #[test]
    fn extra_warnings_are_counted_not_listed() {
        let notes = Conversion {
            warnings: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            ..conversion()
        }
        .notes();
        assert_eq!(notes, ["warning: a (and 2 more page warning(s))"]);
    }

    #[test]
    fn long_page_lists_collapse_to_a_range() {
        assert_eq!(summarize_pages(&[]), "none");
        assert_eq!(summarize_pages(&[3, 4, 9]), "3,4,9");
        assert_eq!(
            summarize_pages(&[1, 2, 3, 4, 5, 6, 7, 8, 9]),
            "9 page(s), 1–9"
        );
    }
}
