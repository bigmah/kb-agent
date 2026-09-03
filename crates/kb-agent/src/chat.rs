//! `kb-agent chat`: a prompt on a library.
//!
//! A line that does not start with `/` is a question, run through every
//! stage `kb-agent query` runs: the answer is printed and the run's files
//! land under `.kb-agent/queries/` as usual. A line that starts with `/` is
//! a command — the other subcommands with their own flags, and what only
//! makes sense inside a session: settings that hold from one question to
//! the next, the runs so far, and a follow-up put to the last run's list
//! without reading the library again.
//!
//! Ctrl-C stops the command running and comes back to the prompt: requests
//! in flight are dropped, a PDF being converted is abandoned, files already
//! written stay. Ctrl-D leaves.
//!
//! Everything a command does is the same code the subcommand runs. A slash
//! command's line is split into words, put behind the subcommand's name and
//! the session's settings, and handed to the same parser, so the flags are
//! the flags and the help is the help.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kb::Corpus;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::cli::{self, ChatArgs, Command, Llm, QueryArgs};
use crate::{build, convert, progress, query, status};

pub const HELP: &str = "\
A line that does not start with / is a question for the library.

    /status                        What is built and what is not
    /build [OPTIONS]               Convert and summarize; the options are `kb-agent build`'s
    /convert <file.pdf> [OPTIONS]  One PDF into Markdown; the options are `kb-agent convert`'s
    /plan <question>               Which sources a question would reach, without sending anything
    /answer <question>             A follow-up put to the last run's list, in one request,
                                   without reading the library again
    /runs                          The runs so far, newest first
    /show [run]                    The answer from the last run, or from a run by number or name
    /points [run]                  The list from the last run, or from a run by number or name
    /set                           The session's settings
    /set <name> <value>            provider, model, concurrency, context-tokens, max-tokens,
                                   retries, reduce (on/off), answer (on/off)
    /open <dir>                    Switch to another library
    /help                          This
    /quit                          Leave; so does Ctrl-D

Paths are relative to where kb-agent was started. Ctrl-C stops the command
running and comes back to the prompt: requests in flight are dropped, a PDF
being converted is abandoned, files already written stay.";

/// Where the prompt's line history is kept, under the library.
const HISTORY_FILE: &str = ".kb-agent/history";

pub fn run(args: ChatArgs) -> Result<(), String> {
    // Before any thread exists — the Ctrl-C handler starts one, and so does
    // the runtime the first time it blocks. See `convert.rs`.
    pdf_extractor::init();
    let interrupt = Interrupt::install()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime: {error}"))?;
    let mut session = Session {
        dir: args.dir.clone(),
        llm: args.llm,
        reduce: true,
        answer: true,
        last_run: None,
        runtime,
        interrupt,
    };
    session.open(&args.dir)?;

    let mut editor =
        DefaultEditor::new().map_err(|error| format!("could not open the prompt: {error}"))?;
    load_history(&mut editor, &session.dir);
    eprintln!("Ask a question, or /help for the commands.");

    loop {
        let line = match editor.readline(&session.prompt()) {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => {
                eprintln!("(/quit or Ctrl-D leaves)");
                continue;
            }
            Err(ReadlineError::Eof) => break,
            Err(error) => return Err(format!("could not read a line: {error}")),
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let _ = editor.add_history_entry(line);

        let before = session.dir.clone();
        match session.handle(line) {
            Ok(Flow::Continue) => {}
            Ok(Flow::Quit) => break,
            Err(message) => eprintln!("error: {message}"),
        }
        if session.dir != before {
            // The history follows the library, since the questions do.
            save_history(&mut editor, &before);
            let _ = editor.clear_history();
            load_history(&mut editor, &session.dir);
        }
    }
    save_history(&mut editor, &session.dir);
    Ok(())
}

enum Flow {
    Continue,
    Quit,
}

struct Session {
    dir: PathBuf,
    llm: Llm,
    reduce: bool,
    answer: bool,
    /// The run this session made most recently: what `/show`, `/points` and
    /// `/answer` mean by "the last run" until the next one lands.
    last_run: Option<PathBuf>,
    runtime: tokio::runtime::Runtime,
    interrupt: Interrupt,
}

impl Session {
    fn prompt(&self) -> String {
        format!("{}> ", prompt_name(&self.dir))
    }

    fn handle(&mut self, line: &str) -> Result<Flow, String> {
        let Some(rest) = line.strip_prefix('/') else {
            self.ask(line)?;
            return Ok(Flow::Continue);
        };
        let (command, rest) = match rest.split_once(char::is_whitespace) {
            Some((command, rest)) => (command, rest.trim()),
            None => (rest, ""),
        };
        match command {
            "help" | "h" | "?" => println!("{HELP}"),
            "quit" | "exit" | "q" => return Ok(Flow::Quit),
            "status" => status::run(&self.dir)?,
            "build" => self.build(rest)?,
            "convert" => self.convert(rest)?,
            "plan" => self.plan(rest)?,
            "answer" => self.answer(rest)?,
            "runs" => self.runs()?,
            "show" => self.show(rest)?,
            "points" => self.points(rest)?,
            "set" => self.set(rest)?,
            "open" => {
                if rest.is_empty() {
                    return Err("/open needs a directory".to_string());
                }
                self.open(Path::new(rest))?;
            }
            other => return Err(format!("no such command /{other} — /help lists them")),
        }
        Ok(Flow::Continue)
    }

    /// Point the session at `dir`, saying what is there.
    fn open(&mut self, dir: &Path) -> Result<(), String> {
        let corpus = Corpus::scan(dir).map_err(|e| e.to_string())?;
        let status = corpus.status();
        eprintln!("{}: {}", dir.display(), status.describe());
        if corpus.is_empty() {
            eprintln!(
                "(no PDFs and no Markdown here yet — put some in and /build, or /open another \
                 directory)"
            );
        } else if status.summarized == 0 {
            eprintln!(
                "(nothing is ready to query yet — /build makes the summaries a question needs)"
            );
        }
        self.dir = dir.to_path_buf();
        self.last_run = None;
        Ok(())
    }

    // --- the library, asked -------------------------------------------------

    fn ask(&mut self, question: &str) -> Result<(), String> {
        let args = self.query_args(question, false);
        if let Some(Some(out)) = self.until_interrupted(query::run_async(&args))? {
            self.last_run = Some(out);
        }
        Ok(())
    }

    fn plan(&self, question: &str) -> Result<(), String> {
        if question.is_empty() {
            return Err("/plan needs a question".to_string());
        }
        let args = self.query_args(question, true);
        self.until_interrupted(query::run_async(&args)).map(|_| ())
    }

    fn query_args(&self, question: &str, plan: bool) -> QueryArgs {
        QueryArgs {
            dir: self.dir.clone(),
            question: question.to_string(),
            llm: self.llm.clone(),
            output: None,
            plan,
            reduce: self.reduce,
            answer: self.answer,
        }
    }

    /// The answering stage alone, over the last run's list: what a question
    /// costs once the library has already been read for it.
    fn answer(&mut self, question: &str) -> Result<(), String> {
        if question.is_empty() {
            return Err("/answer needs a question".to_string());
        }
        let run = self.resolve_run("")?;
        let (file, list) = read_list(&run)?;
        eprintln!(
            "answer: from {file} in {}, with {}",
            name_of(&run),
            self.llm.describe()
        );
        let options = self.llm.options();
        let reply = self.until_interrupted(async {
            options
                .answer(question, &list)
                .await
                .map_err(|e| e.to_string())
        })?;
        let Some(reply) = reply else {
            return Ok(());
        };
        let path = run.join(format!("followup-{}.md", query::run_name(question)));
        std::fs::write(&path, format!("# {question}\n\n{}\n", reply.value))
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        eprintln!("{}", reply.describe());
        eprintln!("wrote {}", path.display());
        println!("{}", reply.value);
        Ok(())
    }

    // --- the other subcommands, as they are --------------------------------

    fn build(&mut self, rest: &str) -> Result<(), String> {
        let mut argv = vec!["build".to_string(), self.dir.display().to_string()];
        argv.extend(self.llm.flags());
        argv.extend(split_words(rest)?);
        let args = match parsed(argv)? {
            Some(Command::Build(args)) => *args,
            _ => return Ok(()),
        };
        let interrupt = self.interrupt.clone();
        self.until_interrupted(build::run_async(&args, move || interrupt.is_set()))
            .map(|_| ())
    }

    fn convert(&self, rest: &str) -> Result<(), String> {
        let mut argv = vec!["convert".to_string()];
        argv.extend(split_words(rest)?);
        let invocation = match parsed(argv)? {
            Some(Command::Convert(invocation)) => *invocation,
            _ => return Ok(()),
        };
        // Synchronous, so Ctrl-C cannot stop it from here; it does stop the
        // OCR workers, which fails the conversion, which comes back here.
        self.interrupt.reset();
        convert::run(invocation)
    }

    // --- the runs so far ---------------------------------------------------

    fn runs(&self) -> Result<(), String> {
        let runs = self.list_runs()?;
        if runs.is_empty() {
            eprintln!("no runs yet — ask a question first");
            return Ok(());
        }
        for (i, run) in runs.iter().enumerate() {
            println!("{:>3}  {}  ({})", i + 1, run.name, run.state);
            println!("     {}", run.question);
        }
        Ok(())
    }

    fn show(&self, selector: &str) -> Result<(), String> {
        let run = self.resolve_run(selector)?;
        let answer = run.join("answer.md");
        if answer.is_file() {
            eprintln!("{}", answer.display());
            print!("{}", read(&answer)?);
            return Ok(());
        }
        let (file, list) = read_list(&run)?;
        eprintln!("{} has no answer; its {file}:", name_of(&run));
        print!("{list}");
        Ok(())
    }

    fn points(&self, selector: &str) -> Result<(), String> {
        let run = self.resolve_run(selector)?;
        let (file, list) = read_list(&run)?;
        eprintln!("{}", run.join(file).display());
        print!("{list}");
        Ok(())
    }

    /// Every run under the library, newest first.
    fn list_runs(&self) -> Result<Vec<Run>, String> {
        list_runs(&self.dir.join(query::QUERIES_DIR))
    }

    /// The run `selector` names: nothing for the last one — this session's,
    /// or else the newest on disk — a number as `/runs` counts them, or a
    /// name or unique prefix of one.
    fn resolve_run(&self, selector: &str) -> Result<PathBuf, String> {
        if selector.is_empty()
            && let Some(run) = &self.last_run
        {
            return Ok(run.clone());
        }
        let runs = self.list_runs()?;
        select_run(&runs, selector).map(|run| run.path.clone())
    }

    // --- settings ----------------------------------------------------------

    fn set(&mut self, rest: &str) -> Result<(), String> {
        let words = split_words(rest)?;
        let (name, value) = match words.as_slice() {
            [] => {
                self.settings();
                return Ok(());
            }
            [name, value] => (name.trim_start_matches("--"), value.as_str()),
            [name] => return Err(format!("/set {name} needs a value")),
            _ => return Err("/set takes one name and one value".to_string()),
        };
        match name {
            "reduce" => self.reduce = parse_switch(value)?,
            "answer" => self.answer = parse_switch(value)?,
            "model" if value == "default" => self.llm.model = None,
            _ => {
                let mut argv = std::iter::once(value.to_string());
                if !self.llm.take(&format!("--{name}"), &mut argv)? {
                    return Err(format!("no setting named {name}; /set lists them"));
                }
            }
        }
        self.settings();
        Ok(())
    }

    fn settings(&self) {
        let key = self.llm.provider.api_key_env();
        let key_state = match std::env::var_os(key) {
            Some(value) if !value.is_empty() => "set",
            _ => "NOT SET",
        };
        let model = match &self.llm.model {
            Some(model) => model.clone(),
            None => format!(
                "{} (the provider's default)",
                self.llm.options().resolved_model()
            ),
        };
        println!("library         {}", self.dir.display());
        println!(
            "provider        {} ({key} is {key_state})",
            self.llm.provider.name().to_ascii_lowercase()
        );
        println!("model           {model}");
        println!("concurrency     {}", self.llm.concurrency);
        println!("context-tokens  {}", self.llm.context_tokens);
        println!("max-tokens      {}", self.llm.max_tokens);
        println!("retries         {}", self.llm.retries);
        println!("reduce          {}", on_off(self.reduce));
        println!("answer          {}", on_off(self.answer));
    }

    // --- running things ----------------------------------------------------

    /// Run `work` on the session's runtime, giving it up if Ctrl-C comes
    /// first. `None` means it did: whatever `work` had in flight went with
    /// it, and a line has said so.
    fn until_interrupted<T>(
        &self,
        work: impl Future<Output = Result<T, String>>,
    ) -> Result<Option<T>, String> {
        self.interrupt.reset();
        let outcome = self.runtime.block_on(async {
            tokio::select! {
                biased;
                () = self.interrupt.wait() => None,
                result = work => Some(result),
            }
        });
        match outcome {
            Some(result) => result.map(Some),
            None => {
                progress::clear_line();
                eprintln!("stopped");
                Ok(None)
            }
        }
    }
}

/// Ctrl-C as a flag: set by the signal, cleared before each command, read by
/// whatever is running — polled by a future for the async stages, asked
/// between PDFs by the conversion loop.
///
/// At the prompt no signal is raised at all: the line editor has the
/// terminal in raw mode and reports the keystroke itself.
#[derive(Clone)]
struct Interrupt(Arc<AtomicBool>);

impl Interrupt {
    fn install() -> Result<Self, String> {
        let flag = Arc::new(AtomicBool::new(false));
        let handler = flag.clone();
        ctrlc::set_handler(move || handler.store(true, Ordering::SeqCst))
            .map_err(|error| format!("could not take Ctrl-C: {error}"))?;
        Ok(Self(flag))
    }

    fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }

    fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Resolves once Ctrl-C has arrived. A signal handler can do nothing but
    /// set a flag, so this polls it; the interval is what a keystroke feels.
    async fn wait(&self) {
        while !self.is_set() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// One run under `.kb-agent/queries`, as `/runs` lists it.
#[derive(Debug)]
struct Run {
    name: String,
    path: PathBuf,
    question: String,
    /// How far it got, from the files it left: `answer`, `list`, `raw list`
    /// or `unfinished`.
    state: &'static str,
}

fn list_runs(queries: &Path) -> Result<Vec<Run>, String> {
    let entries = match std::fs::read_dir(queries) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("could not read {}: {error}", queries.display())),
    };
    let mut runs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("could not read {}: {e}", queries.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let question = std::fs::read_to_string(path.join("question.md"))
            .ok()
            .and_then(|text| text.lines().next().map(str::to_string))
            .unwrap_or_default();
        let state = if path.join("answer.md").is_file() {
            "answer"
        } else if path.join("points.md").is_file() {
            "list"
        } else if path.join("points.raw.md").is_file() {
            "raw list"
        } else {
            "unfinished"
        };
        runs.push(Run {
            name: name_of(&path),
            path,
            question,
            state,
        });
    }
    // Names start with the time, so newest first is reverse name order.
    runs.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(runs)
}

/// See [`Session::resolve_run`].
fn select_run<'a>(runs: &'a [Run], selector: &str) -> Result<&'a Run, String> {
    if runs.is_empty() {
        return Err("no runs yet — ask a question first".to_string());
    }
    if selector.is_empty() {
        return Ok(&runs[0]);
    }
    // A number that counts a run is one; a name starts with a date, which
    // is also a number, so the count only wins where it can mean something.
    if let Some(run) = selector
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .and_then(|i| runs.get(i))
    {
        return Ok(run);
    }
    if let Some(run) = runs.iter().find(|run| run.name == selector) {
        return Ok(run);
    }
    let mut matching = runs.iter().filter(|run| run.name.starts_with(selector));
    match (matching.next(), matching.next()) {
        (Some(run), None) => Ok(run),
        (None, _) => Err(format!(
            "no run {} {selector}; /runs lists them",
            if selector.bytes().all(|b| b.is_ascii_digit()) {
                "numbered or named"
            } else {
                "named"
            }
        )),
        (Some(_), Some(_)) => Err(format!(
            "{selector} is the start of more than one run's name; /runs lists them"
        )),
    }
}

/// The list a run left — the reduced one, else the raw one — with the file
/// it came from.
fn read_list(run: &Path) -> Result<(&'static str, String), String> {
    for file in ["points.md", "points.raw.md"] {
        let path = run.join(file);
        if path.is_file() {
            let list = read(&path)?;
            if list.trim().is_empty() {
                return Err(format!(
                    "{} is empty — no source had anything to say",
                    path.display()
                ));
            }
            return Ok((file, list));
        }
    }
    Err(format!(
        "{} has no list — it stopped before the reading finished",
        name_of(run)
    ))
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

/// The last component of a path, as text.
fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// What the prompt calls the library: the directory's own name, resolved
/// so that `.` reads as something.
fn prompt_name(dir: &Path) -> String {
    dir.canonicalize()
        .ok()
        .and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| dir.display().to_string())
}

/// Run `argv` through the command-line parser. Help and the version are
/// printed here and come back as `None`; everything else is the caller's.
fn parsed(argv: Vec<String>) -> Result<Option<Command>, String> {
    match cli::parse(argv.into_iter())? {
        Command::Help(text) => {
            println!("{text}");
            Ok(None)
        }
        Command::Version => {
            println!("kb-agent {}", env!("CARGO_PKG_VERSION"));
            Ok(None)
        }
        command => Ok(Some(command)),
    }
}

/// Split a command's line into words the way a shell would, as far as
/// quoting goes: `"…"` and `'…'` hold a word together, and a backslash
/// keeps the character after it.
fn split_words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some('"') if c == '\\' => match chars.next() {
                Some(next @ ('"' | '\\')) => word.push(next),
                Some(next) => {
                    word.push('\\');
                    word.push(next);
                }
                None => word.push('\\'),
            },
            Some(_) => word.push(c),
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    in_word = true;
                }
                '\\' => {
                    if let Some(next) = chars.next() {
                        word.push(next);
                        in_word = true;
                    }
                }
                c if c.is_whitespace() => {
                    if in_word {
                        words.push(std::mem::take(&mut word));
                        in_word = false;
                    }
                }
                c => {
                    word.push(c);
                    in_word = true;
                }
            },
        }
    }
    if quote.is_some() {
        return Err("a quote was opened and not closed".to_string());
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

fn parse_switch(value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "on" | "yes" | "true" | "1" => Ok(true),
        "off" | "no" | "false" | "0" => Ok(false),
        other => Err(format!("expected on or off, got {other:?}")),
    }
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn history_path(dir: &Path) -> PathBuf {
    dir.join(HISTORY_FILE)
}

fn load_history(editor: &mut DefaultEditor, dir: &Path) {
    // A library that has never been chatted with has no history to load.
    let _ = editor.load_history(&history_path(dir));
}

fn save_history(editor: &mut DefaultEditor, dir: &Path) {
    let path = history_path(dir);
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        eprintln!("warning: could not keep the history: {error}");
        return;
    }
    if let Err(error) = editor.save_history(&path) {
        eprintln!("warning: could not write {}: {error}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_split_on_whitespace_and_hold_together_in_quotes() {
        let words = split_words(r#"--focus "keep every date" --ocr off 'a b' c\ d"#).unwrap();
        assert_eq!(
            words,
            ["--focus", "keep every date", "--ocr", "off", "a b", "c d"]
        );
        assert_eq!(split_words("").unwrap(), Vec::<String>::new());
        assert_eq!(split_words("  one  ").unwrap(), ["one"]);
        assert_eq!(split_words(r#""""#).unwrap(), [""], "an empty quoted word is a word");
        assert_eq!(split_words(r#""a \"b\" c""#).unwrap(), ["a \"b\" c"]);
        assert_eq!(split_words(r"'no \escape'").unwrap(), [r"no \escape"]);
        assert!(split_words(r#""open"#).is_err());
    }

    #[test]
    fn switches_read_the_usual_spellings() {
        assert!(parse_switch("on").unwrap() && parse_switch("Yes").unwrap());
        assert!(!parse_switch("off").unwrap() && !parse_switch("0").unwrap());
        assert!(parse_switch("maybe").is_err());
    }

    #[test]
    fn runs_list_newest_first_and_resolve_by_number_name_or_prefix() {
        let dir = std::env::temp_dir().join(format!(
            "kb-agent-chat-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let queries = dir.join(query::QUERIES_DIR);
        let make = |name: &str, question: &str, files: &[&str]| {
            let run = queries.join(name);
            std::fs::create_dir_all(&run).unwrap();
            std::fs::write(run.join("question.md"), format!("{question}\n")).unwrap();
            for file in files {
                std::fs::write(run.join(file), "- a point [x]\n").unwrap();
            }
        };
        make("20260901-100000-older", "older?", &["points.raw.md", "points.md", "answer.md"]);
        make("20260902-100000-newer", "newer?", &["points.raw.md"]);
        make("20260902-110000-newest", "newest?", &[]);
        std::fs::write(queries.join("stray.txt"), "not a run").unwrap();

        let runs = list_runs(&queries).unwrap();
        let names: Vec<_> = runs.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            ["20260902-110000-newest", "20260902-100000-newer", "20260901-100000-older"]
        );
        assert_eq!(runs[0].state, "unfinished");
        assert_eq!(runs[1].state, "raw list");
        assert_eq!(runs[2].state, "answer");
        assert_eq!(runs[2].question, "older?");

        assert_eq!(select_run(&runs, "").unwrap().name, runs[0].name);
        assert_eq!(select_run(&runs, "3").unwrap().name, runs[2].name);
        assert_eq!(select_run(&runs, "20260901").unwrap().name, runs[2].name);
        assert_eq!(select_run(&runs, "20260902-100000-newer").unwrap().name, runs[1].name);
        assert!(select_run(&runs, "0").is_err());
        assert!(select_run(&runs, "4").unwrap_err().contains("numbered or named 4"));
        assert!(select_run(&runs, "20260902").is_err(), "two runs start with it");
        assert!(select_run(&runs, "nope").is_err());
        assert!(select_run(&[], "").is_err());

        let (file, list) = read_list(&runs[2].path).unwrap();
        assert_eq!(file, "points.md");
        assert_eq!(list, "- a point [x]\n");
        assert_eq!(read_list(&runs[1].path).unwrap().0, "points.raw.md");
        assert!(read_list(&runs[0].path).is_err());

        assert!(list_runs(&dir.join("never")).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_prompt_names_the_directory() {
        assert_eq!(prompt_name(Path::new("/definitely/not/here/lib")), "/definitely/not/here/lib");
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(prompt_name(Path::new(".")), name_of(&cwd));
    }
}
