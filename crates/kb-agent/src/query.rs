//! `kb-agent query`: a question, put to every document in the library.
//!
//! Each stage's output is written as soon as the stage finishes, so a run
//! that dies in the reduction — the expensive stage — has already saved the
//! raw list it was reducing.

use std::path::{Path, PathBuf};

use kb::{Corpus, Distillation, Point, QueryOptions};

use crate::cli::QueryArgs;
use crate::progress;

/// Where a query's files go when `--output` is not given, under the library.
pub(crate) const QUERIES_DIR: &str = ".kb-agent/queries";

pub fn run(args: QueryArgs) -> Result<(), String> {
    crate::block_on(run_async(&args))?.map(|_| ())
}

/// The whole of [`run`], for a caller with a runtime of its own — the chat.
///
/// Returns where the files went, or `None` when `--plan` stopped it short of
/// sending anything. Dropping the future cancels the stage in flight; the
/// stages before it have already been written.
pub async fn run_async(args: &QueryArgs) -> Result<Option<PathBuf>, String> {
    let corpus = Corpus::scan(&args.dir).map_err(|e| e.to_string())?;
    let options = QueryOptions::new()
        .agent(args.llm.options())
        .concurrency(args.llm.concurrency)
        .reduce(args.reduce)
        .answer(args.answer)
        .progress(progress::stages());

    let plan = corpus.plan_query(&options);
    eprintln!(
        "{}: {}, with {}",
        args.dir.display(),
        plan.describe(),
        args.llm.describe()
    );
    for excluded in &plan.excluded {
        eprintln!("  excluded {}: {}", excluded.name, excluded.why);
    }
    if plan.reachable.is_empty() {
        return Err(format!(
            "nothing in {} can be reached — run `kb-agent build {}` first",
            args.dir.display(),
            args.dir.display()
        ));
    }
    if args.plan {
        return Ok(None);
    }

    let out = args
        .output
        .clone()
        .unwrap_or_else(|| args.dir.join(QUERIES_DIR).join(run_name(&args.question)));
    std::fs::create_dir_all(&out)
        .map_err(|e| format!("could not create {}: {e}", out.display()))?;
    write(&out, "question.md", &format!("{}\n", args.question))?;
    eprintln!("writing to {}", out.display());

    stages(&corpus, args, &options, &out).await?;
    Ok(Some(out))
}

async fn stages(
    corpus: &Corpus,
    args: &QueryArgs,
    options: &QueryOptions,
    out: &Path,
) -> Result<(), String> {
    let question = &args.question;

    let mask = corpus.mask(question, options).await.map_err(|e| e.to_string())?;
    write(out, "mask.md", &mask.render())?;
    eprintln!("{}", mask.describe());
    if mask.relevant_count() == 0 {
        eprintln!("no source was judged relevant; the library has nothing to say on this");
    }

    let reading = corpus.ask(question, &mask, options).await.map_err(|e| e.to_string())?;
    write(out, "points.raw.md", &Point::render_list(&reading.points))?;
    eprintln!("{}", reading.describe());

    let reduction = if args.reduce && reading.points.len() > 1 {
        eprintln!(
            "reduce: {} pairs to compare across {} points",
            kb::pairs_for(reading.points.len()),
            reading.points.len()
        );
        let reduction = kb::reduce(reading.points.clone(), options)
            .await
            .map_err(|e| e.to_string())?;
        write(out, "points.md", &Point::render_list(&reduction.points))?;
        eprintln!("{}", reduction.describe());
        Some(reduction)
    } else {
        None
    };

    let points = reduction
        .as_ref()
        .map(|r| r.points.as_slice())
        .unwrap_or(&reading.points);
    let answer = if args.answer {
        let answer = kb::answer(question, points, options)
            .await
            .map_err(|e| e.to_string())?;
        write(out, "answer.md", &format!("{}\n", answer.markdown))?;
        eprintln!("{}", answer.describe());
        Some(answer)
    } else {
        None
    };

    let distillation = Distillation {
        question: question.clone(),
        mask,
        reading,
        reduction,
        answer,
    };
    write(out, "report.md", &format!("{}\n", distillation.describe()))?;
    eprintln!("total: {}", distillation.total().describe());
    eprintln!("wrote {}", out.display());

    if let Some(answer) = &distillation.answer {
        println!("{}", answer.markdown);
    } else {
        print!("{}", Point::render_list(distillation.points()));
    }
    Ok(())
}

fn write(dir: &Path, name: &str, text: &str) -> Result<(), String> {
    let path = dir.join(name);
    std::fs::write(&path, text).map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// `20260902-141500-what-limits-throughput`: sorts by time, reads as the
/// question.
pub(crate) fn run_name(question: &str) -> String {
    format!("{}-{}", timestamp(), slug(question))
}

/// The question as a directory name: lowercase, hyphens for everything that
/// is not a letter or digit, and no longer than a glance.
fn slug(question: &str) -> String {
    let mut slug = String::new();
    let mut last_hyphen = true;
    for c in question.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
            last_hyphen = false;
        } else if !last_hyphen {
            slug.push('-');
            last_hyphen = true;
        }
        if slug.len() >= 48 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        "query".to_string()
    } else {
        slug
    }
}

/// `YYYYMMDD-HHMMSS`, UTC, from the system clock and nothing else.
fn timestamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (date, time) = (seconds / 86_400, seconds % 86_400);
    let (year, month, day) = civil_from_days(date as i64);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        time / 3_600,
        (time % 3_600) / 60,
        time % 60
    )
}

/// Days since 1970-01-01 to a calendar date. Howard Hinnant's algorithm, as
/// used by every date library; here so as not to depend on one for a file
/// name.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_becomes_a_directory_name() {
        assert_eq!(slug("What limits throughput?"), "what-limits-throughput");
        assert_eq!(slug("  ---  "), "query");
        assert_eq!(slug("Ünïcode & symbols!"), "ünïcode-symbols");
        assert!(slug(&"word ".repeat(40)).len() <= 48);
    }

    #[test]
    fn the_calendar_is_right_on_the_days_that_matter() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_698), (2026, 9, 2));
    }

    #[test]
    fn the_timestamp_has_the_shape_the_name_promises() {
        let stamp = timestamp();
        assert_eq!(stamp.len(), 15, "{stamp}");
        assert_eq!(&stamp[8..9], "-");
        assert!(stamp[..8].chars().all(|c| c.is_ascii_digit()));
    }
}
