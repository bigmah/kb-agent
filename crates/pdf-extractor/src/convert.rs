//! The extraction pipeline: native text layer first, OCR for what it misses.
//!
//! `pdf-inspector` extracts every page's text layer, decides page by page which
//! pages that layer failed on — image-only pages, vector-drawn text, garbled
//! font encodings — and renders and recognizes only those. PDFium and the
//! PP-OCRv6 Small model on ONNX Runtime are loaded lazily, so a PDF with a good
//! text layer never touches either, and a scanned one needs no different call.
//!
//! # Why the OCR runs one page at a time
//!
//! `pdf-inspector` 1.17.0 deadlocks when it is asked to recognize more than one
//! page in a single call on a machine with 8 or more cores. Its `OarOcrEngine`
//! gives each of its 1–3 workers a private ONNX session behind a mutex and maps
//! worker to `rayon::current_thread_index()`, so a thread only ever touches its
//! own session. But `oar-ocr` runs its CTC decode (`argmax_predictions`) on
//! rayon *while holding that session mutex*, and the nested `join` lets the
//! thread steal a sibling page job off the same pool. The stolen page resolves
//! to the same thread index, so it waits on a mutex its own stack already
//! holds, and `std::sync::Mutex` is not reentrant. Every worker then parks
//! forever.
//!
//! Observed on a 595-page scan: ~8 CPU-minutes in, all 30 threads asleep and
//! 4 GB resident, with no further progress. Verified from the stack: one thread
//! inside `infer_first_output_f32` → `argmax_predictions` → rayon `join` →
//! stolen `recognize_page` → `Mutex::lock`.
//!
//! `OarOcrEngine::recognize` bypasses its pool entirely when handed a single
//! page, so routing exactly one page per call is the one shape that cannot
//! deadlock. It gives up nothing in the output — the OCR fusion path does not
//! do cross-page analysis, and a batched and a page-at-a-time run of the same
//! 20 pages produced byte-identical text. The cross-page work that does matter
//! — running headers, footers, page numbers — happens in the first, native
//! pass over the whole document, which is left batched.
//!
//! Both crates are at their latest published versions (`pdf-inspector` 1.17.0,
//! `oar-ocr` 0.9.2), so there is no upstream fix to take yet.
//!
//! # Why it then runs in several processes
//!
//! One page per call is safe but slow, and not because the work is hard: a
//! single-page request is served by worker 0 alone, whose sessions are built
//! with two intra-op threads. A 24-page slice took 37 s of wall time for 73 s
//! of CPU — two of fourteen cores busy. The remaining twelve are recovered by
//! [`crate::worker`], which fans the routed pages across child processes that
//! share no engine and no locks. Same one-page-per-call shape, same output,
//! about a third of the wall time. `Options::ocr_jobs(Some(1))` restores the
//! single-process loop, and so does a host that has not opted into the
//! fan-out — see [`crate::run_worker_if_spawned`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

use pdf_inspector::vision::{
    FusedPageMarkdown, ModelDownloadPolicy, OcrMode, OcrOptions, OcrPdfOptions, OcrPdfResult,
    OcrPipelineError, PageContentSource, RenderOptions, process_pdf_with_ocr_mem,
};
use pdf_inspector::{PdfError, PdfOptions, ProcessMode, process_pdf_with_options};

use crate::error::error_chain;
use crate::report::{Conversion, Progress, Survey};
use crate::{Error, Ocr, Options, runtime, worker};

/// One finished page, from either the native pass or OCR.
///
/// This exists rather than `FusedPageMarkdown` because a page can arrive from
/// another process, where only these fields survive the trip. Keeping both
/// paths on one type means the assembly and the warning counts cannot drift
/// apart depending on how the run was parallelized.
pub(crate) struct PageOutput {
    pub(crate) page_number: u32,
    pub(crate) markdown: String,
    /// Whether fusion actually preferred the OCR text for this page.
    pub(crate) is_ocr: bool,
    pub(crate) warnings: Vec<String>,
}

impl PageOutput {
    fn from_fused(page: &FusedPageMarkdown) -> Self {
        Self {
            page_number: page.page_number,
            markdown: page.markdown.clone(),
            is_ocr: page.provenance.source == PageContentSource::Ocr,
            warnings: page.provenance.warnings.clone(),
        }
    }
}

/// Everything an OCR pass produces, whether it ran here or in a child.
#[derive(Default)]
pub(crate) struct Harvest {
    pub(crate) pages: BTreeMap<u32, PageOutput>,
    pub(crate) render_ms: u64,
    pub(crate) ocr_ms: u64,
    pub(crate) hosted: BTreeSet<u32>,
    pub(crate) tables: BTreeSet<u32>,
    pub(crate) columns: BTreeSet<u32>,
}

impl Harvest {
    /// Fold another pass's results in. Timings add up across processes because
    /// they are CPU-side stage measurements, not wall clock — the summary
    /// reports them alongside a wall-clock total, which is what shows the win.
    pub(crate) fn absorb(&mut self, other: Harvest) {
        self.pages.extend(other.pages);
        self.render_ms += other.render_ms;
        self.ocr_ms += other.ocr_ms;
        self.hosted.extend(other.hosted);
        self.tables.extend(other.tables);
        self.columns.extend(other.columns);
    }
}

/// Detect the document's type and which pages have no usable text layer,
/// without extracting anything.
pub(crate) fn survey(input: &Path, options: &Options) -> Result<Survey, Error> {
    options.validate(input)?;
    runtime::init();

    let mut pdf = PdfOptions::new().mode(ProcessMode::DetectOnly);
    if let Some(pages) = options.pages.clone() {
        pdf = pdf.pages(pages);
    }
    if let Some(password) = options.password.clone() {
        pdf = pdf.password(password);
    }
    let result = process_pdf_with_options(input, pdf).map_err(from_pdf_error)?;

    // Detection reports on the whole document even when a page selection
    // narrows the run, so intersect here and let everything downstream speak
    // only about the pages that will actually be converted.
    let mut pages_needing_ocr = result.pages_needing_ocr;
    if let Some(selected) = &options.pages {
        let selected: BTreeSet<u32> = selected.iter().copied().collect();
        pages_needing_ocr.retain(|page| selected.contains(page));
    }

    Ok(Survey {
        pdf_type: result.pdf_type,
        page_count: result.page_count,
        confidence: result.confidence,
        pages_needing_ocr,
    })
}

pub(crate) fn convert(input: &Path, options: &Options) -> Result<Conversion, Error> {
    options.validate(input)?;
    runtime::init();

    let started = Instant::now();
    let bytes = std::fs::read(input).map_err(|error| Error::io("read", input, error))?;

    // Pass one, batched: the text layer for the whole selection. This is where
    // the library's cross-page work happens, and it reports which pages the
    // text layer could not serve.
    let base = extract(options, &bytes, OcrMode::Off, None)?;

    let routed: Vec<u32> = match options.ocr {
        Ocr::Off => Vec::new(),
        Ocr::Auto => base.pages_recommended_for_ocr.clone(),
        Ocr::Force => base.pages.iter().map(|page| page.page_number).collect(),
    };

    // Pass two, one page per call, spread over as many processes as the machine
    // and the page count justify. See the module docs for both halves of that.
    let mode = options.ocr_mode();
    let jobs = options.worker_count(routed.len());
    let total = routed.len();
    if total > 0 {
        options.emit(Progress::OcrStarting {
            total,
            workers: jobs,
        });
    }

    let mut harvest = if jobs > 1 {
        worker::run_parallel(input, options, &routed, jobs)?
    } else {
        let mut harvest = Harvest::default();
        for (done, page) in routed.iter().enumerate() {
            options.emit(Progress::OcrPage {
                done: done + 1,
                total,
            });
            ocr_one_page(options, &bytes, mode, *page, &mut harvest)?;
        }
        harvest
    };
    if total > 0 {
        options.emit(Progress::OcrFinished);
    }

    // Every page in selection order, preferring the OCR'd version where there
    // is one. Taking provenance from the same place keeps the warnings honest:
    // an OCR'd page must not still carry the native pass's "recommended for
    // OCR but not processed".
    let pages: Vec<PageOutput> = base
        .pages
        .iter()
        .map(|page| {
            harvest
                .pages
                .remove(&page.page_number)
                .unwrap_or_else(|| PageOutput::from_fused(page))
        })
        .collect();

    let markdown = assemble(&pages, options.page_markers);
    let ocr_pages = pages.iter().filter(|page| page.is_ocr).count() as u32;

    harvest
        .tables
        .extend(base.pages_with_tables.iter().copied());
    harvest
        .columns
        .extend(base.pages_with_columns.iter().copied());

    Ok(Conversion {
        markdown: Some(markdown).filter(|markdown| !markdown.trim().is_empty()),
        page_count: base.page_count,
        converted_pages: pages.len() as u32,
        ocr_pages,
        // Pages that were OCR'd but whose native text the fusion step preferred
        // anyway. That is a normal outcome on a page with a good text layer,
        // not a failure — a page where OCR genuinely came back empty reports
        // itself through the per-page warnings.
        ocr_pages_kept_native: (routed.len() as u32).saturating_sub(ocr_pages),
        workers: jobs,
        render_ms: harvest.render_ms,
        ocr_ms: harvest.ocr_ms,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        pages_with_tables: harvest.tables.into_iter().collect(),
        pages_with_columns: harvest.columns.into_iter().collect(),
        pages_low_confidence: harvest.hosted.into_iter().collect(),
        warnings: pages
            .iter()
            .flat_map(|page| page.warnings.iter().cloned())
            .collect(),
    })
}

/// OCR exactly one page and fold what came back into `harvest`.
///
/// Both the single-process loop and every child process go through here, so
/// there is one definition of what OCRing a page means and what it records.
pub(crate) fn ocr_one_page(
    options: &Options,
    bytes: &[u8],
    mode: OcrMode,
    page: u32,
    harvest: &mut Harvest,
) -> Result<(), Error> {
    let single = extract(options, bytes, mode, Some(page))?;
    harvest.render_ms += single.render_time_ms;
    harvest.ocr_ms += single.ocr_time_ms;
    harvest.hosted.extend(single.pages_recommending_hosted);
    harvest.tables.extend(single.pages_with_tables);
    harvest.columns.extend(single.pages_with_columns);
    if let Some(fused) = single.pages.iter().find(|fused| fused.page_number == page) {
        harvest.pages.insert(page, PageOutput::from_fused(fused));
    }
    Ok(())
}

/// One pass over the document, either native-only or OCR for a single page.
fn extract(
    options: &Options,
    bytes: &[u8],
    mode: OcrMode,
    page: Option<u32>,
) -> Result<OcrPdfResult, Error> {
    let mut ocr = OcrOptions::new()
        .mode(mode)
        .minimum_confidence(options.ocr_min_confidence);
    if let Some(directory) = &options.ocr_model_dir {
        ocr = ocr.model_directory(directory.clone());
    }
    if options.ocr_offline {
        ocr = ocr.model_downloads(ModelDownloadPolicy::Offline);
    }

    let mut pdf = OcrPdfOptions::new()
        .render(RenderOptions::new().dpi(options.ocr_dpi))
        .ocr(ocr)
        .markdown(options.markdown_options());
    match page {
        Some(page) => pdf = pdf.page_numbers([page]),
        None => {
            if let Some(pages) = options.pages.clone() {
                pdf = pdf.page_numbers(pages);
            }
        }
    }
    if let Some(password) = options.password.clone() {
        pdf = pdf.password(password);
    }

    process_pdf_with_ocr_mem(bytes, pdf).map_err(explain)
}

/// Join per-page Markdown exactly the way `pdf-inspector` would have.
fn assemble(pages: &[PageOutput], include_page_numbers: bool) -> String {
    let mut document = String::new();
    for (index, page) in pages.iter().enumerate() {
        if index > 0 {
            document.push_str("\n\n");
        }
        if include_page_numbers {
            document.push_str(&format!("<!-- Page {} -->\n\n", page.page_number));
        }
        document.push_str(page.markdown.trim());
    }
    if !document.is_empty() {
        document.push('\n');
    }
    document
}

fn from_pdf_error(error: PdfError) -> Error {
    match error {
        PdfError::Encrypted => Error::Encrypted,
        other => Error::Pdf {
            message: error_chain(&other),
            hint: None,
        },
    }
}

/// Turn a pipeline failure into something actionable.
///
/// The two interesting cases are a missing shared library — which `build.rs`
/// normally provisions, so reaching here means that step was skipped or failed
/// — and a model set that cannot be downloaded.
fn explain(error: OcrPipelineError) -> Error {
    if let OcrPipelineError::Pdf(PdfError::Encrypted) = error {
        return Error::Encrypted;
    }

    let message = error_chain(&error);
    let missing_library = message.contains("PDFIUM_LIB_PATH")
        || message.contains("ORT_DYLIB_PATH")
        || message.contains("failed to load PDFium")
        || message.contains("failed to load ONNX Runtime");

    let hint = if missing_library {
        Some(format!(
            "OCR needs the PDFium and ONNX Runtime shared libraries. Run \
             crates/pdf-extractor/scripts/fetch-ocr-runtime.sh to fetch them, or turn OCR \
             off to extract only the text layer.\n       looked in: {}",
            runtime::searched_locations().join(", ")
        ))
    } else if message.contains("downloads are disabled") {
        Some(
            "allow model downloads, or point the model directory at a complete model set"
                .to_string(),
        )
    } else {
        None
    };

    Error::Pdf { message, hint }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(number: u32, markdown: &str) -> PageOutput {
        PageOutput {
            page_number: number,
            markdown: markdown.to_string(),
            is_ocr: false,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn assembly_separates_pages_and_ends_with_one_newline() {
        let pages = [page(1, "  first  "), page(2, "second\n")];
        assert_eq!(assemble(&pages, false), "first\n\nsecond\n");
    }

    #[test]
    fn markers_carry_the_real_page_number() {
        let pages = [page(4, "text")];
        assert_eq!(assemble(&pages, true), "<!-- Page 4 -->\n\ntext\n");
    }

    #[test]
    fn an_empty_document_assembles_to_nothing() {
        assert_eq!(assemble(&[], true), "");
    }
}
