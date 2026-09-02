//! Running the OCR pass across several processes.
//!
//! The page-at-a-time shape that [`crate::convert`] is forced into leaves most
//! of the machine idle. `pdf-inspector` hands a single-page request to worker 0
//! and nothing else (`OarOcrEngine::recognize` short-circuits to
//! `recognize_page(page, options, 0)` when `pages.len() <= 1`), and that one
//! worker's ONNX sessions are built with two intra-op threads. Measured on a
//! 14-core M4 Pro: 24 pages took 37 s of wall time for 73 s of CPU time — 1.95
//! cores busy out of 14.
//!
//! Threads cannot recover the rest. Every in-process caller would reach the
//! same cached engine (`OCR_ENGINE_CACHE` is a process-wide `OnceLock`), so
//! they would all queue on worker 0's session mutex. Separate processes each
//! get their own engine, their own sessions and their own Rayon pool, share no
//! locks, and so cannot reproduce the re-entrancy deadlock the module docs in
//! [`crate::convert`] describe.
//!
//! # Why the children get `RAYON_NUM_THREADS`
//!
//! `oar-ocr` runs its CTC decode on Rayon's *global* pool, which sizes itself
//! to the whole machine. A child that does two cores of ONNX work therefore
//! also stands up fourteen Rayon threads, and Rayon's workers spin before they
//! park. Fanning out without capping that is actively counter-productive:
//! four uncapped children took 53 s on the 24-page slice, against 37 s for one
//! process, with per-child recognition rising from 16 s to 49 s. Capping the
//! pool at two turned the same fan-out into 12 s. The cap is set on the child's
//! environment rather than globally because the parent's own native pass wants
//! the full pool.
//!
//! # Protocol
//!
//! A child is *this same executable*, re-invoked with no arguments and
//! `PDF_EXTRACTOR_OCR_WORKER` set — which is why a host binary has to call
//! [`crate::run_worker_if_spawned`] before it parses its own command line. The
//! library cannot know what argv that host expects, so it uses none.
//!
//! The job goes down the child's stdin, which keeps the password out of both
//! `ps` output and the filesystem. Results come back through a *file* rather
//! than the reverse pipe, so a child that outruns the parent's reader can never
//! block on a full pipe buffer; the pipe carries only a greeting and then a
//! line per finished page, which is what drives the progress count.
//!
//! The greeting is what separates the two ways a worker can come to nothing. A
//! child that greets and then dies hit a real problem, and its stderr says
//! what. A child that never greets was never the library's to run — the host
//! did something else with the process — and no amount of reading its stderr
//! will say so, because the message it printed is about the host's own command
//! line rather than about OCR.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::codec::{Malformed, Reader, Writer};
use crate::convert::{Harvest, PageOutput, ocr_one_page};
use crate::report::Progress;
use crate::{Error, Ocr, Options, runtime};

/// Set on a child to say "you are a worker". Its value is unused.
const WORKER_ENV: &str = "PDF_EXTRACTOR_OCR_WORKER";

/// Set to keep the temp directory, which is the only way to read a child's
/// full stderr after the fact.
const KEEP_TEMP_ENV: &str = "PDF_EXTRACTOR_KEEP_TEMP";

/// Rayon threads each child is allowed. See the module docs: this is a cap on
/// the CTC decode pool, not on the ONNX sessions, which size themselves.
const CHILD_RAYON_THREADS: &str = "2";

/// Magic for each direction, so a stale file or a foreign pipe cannot be
/// misread as a message.
const JOB_MAGIC: &[u8; 4] = b"PXJ1";
const RESULT_MAGIC: &[u8; 4] = b"PXR1";

/// The child's first line: "the library has this process". See the module docs.
const GREETING: &str = "pdf-extractor worker";
/// Every line after it: "one more page done".
const PAGE: &str = "page";

// ---------------------------------------------------------------------------
// Parent side
// ---------------------------------------------------------------------------

/// OCR `routed` across `jobs` child processes and merge what they return.
pub(crate) fn run_parallel(
    input: &Path,
    options: &Options,
    routed: &[u32],
    jobs: usize,
) -> Result<Harvest, Error> {
    let executable = std::env::current_exe()
        .map_err(|error| Error::Worker(format!("this executable could not be located: {error}")))?;
    let workspace = Workspace::new()?;

    // Round-robin rather than contiguous blocks: OCR cost varies several-fold
    // between a dense page and a mostly blank one, and interleaving keeps one
    // child from drawing the whole dense chapter and running long after the
    // others have finished.
    let mut chunks: Vec<Vec<u32>> = vec![Vec::new(); jobs];
    for (index, page) in routed.iter().enumerate() {
        chunks[index % jobs].push(*page);
    }
    chunks.retain(|chunk| !chunk.is_empty());

    let done = Arc::new(AtomicUsize::new(0));
    let mut running = Vec::with_capacity(chunks.len());

    for (index, chunk) in chunks.iter().enumerate() {
        let result = workspace.path.join(format!("w{index}.bin"));
        let diagnostics = workspace.path.join(format!("w{index}.err"));
        let errors = std::fs::File::create(&diagnostics)
            .map_err(|error| Error::io("create", &diagnostics, error))?;

        let mut child = Command::new(&executable)
            .env(WORKER_ENV, "1")
            .env("RAYON_NUM_THREADS", CHILD_RAYON_THREADS)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(errors))
            .spawn()
            .map_err(|error| Error::Worker(format!("could not start a worker: {error}")))?;

        // The job first, then close stdin. Safe to do from this thread: the
        // child reads stdin to EOF before it writes anything, so it cannot be
        // blocked on a full stdout pipe while we are blocked on its stdin.
        let job = encode_job(input, &result, chunk, options);
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let handed_over = stdin.write_all(&job).and_then(|()| stdin.flush());
        drop(stdin);
        if let Err(error) = handed_over {
            let _ = child.kill();
            return Err(Error::Worker(format!(
                "a worker would not take its job: {error}"
            )));
        }

        // One reader thread per child. It must exist whatever it does with
        // what it reads: an unread pipe fills and stops the child mid-page.
        let pipe = child.stdout.take().expect("stdout was piped");
        let counter = Arc::clone(&done);
        let greeted = Arc::new(AtomicBool::new(false));
        let greeting = Arc::clone(&greeted);
        let reader = std::thread::spawn(move || {
            let mut lines = BufReader::new(pipe);
            let mut line = String::new();
            while matches!(lines.read_line(&mut line), Ok(read) if read > 0) {
                match line.trim() {
                    GREETING => greeting.store(true, Ordering::Relaxed),
                    PAGE => {
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                    // Whatever a host printed before the library took over is
                    // not a page. Ignoring it beats miscounting.
                    _ => {}
                }
                line.clear();
            }
        });

        running.push(Running {
            child,
            reader,
            result,
            diagnostics,
            greeted,
        });
    }

    // Poll rather than block, so the count keeps moving during a run that can
    // last a quarter of an hour. Repeats are filtered here so the caller's
    // callback only ever sees the count advance.
    let total = routed.len();
    let mut reported = 0;
    loop {
        let finished_pages = done.load(Ordering::Relaxed).min(total);
        if finished_pages > reported {
            reported = finished_pages;
            options.emit(Progress::OcrPage {
                done: reported,
                total,
            });
        }
        let finished = running
            .iter_mut()
            .all(|entry| matches!(entry.child.try_wait(), Ok(Some(_)) | Err(_)));
        if finished {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(120));
    }

    let mut harvest = Harvest::default();
    for entry in running {
        harvest.absorb(entry.collect()?);
    }
    Ok(harvest)
}

/// A spawned child and everything needed to collect or explain it.
struct Running {
    child: Child,
    reader: std::thread::JoinHandle<()>,
    result: PathBuf,
    diagnostics: PathBuf,
    /// Whether this child ever said [`GREETING`].
    greeted: Arc<AtomicBool>,
}

impl Running {
    fn collect(mut self) -> Result<Harvest, Error> {
        let status = self
            .child
            .wait()
            .map_err(|error| Error::Worker(format!("could not be waited on: {error}")))?;
        let _ = self.reader.join();

        // Completion is judged by the result file, not by the exit status.
        //
        // The file is renamed into place only after the child has OCR'd every
        // page it was given, so a result that decodes proves the work landed.
        // The exit status does not prove the opposite: ONNX Runtime
        // intermittently aborts inside the static destructors of its `dlopen`ed
        // library — `recursive_mutex lock failed: Invalid argument`, thrown
        // long after the result is on disk. Observed on 2 of 7 concurrent
        // workers and on 0 of 12 run one at a time, so it tracks load rather
        // than anything about the pages. Failing the run over that would throw
        // away a complete and correct result; a child that dies before
        // finishing leaves no file to read and is still reported below.
        if let Ok(encoded) = std::fs::read(&self.result)
            && let Ok(harvest) = decode_result(&encoded)
        {
            return Ok(harvest);
        }

        // Neither the exit status nor the stderr says which of the two
        // failures this is, so the greeting decides and the child's own words
        // are appended for whatever they are worth.
        let said = last_words(&self.diagnostics);
        if !self.greeted.load(Ordering::Relaxed) {
            let mut message = "it never started. The program embedding pdf-extractor must call \
                 pdf_extractor::run_worker_if_spawned() as the first thing in main, or set \
                 ocr_jobs to 1 to keep OCR in this process"
                .to_string();
            if let Some(said) = said {
                let _ = write!(message, " (it said: {said})");
            }
            return Err(Error::Worker(message));
        }
        Err(Error::Worker(said.unwrap_or_else(|| {
            // It greeted, then died without a word. The status is all there is.
            format!("exited with {status}")
        })))
    }
}

/// The most useful line a dead child left on stderr.
///
/// A worker reports through the same `error:` prefix the caller is about to
/// wrap this in, so that line wins and its prefix goes. Failing that — a panic,
/// an abort inside a `dlopen`ed library — the last line that carries any
/// information does, which is not quite the last line: a panic signs off with
/// a note about `RUST_BACKTRACE` that says nothing about what went wrong.
fn last_words(diagnostics: &Path) -> Option<String> {
    let text = std::fs::read_to_string(diagnostics).ok()?;
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("note: run with"))
        .collect();
    lines
        .iter()
        .rev()
        .find_map(|line| line.strip_prefix("error: "))
        .or_else(|| lines.last().copied())
        .map(str::to_string)
}

/// A temp directory that removes itself, so a long run does not litter.
struct Workspace {
    path: PathBuf,
}

impl Workspace {
    fn new() -> Result<Self, Error> {
        // The pid alone would collide with a crashed run's leftovers once the
        // OS recycled it, and a stale `w0.bin` would then decode as a result.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        let path =
            std::env::temp_dir().join(format!("pdf-extractor-{}-{stamp:09}", std::process::id()));
        std::fs::create_dir_all(&path).map_err(|error| Error::io("create", &path, error))?;
        restrict(&path);
        Ok(Self { path })
    }
}

/// Keep the job and result files to this user. They hold the document's text,
/// and a shared `/tmp` is the default everywhere this runs.
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

impl Drop for Workspace {
    fn drop(&mut self) {
        // Keeping the directory is the only way to see a child's full output
        // after the fact, which is what a worker failure usually needs.
        if std::env::var_os(KEEP_TEMP_ENV).is_some() {
            eprintln!("kept worker files in {}", self.path.display());
            return;
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------------------
// Child side
// ---------------------------------------------------------------------------

/// See [`crate::run_worker_if_spawned`], which is this with the documentation.
pub(crate) fn run_if_spawned() -> Option<ExitCode> {
    std::env::var_os(WORKER_ENV)?;

    // Say so before anything that can fail, so the parent can tell a worker
    // that broke from a process that was never a worker at all.
    let mut greeting = std::io::stdout();
    let _ = writeln!(greeting, "{GREETING}");
    let _ = greeting.flush();

    Some(match work() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // The parent scrapes the last non-empty line of this.
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    })
}

fn work() -> Result<(), Error> {
    let mut job = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin().lock(), &mut job)
        .map_err(|error| Error::Worker(format!("could not read its job: {error}")))?;
    let job = decode_job(&job)
        .map_err(|error| Error::Worker(format!("could not read its job: {error}")))?;

    // The parent already filled these in and children inherit them, so this is
    // normally a no-op; it matters when the parent is a different program.
    runtime::init();

    let bytes = std::fs::read(&job.input).map_err(|error| Error::io("read", &job.input, error))?;
    let mode = job.options.ocr_mode();
    let mut harvest = Harvest::default();
    let mut progress = std::io::stdout();
    for page in job.pages {
        ocr_one_page(&job.options, &bytes, mode, page, &mut harvest)?;
        // One line per finished page is the parent's entire progress feed.
        let _ = writeln!(progress, "{PAGE}");
        let _ = progress.flush();
    }

    // Write, then rename. The parent treats a decodable result as proof that
    // this worker finished every page it was given, so the file must never be
    // observable half-written. Every step above returns early on failure, so
    // reaching the rename means the whole page list succeeded.
    let staged = job.result.with_extension("partial");
    std::fs::write(&staged, encode_result(&harvest))
        .map_err(|error| Error::io("write", &staged, error))?;
    std::fs::rename(&staged, &job.result).map_err(|error| Error::io("finish", &job.result, error))
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

struct Job {
    input: PathBuf,
    result: PathBuf,
    pages: Vec<u32>,
    options: Options,
}

fn encode_job(input: &Path, result: &Path, pages: &[u32], options: &Options) -> Vec<u8> {
    let mut out = Writer::new(JOB_MAGIC);
    out.path(input)
        .path(result)
        .u32s(pages.iter().copied())
        // The page selection is not carried: a worker is told one page at a
        // time, and that page always wins over it.
        .bool(options.page_markers)
        .bool(options.images)
        .bool(options.keep_furniture)
        .bool(options.compact)
        .opt_str(options.password.as_deref())
        .u32(match options.ocr {
            Ocr::Off => 0,
            Ocr::Auto => 1,
            Ocr::Force => 2,
        })
        .f32(options.ocr_dpi)
        .f32(options.ocr_min_confidence)
        .opt_path(options.ocr_model_dir.as_deref())
        .bool(options.ocr_offline);
    out.finish()
}

fn decode_job(bytes: &[u8]) -> Result<Job, Malformed> {
    let mut reader = Reader::new(bytes, JOB_MAGIC)?;
    let input = reader.path()?;
    let result = reader.path()?;
    let pages = reader.u32s()?;
    let options = Options {
        pages: None,
        page_markers: reader.bool()?,
        images: reader.bool()?,
        keep_furniture: reader.bool()?,
        compact: reader.bool()?,
        password: reader.opt_str()?,
        ocr: match reader.u32()? {
            0 => Ocr::Off,
            2 => Ocr::Force,
            _ => Ocr::Auto,
        },
        ocr_dpi: reader.f32()?,
        ocr_min_confidence: reader.f32()?,
        ocr_model_dir: reader.opt_path()?,
        ocr_offline: reader.bool()?,
        // A worker never fans out again, and reports through its pipe.
        ocr_jobs: Some(1),
        progress: None,
    };
    Ok(Job {
        input,
        result,
        pages,
        options,
    })
}

fn encode_result(harvest: &Harvest) -> Vec<u8> {
    let mut out = Writer::new(RESULT_MAGIC);
    out.u64(harvest.render_ms).u64(harvest.ocr_ms);
    for set in [&harvest.hosted, &harvest.tables, &harvest.columns] {
        out.u32s(set.iter().copied());
    }
    out.len(harvest.pages.len());
    for page in harvest.pages.values() {
        out.u32(page.page_number)
            .bool(page.is_ocr)
            .len(page.warnings.len());
        for warning in &page.warnings {
            out.str(warning);
        }
        out.str(&page.markdown);
    }
    out.finish()
}

fn decode_result(bytes: &[u8]) -> Result<Harvest, Malformed> {
    let mut reader = Reader::new(bytes, RESULT_MAGIC)?;
    let mut harvest = Harvest {
        render_ms: reader.u64()?,
        ocr_ms: reader.u64()?,
        ..Harvest::default()
    };
    for set in [
        &mut harvest.hosted,
        &mut harvest.tables,
        &mut harvest.columns,
    ] {
        set.extend(reader.u32s()?);
    }
    let pages = reader.len()?;
    for _ in 0..pages {
        let page_number = reader.u32()?;
        let is_ocr = reader.bool()?;
        let warning_count = reader.len()?;
        let mut warnings = Vec::new();
        for _ in 0..warning_count {
            warnings.push(reader.str()?);
        }
        harvest.pages.insert(
            page_number,
            PageOutput {
                page_number,
                markdown: reader.str()?,
                is_ocr,
                warnings,
            },
        );
    }
    Ok(harvest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(number: u32, markdown: &str, warnings: &[&str]) -> PageOutput {
        PageOutput {
            page_number: number,
            markdown: markdown.to_string(),
            is_ocr: true,
            warnings: warnings.iter().map(|w| w.to_string()).collect(),
        }
    }

    #[test]
    fn round_trips_a_harvest() {
        let mut harvest = Harvest {
            render_ms: 1234,
            ocr_ms: 56789,
            ..Harvest::default()
        };
        harvest.hosted.insert(7);
        harvest.tables.extend([3, 9]);
        harvest.columns.insert(4);
        // Markdown with the delimiters a line-oriented encoding would break on.
        harvest.pages.insert(
            3,
            page(3, "# Heading\n\nA line\r\nand \0 a null\n", &["went wrong"]),
        );
        harvest.pages.insert(9, page(9, "", &[]));

        let decoded = decode_result(&encode_result(&harvest)).expect("decodes");
        assert_eq!(decoded.render_ms, 1234);
        assert_eq!(decoded.ocr_ms, 56789);
        assert_eq!(decoded.hosted, harvest.hosted);
        assert_eq!(decoded.tables, harvest.tables);
        assert_eq!(decoded.columns, harvest.columns);
        assert_eq!(decoded.pages.len(), 2);
        assert_eq!(
            decoded.pages[&3].markdown,
            "# Heading\n\nA line\r\nand \0 a null\n"
        );
        assert_eq!(decoded.pages[&3].warnings, vec!["went wrong".to_string()]);
        assert!(decoded.pages[&9].markdown.is_empty());
    }

    #[test]
    fn rejects_a_truncated_result() {
        let harvest = Harvest {
            pages: [(1, page(1, "text", &[]))].into_iter().collect(),
            ..Harvest::default()
        };
        let encoded = encode_result(&harvest);
        assert!(decode_result(&encoded[..encoded.len() - 2]).is_err());
    }

    #[test]
    fn rejects_a_foreign_file() {
        assert!(decode_result(b"not a result file").is_err());
    }

    #[test]
    fn a_job_survives_the_trip() {
        let options = Options::new()
            .pages([1, 2, 3])
            .images(true)
            .compact(true)
            .password("hunter2")
            .ocr(Ocr::Force)
            .ocr_dpi(150.0)
            .ocr_min_confidence(0.25)
            .ocr_model_dir("/models")
            .ocr_offline(true);

        let encoded = encode_job(
            Path::new("/in/a book.pdf"),
            Path::new("/tmp/w0.bin"),
            &[4, 8, 15],
            &options,
        );
        let job = decode_job(&encoded).expect("decodes");

        assert_eq!(job.input, Path::new("/in/a book.pdf"));
        assert_eq!(job.result, Path::new("/tmp/w0.bin"));
        assert_eq!(job.pages, [4, 8, 15]);
        assert_eq!(job.options.password.as_deref(), Some("hunter2"));
        assert_eq!(job.options.ocr, Ocr::Force);
        assert_eq!(job.options.ocr_dpi, 150.0);
        assert_eq!(job.options.ocr_min_confidence, 0.25);
        assert_eq!(
            job.options.ocr_model_dir.as_deref(),
            Some(Path::new("/models"))
        );
        assert!(job.options.ocr_offline);
        assert!(job.options.images && job.options.compact);
        assert!(!job.options.keep_furniture && !job.options.page_markers);
        // A worker takes its pages one at a time and never fans out again.
        assert_eq!(job.options.pages, None);
        assert_eq!(job.options.ocr_jobs, Some(1));
    }

    #[test]
    fn a_job_is_not_a_result() {
        let encoded = encode_job(
            Path::new("a.pdf"),
            Path::new("w0.bin"),
            &[1],
            &Options::new(),
        );
        assert_eq!(decode_result(&encoded).err(), Some(Malformed::Unrecognized));
    }
}
