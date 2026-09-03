//! The command line: parsing it, and saying what a run is about to do.

use std::path::PathBuf;

use agent::Provider;
use pdf_extractor::{DEFAULT_OCR_DPI, Ocr, Options, Survey};

pub const USAGE: &str = "\
kb-agent — a directory of books and papers as something you can ask

USAGE:
    kb-agent build <dir> [OPTIONS]                Every PDF into Markdown, every Markdown into a summary
    kb-agent status <dir>                         What is built and what is not
    kb-agent query <dir> \"<question>\" [OPTIONS]   Put a question to every document in the library
    kb-agent chat <dir> [OPTIONS]                 A prompt: questions, with the rest as /commands
    kb-agent convert <input.pdf> [OPTIONS]        One PDF into Markdown
    kb-agent <input.pdf> [OPTIONS]                The same as convert

    -h, --help            Show this help; `kb-agent <command> --help` shows the command's
    -V, --version         Show the version

A library is a directory. Put PDFs in it, run `build`, and `query` reads every
document — each in full, each in its own request — before it answers.";

pub const CONVERT_USAGE: &str = "\
kb-agent convert — convert a PDF into a Markdown file

USAGE:
    kb-agent convert <input.pdf> [OPTIONS]

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

// The first line is not continued from the quote: a `\` there would strip
// the indent along with the newline.
pub const LLM_USAGE: &str =
    "        --provider <name>       openai (default) or anthropic; the key comes from
                                OPENAI_API_KEY or ANTHROPIC_API_KEY
        --model <name>          Model to use (default: the provider's own)
        --concurrency <n>       Requests in flight at once (default: 8)
        --context-tokens <n>    Input budget per request; a document over it is
                                left out rather than cut down (default: 700000)
        --max-tokens <n>        Output cap per request (default: 50000)
        --retries <n>           Resends when the provider is busy (default: 5)";

pub const BUILD_USAGE: &str = "\
kb-agent build — make a directory of documents into a library

USAGE:
    kb-agent build <dir> [OPTIONS]

Walks <dir>. Every PDF without a .md beside it is converted; every .md without
a _summary.md beside it is summarized. Both steps skip what is already there,
so a build picks up where it left off, and a document dropped in later is
picked up by the next one. Hidden files and directories are ignored.

OPTIONS:
        --plan                  Say what would be done and stop
        --force                 Redo summaries that already exist
        --reconvert             Redo Markdown that already exists
        --no-summaries          Convert only; send nothing
        --focus <text>          Something the summaries should pay attention to
        --ocr <mode>            auto (default), off, or force — see `convert --help`
        --ocr-dpi <n>           Page render resolution for OCR (default: 300)
        --ocr-jobs <n>          OCR worker processes per PDF
        --compact               Token-efficient Markdown instead of source fidelity
    -h, --help                  Show this help

LLM OPTIONS:
{LLM}";

pub const QUERY_USAGE: &str = "\
kb-agent query — put a question to every document in a library

USAGE:
    kb-agent query <dir> \"<question>\" [OPTIONS]

Four stages, each many requests with a fresh context, each written to the
output directory as it finishes:
    mask     every summary is judged for relevance to the question   → mask.md
    read     every relevant document is read in full and asked        → points.raw.md
    reduce   every pair of points is compared and the repeats merged  → points.md
    answer   the question is answered from the reduced list           → answer.md

The answer goes to stdout as well. The reduce stage costs the square of the
list: 200 points is 20,000 comparisons. The count is printed before it starts.

OPTIONS:
    -o, --output <dir>          Where the files go
                                (default: <dir>/.kb-agent/queries/<time>-<question>/)
        --plan                  Say which sources would be judged and stop
        --no-reduce             Skip the reduction; answer from the raw list
        --no-answer             Stop after the list; write no answer
    -h, --help                  Show this help

LLM OPTIONS:
{LLM}";

pub const CHAT_USAGE: &str = "\
kb-agent chat — a prompt on a library

USAGE:
    kb-agent chat <dir> [OPTIONS]

A line that does not start with / is a question, run exactly as
`kb-agent query <dir> \"<question>\"` would run it: the answer is printed and
the run's files are kept under <dir>/.kb-agent/queries/. A line that starts
with / is a command — build, status and convert as they are here, plus what
only makes sense in a session: settings that hold from one question to the
next, the runs so far, and a follow-up put to the last run's list without
reading the library again. /help lists them.

Ctrl-C stops the command running and comes back to the prompt. Ctrl-D leaves.
The line history is kept in <dir>/.kb-agent/history.

OPTIONS:
    -h, --help                  Show this help

LLM OPTIONS (the session's starting settings; /set changes them):
{LLM}";

/// What the command line asked for.
pub enum Command {
    Convert(Box<Invocation>),
    Build(Box<BuildArgs>),
    Status(PathBuf),
    Query(Box<QueryArgs>),
    Chat(Box<ChatArgs>),
    /// Print this text and stop.
    Help(String),
    Version,
}

pub fn parse(argv: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut argv = argv.peekable();
    let Some(first) = argv.next() else {
        return Err(format!("no command given\n\n{USAGE}"));
    };
    match first.as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help(USAGE.to_string())),
        "-V" | "--version" => Ok(Command::Version),
        "convert" => parse_convert(argv),
        "build" => parse_build(argv),
        "status" => parse_status(argv),
        "query" => parse_query(argv),
        "chat" | "repl" => parse_chat(argv),
        other if other.starts_with('-') => Err(format!("unknown option {other}\n\n{USAGE}")),
        // `kb-agent book.pdf`, as the command has always been used.
        _ => parse_convert(std::iter::once(first).chain(argv)),
    }
}

// --- convert ---------------------------------------------------------------

/// A parsed `convert` command line: where the Markdown goes, and everything
/// the library needs to produce it.
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

fn parse_convert(argv: impl Iterator<Item = String>) -> Result<Command, String> {
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
            "-h" | "--help" => return Ok(Command::Help(CONVERT_USAGE.to_string())),
            "-V" | "--version" => return Ok(Command::Version),
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
                return Err(format!("unknown option {other}\n\n{CONVERT_USAGE}"));
            }
            other => {
                if input.replace(PathBuf::from(other)).is_some() {
                    return Err(format!("unexpected extra argument {other}\n\n{CONVERT_USAGE}"));
                }
            }
        }
    }

    let input = input.ok_or_else(|| format!("no input PDF given\n\n{CONVERT_USAGE}"))?;

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

    Ok(Command::Convert(Box::new(Invocation {
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

// --- the LLM flags every request-making command shares --------------------

/// The flags that become an [`agent::Options`], plus the one that is the
/// library's: how many requests to keep in flight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Llm {
    pub provider: Provider,
    pub model: Option<String>,
    pub concurrency: usize,
    pub context_tokens: usize,
    pub max_tokens: u64,
    pub retries: u32,
}

impl Default for Llm {
    fn default() -> Self {
        Self {
            provider: Provider::default(),
            model: None,
            concurrency: kb::DEFAULT_CONCURRENCY,
            context_tokens: agent::DEFAULT_CONTEXT_TOKENS,
            max_tokens: agent::DEFAULT_MAX_TOKENS,
            retries: agent::DEFAULT_RETRIES,
        }
    }
}

impl Llm {
    /// Take `arg` if it is one of these flags, consuming its value. `false`
    /// means it was not one of these.
    pub(crate) fn take(
        &mut self,
        arg: &str,
        argv: &mut impl Iterator<Item = String>,
    ) -> Result<bool, String> {
        match arg {
            "--provider" => self.provider = parse_provider(&value_for(arg, argv)?)?,
            "--model" => self.model = Some(value_for(arg, argv)?),
            "--concurrency" => self.concurrency = parse_count(arg, &value_for(arg, argv)?)?,
            "--context-tokens" => {
                self.context_tokens = parse_count(arg, &value_for(arg, argv)?)?;
            }
            "--max-tokens" => self.max_tokens = parse_count(arg, &value_for(arg, argv)?)? as u64,
            "--retries" => {
                self.retries = value_for(arg, argv)?
                    .parse()
                    .map_err(|_| format!("{arg} needs a whole number"))?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    pub fn options(&self) -> agent::Options {
        let mut options = agent::Options::new()
            .provider(self.provider)
            .context_tokens(self.context_tokens)
            .max_tokens(self.max_tokens)
            .retries(self.retries);
        if let Some(model) = &self.model {
            options = options.model(model.clone());
        }
        options
    }

    /// `gpt-5.6 via OpenAI, 8 at a time`, for the plan line.
    pub fn describe(&self) -> String {
        format!(
            "{} via {}, {} at a time",
            self.options().resolved_model(),
            self.provider.name(),
            self.concurrency
        )
    }

    /// These settings as the flags that would produce them, so a command
    /// line assembled inside the chat can go through the same parser as one
    /// typed at the shell. Later flags win there, so anything appended
    /// overrides these.
    pub fn flags(&self) -> Vec<String> {
        let mut flags = vec![
            "--provider".to_string(),
            self.provider.name().to_ascii_lowercase(),
        ];
        if let Some(model) = &self.model {
            flags.push("--model".to_string());
            flags.push(model.clone());
        }
        for (flag, value) in [
            ("--concurrency", self.concurrency.to_string()),
            ("--context-tokens", self.context_tokens.to_string()),
            ("--max-tokens", self.max_tokens.to_string()),
            ("--retries", self.retries.to_string()),
        ] {
            flags.push(flag.to_string());
            flags.push(value);
        }
        flags
    }
}

fn parse_provider(value: &str) -> Result<Provider, String> {
    match value.to_ascii_lowercase().as_str() {
        "openai" | "chatgpt" => Ok(Provider::OpenAi),
        "anthropic" | "claude" => Ok(Provider::Anthropic),
        other => Err(format!(
            "unknown provider {other:?}; expected openai or anthropic"
        )),
    }
}

fn parse_count(flag: &str, value: &str) -> Result<usize, String> {
    match value.replace(['_', ','], "").parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!("{flag} needs a count of 1 or more, got {value:?}")),
    }
}

// --- build -----------------------------------------------------------------

pub struct BuildArgs {
    pub dir: PathBuf,
    pub llm: Llm,
    pub plan: bool,
    pub force: bool,
    pub reconvert: bool,
    pub summaries: bool,
    pub focus: Option<String>,
    pub extractor: Options,
}

fn parse_build(argv: impl Iterator<Item = String>) -> Result<Command, String> {
    let usage = BUILD_USAGE.replace("{LLM}", LLM_USAGE);
    let mut dir = None;
    let mut llm = Llm::default();
    let mut plan = false;
    let mut force = false;
    let mut reconvert = false;
    let mut summaries = true;
    let mut focus = None;
    let mut ocr = Ocr::Auto;
    let mut ocr_dpi = DEFAULT_OCR_DPI;
    let mut ocr_jobs = None;
    let mut compact = false;

    let mut argv = argv.peekable();
    while let Some(arg) = argv.next() {
        if llm.take(&arg, &mut argv)? {
            continue;
        }
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help(usage)),
            "--plan" => plan = true,
            "--force" => force = true,
            "--reconvert" => reconvert = true,
            "--no-summaries" => summaries = false,
            "--focus" => focus = Some(value_for(&arg, &mut argv)?),
            "--ocr" => ocr = parse_ocr_mode(&value_for(&arg, &mut argv)?)?,
            "--ocr-dpi" => ocr_dpi = parse_positive(&arg, &value_for(&arg, &mut argv)?)?,
            "--ocr-jobs" => ocr_jobs = Some(parse_jobs(&value_for(&arg, &mut argv)?)?),
            "--compact" => compact = true,
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other}\n\n{usage}"));
            }
            other => {
                if dir.replace(PathBuf::from(other)).is_some() {
                    return Err(format!("unexpected extra argument {other}\n\n{usage}"));
                }
            }
        }
    }
    let dir = dir.ok_or_else(|| format!("no directory given\n\n{usage}"))?;
    let extractor = Options::new()
        .ocr(ocr)
        .ocr_dpi(ocr_dpi)
        .ocr_jobs(ocr_jobs)
        .compact(compact);
    Ok(Command::Build(Box::new(BuildArgs {
        dir,
        llm,
        plan,
        force,
        reconvert,
        summaries,
        focus,
        extractor,
    })))
}

// --- status ----------------------------------------------------------------

fn parse_status(argv: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut dir = None;
    for arg in argv {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help(USAGE.to_string())),
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other}\n\n{USAGE}"));
            }
            other => {
                if dir.replace(PathBuf::from(other)).is_some() {
                    return Err(format!("unexpected extra argument {other}\n\n{USAGE}"));
                }
            }
        }
    }
    dir.map(Command::Status)
        .ok_or_else(|| format!("no directory given\n\n{USAGE}"))
}

// --- query -----------------------------------------------------------------

pub struct QueryArgs {
    pub dir: PathBuf,
    pub question: String,
    pub llm: Llm,
    pub output: Option<PathBuf>,
    pub plan: bool,
    pub reduce: bool,
    pub answer: bool,
}

fn parse_query(argv: impl Iterator<Item = String>) -> Result<Command, String> {
    let usage = QUERY_USAGE.replace("{LLM}", LLM_USAGE);
    let mut positional: Vec<String> = Vec::new();
    let mut llm = Llm::default();
    let mut output = None;
    let mut plan = false;
    let mut reduce = true;
    let mut answer = true;

    let mut argv = argv.peekable();
    while let Some(arg) = argv.next() {
        if llm.take(&arg, &mut argv)? {
            continue;
        }
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help(usage)),
            "-o" | "--output" => output = Some(PathBuf::from(value_for(&arg, &mut argv)?)),
            "--plan" => plan = true,
            "--no-reduce" => reduce = false,
            "--no-answer" => answer = false,
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unknown option {other}\n\n{usage}"));
            }
            _ => positional.push(arg),
        }
    }
    let mut positional = positional.into_iter();
    let dir = positional
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("no directory given\n\n{usage}"))?;
    // Everything after the directory is the question, so it need not be
    // quoted as one argument.
    let question = positional.collect::<Vec<_>>().join(" ");
    if question.trim().is_empty() {
        return Err(format!("no question given\n\n{usage}"));
    }
    Ok(Command::Query(Box::new(QueryArgs {
        dir,
        question,
        llm,
        output,
        plan,
        reduce,
        answer,
    })))
}

// --- chat ------------------------------------------------------------------

pub struct ChatArgs {
    pub dir: PathBuf,
    pub llm: Llm,
}

fn parse_chat(argv: impl Iterator<Item = String>) -> Result<Command, String> {
    let usage = CHAT_USAGE.replace("{LLM}", LLM_USAGE);
    let mut dir = None;
    let mut llm = Llm::default();

    let mut argv = argv.peekable();
    while let Some(arg) = argv.next() {
        if llm.take(&arg, &mut argv)? {
            continue;
        }
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help(usage)),
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other}\n\n{usage}"));
            }
            other => {
                if dir.replace(PathBuf::from(other)).is_some() {
                    return Err(format!("unexpected extra argument {other}\n\n{usage}"));
                }
            }
        }
    }
    let dir = dir.ok_or_else(|| format!("no directory given\n\n{usage}"))?;
    Ok(Command::Chat(Box::new(ChatArgs { dir, llm })))
}

// --- shared pieces ---------------------------------------------------------

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

    fn parse_argv(argv: &[&str]) -> Result<Command, String> {
        parse(argv.iter().map(|arg| (*arg).to_string()))
    }

    fn convert(argv: &[&str]) -> Invocation {
        match parse_argv(argv).expect("parses") {
            Command::Convert(invocation) => *invocation,
            _ => panic!("expected a conversion"),
        }
    }

    fn query(argv: &[&str]) -> QueryArgs {
        match parse_argv(argv).expect("parses") {
            Command::Query(args) => *args,
            _ => panic!("expected a query"),
        }
    }

    fn build(argv: &[&str]) -> BuildArgs {
        match parse_argv(argv).expect("parses") {
            Command::Build(args) => *args,
            _ => panic!("expected a build"),
        }
    }

    #[test]
    fn a_bare_pdf_is_a_conversion_as_it_always_was() {
        let invocation = convert(&["a/book.pdf"]);
        match invocation.destination {
            Destination::File(path) => assert_eq!(path, PathBuf::from("a/book.md")),
            Destination::Stdout => panic!("expected a file"),
        }
        let explicit = convert(&["convert", "a/book.pdf", "--stdout"]);
        assert!(matches!(explicit.destination, Destination::Stdout));
    }

    #[test]
    fn a_query_takes_the_rest_of_the_line_as_the_question() {
        let args = query(&["query", "lib", "what", "limits", "throughput?"]);
        assert_eq!(args.dir, PathBuf::from("lib"));
        assert_eq!(args.question, "what limits throughput?");
        assert!(args.reduce && args.answer && !args.plan);
        assert_eq!(args.llm.concurrency, kb::DEFAULT_CONCURRENCY);
    }

    #[test]
    fn query_flags_land_where_they_should() {
        let args = query(&[
            "query", "lib", "--provider", "anthropic", "--model", "m", "--concurrency", "3",
            "--context-tokens", "100_000", "--no-reduce", "--no-answer", "-o", "out", "q",
        ]);
        assert_eq!(args.llm.provider, Provider::Anthropic);
        assert_eq!(args.llm.options().resolved_model(), "m");
        assert_eq!(args.llm.concurrency, 3);
        assert_eq!(args.llm.context_tokens, 100_000);
        assert!(!args.reduce && !args.answer);
        assert_eq!(args.output, Some(PathBuf::from("out")));
        assert_eq!(args.question, "q");
        assert_eq!(args.llm.describe(), "m via Anthropic, 3 at a time");
    }

    #[test]
    fn a_build_takes_conversion_and_llm_flags_together() {
        let args = build(&["build", "lib", "--ocr", "off", "--force", "--retries", "0", "--plan"]);
        assert_eq!(args.dir, PathBuf::from("lib"));
        assert!(args.force && args.plan && !args.reconvert && args.summaries);
        assert_eq!(args.llm.retries, 0);
    }

    #[test]
    fn a_chat_takes_a_directory_and_the_llm_flags() {
        let args = match parse_argv(&["chat", "lib", "--provider", "claude", "--retries", "2"]) {
            Ok(Command::Chat(args)) => *args,
            other => panic!("expected a chat, got {:?}", other.map(|_| ())),
        };
        assert_eq!(args.dir, PathBuf::from("lib"));
        assert_eq!(args.llm.provider, Provider::Anthropic);
        assert_eq!(args.llm.retries, 2);
        assert!(matches!(parse_argv(&["repl", "lib"]).unwrap(), Command::Chat(_)));
        assert!(parse_argv(&["chat", "lib", "--plan"]).is_err());
        assert!(parse_argv(&["chat", "a", "b"]).is_err());
        match parse_argv(&["chat", "--help"]).unwrap() {
            Command::Help(text) => assert!(text.contains("--provider") && !text.contains("{LLM}")),
            _ => panic!("expected help"),
        }
    }

    #[test]
    fn llm_settings_survive_a_trip_through_their_own_flags() {
        for provider in [Provider::OpenAi, Provider::Anthropic] {
            let llm = Llm {
                provider,
                model: Some("m".to_string()),
                concurrency: 3,
                context_tokens: 12_000,
                max_tokens: 900,
                retries: 1,
            };
            let mut argv = vec!["query".to_string(), "lib".to_string()];
            argv.extend(llm.flags());
            argv.push("q".to_string());
            let parsed = parse(argv.into_iter()).expect("parses");
            match parsed {
                Command::Query(args) => assert_eq!(args.llm, llm),
                _ => panic!("expected a query"),
            }
        }
        let flags = Llm::default().flags();
        assert!(!flags.contains(&"--model".to_string()), "no model means the provider's");
    }

    #[test]
    fn every_command_wants_its_argument() {
        assert!(parse_argv(&["query", "lib"]).is_err());
        assert!(parse_argv(&["query"]).is_err());
        assert!(parse_argv(&["build"]).is_err());
        assert!(parse_argv(&["status"]).is_err());
        assert!(parse_argv(&["status", "a", "b"]).is_err());
        assert!(parse_argv(&["chat"]).is_err());
        assert!(parse_argv(&[]).is_err());
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
        assert!(parse_argv(&["query", "lib", "q", "--provider", "gemini"]).is_err());
        assert!(parse_argv(&["query", "lib", "q", "--concurrency", "0"]).is_err());
        assert!(parse_argv(&["build", "lib", "--wat"]).is_err());
    }

    #[test]
    fn help_and_version_win_over_everything_else() {
        assert!(matches!(parse_argv(&["--help"]).unwrap(), Command::Help(_)));
        assert!(matches!(
            parse_argv(&["a.pdf", "--version"]).unwrap(),
            Command::Version
        ));
        match parse_argv(&["query", "--help"]).unwrap() {
            Command::Help(text) => {
                assert!(text.contains("kb-agent query"));
                assert!(text.contains("--provider"), "the LLM flags are filled in");
                assert!(!text.contains("{LLM}"));
            }
            _ => panic!("expected help"),
        }
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
