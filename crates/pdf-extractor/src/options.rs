//! What a conversion can be asked to do, and how much machine to do it with.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pdf_inspector::{MarkdownOptions, MarkdownProfile};

use crate::Progress;

/// Default render resolution for OCR.
///
/// 150 DPI is roughly four times faster but conflates headings with body text
/// on a mediocre scan, which costs the document its structure. Correctness at
/// the slower setting is the better default for a library whose whole point is
/// that the caller does not have to tune it.
pub const DEFAULT_OCR_DPI: f32 = 300.0;

/// How much of the document the OCR engine should look at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ocr {
    /// Never OCR. A fully scanned document yields nothing.
    Off,
    /// OCR only the pages whose text layer is missing or unusable.
    #[default]
    Auto,
    /// OCR every page, ignoring any text layer.
    Force,
}

/// A conversion, configured.
///
/// Every field has a default that suits an unknown PDF, so
/// `Options::new().convert(path)` is the whole API for most callers — see the
/// free functions in [the crate root](crate) for even less than that.
#[derive(Clone)]
pub struct Options {
    pub(crate) pages: Option<Vec<u32>>,
    pub(crate) page_markers: bool,
    pub(crate) images: bool,
    pub(crate) keep_furniture: bool,
    pub(crate) compact: bool,
    pub(crate) password: Option<String>,
    pub(crate) ocr: Ocr,
    pub(crate) ocr_dpi: f32,
    pub(crate) ocr_min_confidence: f32,
    pub(crate) ocr_model_dir: Option<PathBuf>,
    pub(crate) ocr_offline: bool,
    pub(crate) ocr_jobs: Option<usize>,
    /// Not encoded into a worker job: children report through their pipe, and
    /// the parent is the one that calls this.
    pub(crate) progress: Option<Arc<dyn Fn(Progress) + Send + Sync>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            pages: None,
            page_markers: false,
            images: false,
            keep_furniture: false,
            compact: false,
            password: None,
            ocr: Ocr::Auto,
            ocr_dpi: DEFAULT_OCR_DPI,
            ocr_min_confidence: 0.0,
            ocr_model_dir: None,
            ocr_offline: false,
            ocr_jobs: None,
            progress: None,
        }
    }
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("pages", &self.pages)
            .field("page_markers", &self.page_markers)
            .field("images", &self.images)
            .field("keep_furniture", &self.keep_furniture)
            .field("compact", &self.compact)
            // A password does not belong in a log line.
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("ocr", &self.ocr)
            .field("ocr_dpi", &self.ocr_dpi)
            .field("ocr_min_confidence", &self.ocr_min_confidence)
            .field("ocr_model_dir", &self.ocr_model_dir)
            .field("ocr_offline", &self.ocr_offline)
            .field("ocr_jobs", &self.ocr_jobs)
            .field("progress", &self.progress.as_ref().map(|_| "<callback>"))
            .finish()
    }
}

impl Options {
    pub fn new() -> Self {
        Self::default()
    }

    /// Convert only these 1-indexed pages. Order and duplicates do not matter;
    /// the selection is treated as a set.
    pub fn pages(mut self, pages: impl IntoIterator<Item = u32>) -> Self {
        let mut pages: Vec<u32> = pages.into_iter().collect();
        pages.sort_unstable();
        pages.dedup();
        self.pages = Some(pages);
        self
    }

    /// Insert `<!-- Page N -->` markers between pages.
    pub fn page_markers(mut self, yes: bool) -> Self {
        self.page_markers = yes;
        self
    }

    /// Include `![Image: ...]` placeholders.
    pub fn images(mut self, yes: bool) -> Self {
        self.images = yes;
        self
    }

    /// Keep repeated headers, footers and page numbers instead of stripping
    /// them.
    pub fn keep_furniture(mut self, yes: bool) -> Self {
        self.keep_furniture = yes;
        self
    }

    /// Emit token-efficient Markdown instead of preserving source fidelity.
    pub fn compact(mut self, yes: bool) -> Self {
        self.compact = yes;
        self
    }

    /// Password for an encrypted PDF.
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// How much of the document to OCR. Defaults to [`Ocr::Auto`].
    pub fn ocr(mut self, ocr: Ocr) -> Self {
        self.ocr = ocr;
        self
    }

    /// Page render resolution for OCR. Defaults to [`DEFAULT_OCR_DPI`].
    pub fn ocr_dpi(mut self, dpi: f32) -> Self {
        self.ocr_dpi = dpi;
        self
    }

    /// Drop OCR spans scoring below this, on a 0–1 scale.
    pub fn ocr_min_confidence(mut self, confidence: f32) -> Self {
        self.ocr_min_confidence = confidence;
        self
    }

    /// Take the OCR models from here instead of the download cache.
    pub fn ocr_model_dir(mut self, directory: impl Into<PathBuf>) -> Self {
        self.ocr_model_dir = Some(directory.into());
        self
    }

    /// Fail rather than download a missing model set.
    pub fn ocr_offline(mut self, yes: bool) -> Self {
        self.ocr_offline = yes;
        self
    }

    /// How many worker processes to spread OCR across. `None` — the default —
    /// sizes it to the machine; see [`Options::worker_count`].
    ///
    /// Requires the host binary to call [`run_worker_if_spawned`] at the top of
    /// `main`. A value of 1 keeps everything in this process and needs nothing.
    ///
    /// [`run_worker_if_spawned`]: crate::run_worker_if_spawned
    pub fn ocr_jobs(mut self, jobs: Option<usize>) -> Self {
        self.ocr_jobs = jobs.map(|jobs| jobs.max(1));
        self
    }

    /// Be told how the OCR pass is going. It can run for many minutes, and the
    /// callback is the only thing this library says while it does.
    ///
    /// Called from the thread driving the conversion, and from nowhere else.
    pub fn progress(mut self, on_progress: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        self.progress = Some(Arc::new(on_progress));
        self
    }

    /// Markdown formatting, shared by native and OCR page assembly.
    pub(crate) fn markdown_options(&self) -> MarkdownOptions {
        MarkdownOptions {
            profile: if self.compact {
                MarkdownProfile::Compact
            } else {
                MarkdownProfile::Fidelity
            },
            include_images: self.images,
            include_page_numbers: self.page_markers,
            strip_headers_footers: !self.keep_furniture,
            remove_page_numbers: !self.keep_furniture,
            ..MarkdownOptions::default()
        }
    }

    /// How much of a routed page the OCR pass should trust.
    pub(crate) fn ocr_mode(&self) -> pdf_inspector::vision::OcrMode {
        match self.ocr {
            Ocr::Force => pdf_inspector::vision::OcrMode::Force,
            _ => pdf_inspector::vision::OcrMode::Auto,
        }
    }

    pub(crate) fn emit(&self, event: Progress) {
        if let Some(callback) = &self.progress {
            callback(event);
        }
    }

    pub(crate) fn validate(&self, input: &Path) -> Result<(), crate::Error> {
        if let Some(pages) = &self.pages {
            if pages.is_empty() {
                return Err(crate::Error::Options(
                    "the page selection is empty".to_string(),
                ));
            }
            if pages[0] == 0 {
                return Err(crate::Error::Options(
                    "pages are 1-indexed, 0 is not a page".to_string(),
                ));
            }
        }
        if !(self.ocr_dpi.is_finite() && self.ocr_dpi > 0.0) {
            return Err(crate::Error::Options(format!(
                "the OCR resolution must be a positive number, got {}",
                self.ocr_dpi
            )));
        }
        if !(0.0..=1.0).contains(&self.ocr_min_confidence) {
            return Err(crate::Error::Options(format!(
                "the minimum OCR confidence must be between 0 and 1, got {}",
                self.ocr_min_confidence
            )));
        }
        // Checked here so a missing or unreadable input fails immediately with
        // the operating system's own words, rather than several seconds into a
        // pipeline that has to invent an explanation.
        let metadata =
            std::fs::metadata(input).map_err(|error| crate::Error::io("read", input, error))?;
        if !metadata.is_file() {
            return Err(crate::Error::io(
                "read",
                input,
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"),
            ));
        }
        Ok(())
    }

    /// Worker processes this configuration would actually use for `routed`
    /// pages — the answer callers want *before* starting, to say what is about
    /// to happen.
    ///
    /// A worker costs about half a second to start — it builds its own ONNX
    /// sessions — so a handful of pages is not worth splitting, and asking for
    /// more workers than there is work for just multiplies that cost.
    pub fn worker_count(&self, routed: usize) -> usize {
        if routed == 0 {
            return 1;
        }
        let requested = self.ocr_jobs.unwrap_or_else(default_ocr_jobs);
        requested.min(routed.div_ceil(MINIMUM_PAGES_PER_JOB)).max(1)
    }
}

/// Below this many pages a worker spends more time starting than recognizing.
const MINIMUM_PAGES_PER_JOB: usize = 3;

/// Workers to use when the caller does not say.
///
/// Each one drives roughly two cores of ONNX work, so cores set the ceiling on
/// what more workers can buy. Measured on a 14-core M4 Pro over a 24-page
/// slice: 1 worker 37 s, 4 workers 12 s, 6 workers 11 s, 8 workers 10 s — the
/// gain is nearly all in the first few.
///
/// Memory is the harder limit, and the reason this is not just a core count. A
/// worker resides at about 2 GB: `pdf-inspector` sizes its engine to the whole
/// machine and builds three detector/recognizer pairs, of which a one-page
/// request only ever uses the first, and nothing exposes that. Seven workers
/// peaked at 14.9 GB on a 595-page book. Sized on cores alone this would put
/// four workers and 8 GB on an 8 GB laptop and swap it into the ground, so the
/// budget below wins whenever it is the smaller of the two.
fn default_ocr_jobs() -> usize {
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);

    // Two thirds of RAM, at 2 GB a worker. A machine that will not say how much
    // memory it has is trusted with the core count alone rather than pinned to
    // one worker, since that is the pre-existing behaviour.
    let affordable = total_memory_bytes()
        .map(|total| (total / 3 * 2 / (2 * 1024 * 1024 * 1024)) as usize)
        .unwrap_or(usize::MAX);

    cores.div_ceil(2).min(affordable).clamp(1, 8)
}

/// Physical memory, if this platform will say. Probed once — it is only ever
/// consulted on a run that is about to spend minutes in OCR.
fn total_memory_bytes() -> Option<u64> {
    static TOTAL: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *TOTAL.get_or_init(|| {
        if cfg!(any(target_os = "macos", target_os = "ios")) {
            let output = std::process::Command::new("sysctl")
                .args(["-n", "hw.memsize"])
                .output()
                .ok()?;
            return String::from_utf8(output.stdout).ok()?.trim().parse().ok();
        }
        // MemTotal is in kibibytes: "MemTotal:       16316532 kB".
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kibibytes: u64 = meminfo
            .lines()
            .find_map(|line| line.strip_prefix("MemTotal:"))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()?;
        Some(kibibytes * 1024)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_selection_is_a_set() {
        let options = Options::new().pages([9, 1, 9, 4]);
        assert_eq!(options.pages.as_deref(), Some(&[1, 4, 9][..]));
    }

    #[test]
    fn workers_never_outnumber_the_work() {
        let options = Options::new().ocr_jobs(Some(8));
        assert_eq!(options.worker_count(0), 1);
        assert_eq!(options.worker_count(2), 1);
        assert_eq!(options.worker_count(6), 2);
        assert_eq!(options.worker_count(600), 8);
    }

    #[test]
    fn a_password_stays_out_of_debug_output() {
        let rendered = format!("{:?}", Options::new().password("hunter2"));
        assert!(!rendered.contains("hunter2"), "{rendered}");
    }
}
