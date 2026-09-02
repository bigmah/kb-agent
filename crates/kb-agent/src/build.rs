//! `kb-agent build`: every PDF into Markdown, every Markdown into a summary.

use std::process::ExitCode;

use kb::{ConvertOptions, Corpus, SummarizeOptions};

use crate::cli::BuildArgs;
use crate::progress;

pub fn run(args: BuildArgs) -> Result<ExitCode, String> {
    // Must precede anything that could spawn a thread — see `convert.rs`.
    pdf_extractor::init();

    let mut corpus = Corpus::scan(&args.dir).map_err(|e| e.to_string())?;
    let status = corpus.status();
    eprintln!("{}: {}", args.dir.display(), status.describe());
    if corpus.is_empty() {
        return Err(format!(
            "{} holds no PDFs and no Markdown — nothing to build",
            args.dir.display()
        ));
    }

    let to_convert = if args.reconvert {
        status.pdfs
    } else {
        status.unconverted
    };
    let to_summarize = if !args.summaries {
        0
    } else if args.force {
        status.summarized + status.unsummarized + to_convert
    } else {
        status.unsummarized + to_convert
    };
    eprintln!(
        "build: {} to convert, {} to summarize with {}",
        to_convert,
        to_summarize,
        args.llm.describe()
    );
    if args.plan {
        return Ok(ExitCode::SUCCESS);
    }

    let mut failures = 0;

    if to_convert > 0 {
        let options = ConvertOptions::new()
            .extractor(args.extractor.clone().progress(progress::ocr()))
            .force(args.reconvert)
            .progress(progress::stages());
        let report = corpus.convert(&options).map_err(|e| e.to_string())?;
        eprintln!("{}", report.describe());
        for (name, why) in &report.failed {
            eprintln!("  {name}: {why}");
        }
        failures += report.failed.len();
    }

    if args.summaries {
        let mut agent = args.llm.options();
        if let Some(focus) = &args.focus {
            agent = agent.focus(focus.clone());
        }
        let options = SummarizeOptions::new()
            .agent(agent)
            .concurrency(args.llm.concurrency)
            .force(args.force)
            .progress(progress::stages());
        let report = crate::block_on(corpus.summarize(&options))?.map_err(|e| e.to_string())?;
        eprintln!("{}", report.describe());
        for (name, tokens) in &report.too_large {
            eprintln!(
                "  {name}: ~{tokens} tokens, over the {}-token budget — raise --context-tokens \
                 or split the document",
                args.llm.context_tokens
            );
        }
        for name in &report.empty {
            eprintln!("  {name}: empty");
        }
        for (name, why) in &report.failed {
            eprintln!("  {name}: {why}");
        }
        failures += report.failed.len();
    }

    eprintln!("{}: {}", args.dir.display(), corpus.status().describe());
    Ok(if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
