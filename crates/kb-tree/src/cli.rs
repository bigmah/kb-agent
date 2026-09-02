//! The command line: parsing it, and saying what a run is about to do.

use std::path::PathBuf;

use pdf_extractor::{DEFAULT_OCR_DPI, Ocr, Options, Survey};

pub const USAGE: &str = "\
kb-tree — convert a PDF into a Markdown file

USAGE:
    kb-tree <input.pdf> [OPTIONS]

OPTIONS:
    -o, --output <file>   Write Markdown here (default: <input> with a .md extension)
        --stdout          Write Markdown to stdout instead of a file
        --pages <list>    Only convert these 1-indexed pages, e.g. 1,4,7-9
        --page-markers    Insert <!-- Page N --> markers between pages
        --images          Include ![Image: ...] placeholders
        --keep-furniture  Keep repeated headers/footers and page numbers
        --compact         Token-efficient output instead of source fidelity
        --password <pw>   Password for an encrypted PDF
    -h, --help            Show this help
    -V, --version         Show the version

OCR runs automatically on any page whose text layer is missing or unusable.
It is slow — budget a couple of seconds per such page — but needs no setup:
        --ocr <mode>      auto (default; only the pages that need it),
                          off (text layer only, never OCR),
                          or force (every page, ignoring any text layer)
        --ocr-dpi <n>     Page render resolution (default: 300)
        --ocr-min-confidence <n>   Drop OCR spans below this 0–1 score (default: 0)
        --ocr-model-dir <dir>      Use models from here instead of the cache
        --ocr-offline     Fail rather than download missing models
        --ocr-jobs <n>    OCR worker processes (default: sized to this machine).
                          Each holds its own model sessions, so lower this if
                          memory is tight; 1 runs everything in this process.

The input PDF is read only and left untouched.";

/// A parsed command line: where the Markdown goes, and everything the library
/// needs to produce it.
pub struct Invocation {
    pub input: PathBuf,
    pub destination: Destination,
    pub options: Options,
    /// Kept out of `options` because the plan line needs it before conversion
    /// starts, and `Options` deliberately does not expose its own fields.
    pub ocr: Ocr,
    pub ocr_dpi: f32,
    pub pages: Option<Vec<u32>>,
}

pub enum Destination {
    /// The path given, or the input's name with a `.md` extension.
    File(PathBuf),
    Stdout,
}

/// What `parse` decided to do, since two of the three outcomes are not a run.
pub enum Parsed {
    Run(Box<Invocation>),
    Help,
    Version,
}

pub fn parse(argv: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut input: Option<PathBuf> = None;
    let mut output = None;
    let mut to_stdout = false;
    let mut pages = None;
    let mut page_markers = false;
    let mut images = false;
    let mut keep_furniture = false;
    let mut compact = false;
    let mut password = None;
    let mut ocr = Ocr::Auto;
    let mut ocr_dpi = DEFAULT_OCR_DPI;
    let mut ocr_min_confidence = 0.0;
    let mut ocr_model_dir = None;
    let mut ocr_offline = false;
    let mut ocr_jobs = None;

    let mut argv = argv.peekable();
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            "-o" | "--output" => output = Some(PathBuf::from(value_for(&arg, &mut argv)?)),
            "--stdout" => to_stdout = true,
            "--pages" => pages = Some(parse_pages(&value_for(&arg, &mut argv)?)?),
            "--page-markers" => page_markers = true,
            "--images" => images = true,
            "--keep-furniture" => keep_furniture = true,
            "--compact" => compact = true,
            "--password" => password = Some(value_for(&arg, &mut argv)?),
            "--ocr" => ocr = parse_ocr_mode(&value_for(&arg, &mut argv)?)?,
            "--ocr-dpi" => ocr_dpi = parse_positive(&arg, &value_for(&arg, &mut argv)?)?,
            "--ocr-min-confidence" => {
                ocr_min_confidence = parse_confidence(&value_for(&arg, &mut argv)?)?;
            }
            "--ocr-model-dir" => ocr_model_dir = Some(PathBuf::from(value_for(&arg, &mut argv)?)),
            "--ocr-offline" => ocr_offline = true,
            "--ocr-jobs" => ocr_jobs = Some(parse_jobs(&value_for(&arg, &mut argv)?)?),
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown option {other}\n\n{USAGE}"));
            }
            other => {
                if input.replace(PathBuf::from(other)).is_some() {
                    return Err(format!("unexpected extra argument {other}\n\n{USAGE}"));
                }
            }
        }
    }

    let input = input.ok_or_else(|| format!("no input PDF given\n\n{USAGE}"))?;

    let mut options = Options::new()
        .page_markers(page_markers)
        .images(images)
        .keep_furniture(keep_furniture)
        .compact(compact)
        .ocr(ocr)
        .ocr_dpi(ocr_dpi)
        .ocr_min_confidence(ocr_min_confidence)
        .ocr_offline(ocr_offline)
        .ocr_jobs(ocr_jobs);
    if let Some(pages) = pages.clone() {
        options = options.pages(pages);
    }
    if let Some(password) = password {
        options = options.password(password);
    }
    if let Some(directory) = ocr_model_dir {
        options = options.ocr_model_dir(directory);
    }

    let destination = if to_stdout {
        Destination::Stdout
    } else {
        Destination::File(output.unwrap_or_else(|| pdf_extractor::default_output(&input)))
    };

    Ok(Parsed::Run(Box::new(Invocation {
        input,
        destination,
        options,
        ocr,
        ocr_dpi,
        pages,
    })))
}

impl Invocation {
    /// Say what the run is about to do, before it goes quiet for possibly
    /// minutes. Everything here is derived from the cheap detection pass.
    pub fn announce(&self, survey: &Survey) {
        eprintln!(
            "{}: {} page(s), {:?} (confidence {:.2})",
            self.input.display(),
            survey.page_count,
            survey.pdf_type,
            survey.confidence
        );

        if self.ocr == Ocr::Off {
            let needing = survey.pages_needing_ocr.len();
            if needing > 0 {
                eprintln!(
                    "warning: {needing} page(s) have no usable text layer and --ocr off \
                     skips them; drop the flag to recover them"
                );
            }
            return;
        }

        let ocr_pages = survey.pages_to_ocr(self.ocr, self.pages.as_deref());
        if ocr_pages == 0 {
            return;
        }

        let jobs = self.options.worker_count(ocr_pages);
        let workers = match jobs {
            1 => "on CPU".to_string(),
            n => format!("on CPU across {n} workers"),
        };
        eprintln!(
            "OCR: {ocr_pages} page(s) at {:.0} DPI {workers}, {} — the ~31 MB model set \
             downloads on first use",
            self.ocr_dpi,
            format_estimate(estimated_ocr_ms(ocr_pages, self.ocr_dpi) / jobs as u64)
        );
    }
}

/// A deliberately rough OCR time estimate, for the one-line plan.
///
/// Recognition is CPU-bound and scales with pixel count, so cost grows with the
/// square of the DPI. The constant is an order-of-magnitude figure measured on
/// ordinary scanned body text, not a benchmark — machines differ by several
/// times either way, which is why [`format_estimate`] rounds hard. It exists
/// so "this will take a while" reads as minutes or hours rather than as a
/// number of pages.
fn estimated_ocr_ms(pages: usize, dpi: f32) -> u64 {
    const MS_PER_PAGE_AT_150_DPI: f64 = 400.0;
    let scale = (f64::from(dpi) / 150.0).powi(2);
    (pages as f64 * MS_PER_PAGE_AT_150_DPI * scale) as u64
}

/// Render an estimate at a precision it can actually support.
fn format_estimate(ms: u64) -> String {
    match ms / 60_000 {
        0 => "under a minute".to_string(),
        minutes @ 1..=90 => format!("about {minutes} min"),
        _ => format!("about {:.1} hours", ms as f64 / 3_600_000.0),
    }
}

fn value_for(flag: &str, argv: &mut impl Iterator<Item = String>) -> Result<String, String> {
    argv.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn parse_jobs(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!(
            "--ocr-jobs needs a count of 1 or more, got {value:?}"
        )),
    }
}

fn parse_ocr_mode(value: &str) -> Result<Ocr, String> {
    match value {
        "off" => Ok(Ocr::Off),
        "auto" => Ok(Ocr::Auto),
        "force" => Ok(Ocr::Force),
        other => Err(format!(
            "invalid --ocr mode {other:?}; expected auto, off, or force"
        )),
    }
}

fn parse_positive(flag: &str, value: &str) -> Result<f32, String> {
    match value.parse::<f32>() {
        Ok(n) if n.is_finite() && n > 0.0 => Ok(n),
        _ => Err(format!("{flag} needs a positive number, got {value:?}")),
    }
}

fn parse_confidence(value: &str) -> Result<f32, String> {
    match value.parse::<f32>() {
        Ok(n) if (0.0..=1.0).contains(&n) => Ok(n),
        _ => Err(format!(
            "--ocr-min-confidence takes a score between 0 and 1, got {value:?}"
        )),
    }
}

/// Parse a 1-indexed page selection like `1,4,7-9`.
fn parse_pages(spec: &str) -> Result<Vec<u32>, String> {
    let mut pages = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((start, end)) => {
                let start = parse_page(start)?;
                let end = parse_page(end)?;
                if start > end {
                    return Err(format!("page range {part} runs backwards"));
                }
                pages.extend(start..=end);
            }
            None => pages.push(parse_page(part)?),
        }
    }
    if pages.is_empty() {
        return Err("--pages selected no pages".to_string());
    }
    // The pipeline treats the selection as a set, so normalize here and let the
    // reported page counts match what actually gets converted.
    pages.sort_unstable();
    pages.dedup();
    Ok(pages)
}

fn parse_page(s: &str) -> Result<u32, String> {
    match s.trim().parse::<u32>() {
        Ok(0) => Err("pages are 1-indexed, 0 is not a page".to_string()),
        Ok(n) => Ok(n),
        Err(_) => Err(format!("{s:?} is not a page number")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_argv(argv: &[&str]) -> Result<Parsed, String> {
        parse(argv.iter().map(|arg| (*arg).to_string()))
    }

    fn run(argv: &[&str]) -> Invocation {
        match parse_argv(argv).expect("parses") {
            Parsed::Run(invocation) => *invocation,
            _ => panic!("expected a run"),
        }
    }

    #[test]
    fn the_output_defaults_to_the_input_with_md() {
        let invocation = run(&["a/book.pdf"]);
        match invocation.destination {
            Destination::File(path) => assert_eq!(path, PathBuf::from("a/book.md")),
            Destination::Stdout => panic!("expected a file"),
        }
    }

    #[test]
    fn ranges_and_repeats_collapse_to_a_sorted_set() {
        assert_eq!(parse_pages("7-9,1,4,4").unwrap(), [1, 4, 7, 8, 9]);
        assert_eq!(parse_pages(" 3 , 2 ").unwrap(), [2, 3]);
    }

    #[test]
    fn a_page_selection_is_rejected_rather_than_silently_fixed() {
        assert!(parse_pages("9-1").is_err());
        assert!(parse_pages("0").is_err());
        assert!(parse_pages("x").is_err());
        assert!(parse_pages(",").is_err());
    }

    #[test]
    fn bad_flags_and_values_are_refused() {
        assert!(parse_argv(&["--nope", "a.pdf"]).is_err());
        assert!(parse_argv(&["a.pdf", "b.pdf"]).is_err());
        assert!(parse_argv(&["a.pdf", "--ocr", "sometimes"]).is_err());
        assert!(parse_argv(&["a.pdf", "--ocr-dpi", "0"]).is_err());
        assert!(parse_argv(&["a.pdf", "--ocr-min-confidence", "2"]).is_err());
        assert!(parse_argv(&["a.pdf", "--ocr-jobs", "0"]).is_err());
        assert!(parse_argv(&["a.pdf", "--output"]).is_err());
        assert!(parse_argv(&[]).is_err());
    }

    #[test]
    fn help_and_version_win_over_everything_else() {
        assert!(matches!(parse_argv(&["--help"]).unwrap(), Parsed::Help));
        assert!(matches!(
            parse_argv(&["a.pdf", "--version"]).unwrap(),
            Parsed::Version
        ));
    }

    #[test]
    fn estimates_round_to_something_they_can_support() {
        assert_eq!(format_estimate(0), "under a minute");
        assert_eq!(format_estimate(59_000), "under a minute");
        assert_eq!(format_estimate(120_000), "about 2 min");
        assert_eq!(format_estimate(7_200_000), "about 2.0 hours");
    }

    #[test]
    fn the_estimate_grows_with_the_square_of_the_dpi() {
        assert_eq!(estimated_ocr_ms(10, 150.0), 4_000);
        assert_eq!(estimated_ocr_ms(10, 300.0), 16_000);
    }
}
