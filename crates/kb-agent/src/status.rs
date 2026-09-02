//! `kb-agent status`: what is built and what is not.

use std::path::Path;

use kb::Corpus;

pub fn run(dir: &Path) -> Result<(), String> {
    let corpus = Corpus::scan(dir).map_err(|e| e.to_string())?;
    eprintln!("{}: {}", dir.display(), corpus.status().describe());
    if corpus.is_empty() {
        return Ok(());
    }
    println!("pdf  md   summary  source");
    for source in corpus.sources() {
        let mark = |present: bool| if present { "yes" } else { "-  " };
        println!(
            "{}  {}  {}      {}",
            mark(source.pdf.is_some()),
            mark(source.markdown.is_some()),
            mark(source.summary.is_some()),
            source.name
        );
    }
    Ok(())
}
