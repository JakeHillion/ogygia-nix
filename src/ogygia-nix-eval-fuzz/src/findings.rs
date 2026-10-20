//! The findings directory: one subdirectory per input on which the two
//! evaluators disagree, named by a hash of the input, holding the input as
//! `input.nix` and a human-readable `report`.

use std::num::NonZero;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use sha2::Digest;

use crate::Outcome;

/// Replace `dir/name` with `contents` without readers seeing a partial file.
fn write_atomic(dir: &Path, name: &str, contents: &str) -> std::io::Result<()> {
    let tmp = dir.join(format!(".{name}.{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, dir.join(name))
}

/// Add `src`, on which the two evaluators disagree as `report` describes,
/// to the findings in `findings`. Recording an input that is already there
/// does nothing. Safe to call from several processes at once.
pub fn record(findings: &Path, src: &str, report: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(findings)?;
    let hash = sha2::Sha256::digest(src.as_bytes());
    let dir = findings.join(hex::encode(&hash[..16]));
    match std::fs::create_dir(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(e),
    }
    write_atomic(&dir, "input.nix", src)?;
    write_atomic(&dir, "report", report)
}

/// What [`recheck`] did.
#[derive(Debug, Default, PartialEq)]
pub struct Recheck {
    /// Still diverging; their reports are refreshed.
    pub kept: usize,
    /// No longer diverging, hitting a fixed limit of Nix, or missing their
    /// input, so deleted.
    pub removed: usize,
    /// Nix ran out of time, memory or stack, so left as they were.
    pub inconclusive: usize,
}

/// Run `check` on every finding in `findings` again, `jobs` at a time,
/// keeping only those that still diverge, and pass `log` progress lines
/// that are printable ASCII apart from `findings`. Must not run while
/// anything records findings there.
pub fn recheck(
    findings: &Path,
    jobs: NonZero<usize>,
    check: impl Fn(&str) -> Outcome + Sync,
    mut log: impl FnMut(&str) + Send,
) -> std::io::Result<Recheck> {
    log(&format!("rechecking findings in {}", findings.display()));
    let entries = match std::fs::read_dir(findings) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Recheck::default()),
        Err(e) => return Err(e),
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') || !entry.file_type()?.is_dir() {
            continue;
        }
        dirs.push(entry.path());
    }
    let total = dirs.len();
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let progress = Mutex::new((Recheck::default(), 0, log));
    let worker = || -> std::io::Result<()> {
        while !failed.load(Ordering::Relaxed) {
            let Some(dir) = dirs.get(next.fetch_add(1, Ordering::Relaxed)) else {
                break;
            };
            let result = recheck_one(dir, &check);
            let mut progress = progress.lock().unwrap();
            let (summary, processed, log) = &mut *progress;
            match result {
                Ok(Rechecked::Kept) => summary.kept += 1,
                Ok(Rechecked::Removed) => summary.removed += 1,
                Ok(Rechecked::Inconclusive) => summary.inconclusive += 1,
                Err(e) => {
                    failed.store(true, Ordering::Relaxed);
                    return Err(e);
                }
            }
            *processed += 1;
            if *processed % 1000 == 0 {
                log(&format!(
                    "rechecked {processed}/{total} findings ({:.1}%)",
                    *processed as f64 * 100.0 / total as f64
                ));
            }
        }
        Ok(())
    };
    std::thread::scope(|s| {
        let workers: Vec<_> = (0..jobs.get().min(total))
            .map(|_| s.spawn(worker))
            .collect();
        workers
            .into_iter()
            .try_for_each(|w| w.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
    })?;
    Ok(progress.into_inner().unwrap().0)
}

/// What [`recheck_one`] did with a finding.
enum Rechecked {
    Kept,
    Removed,
    Inconclusive,
}

fn recheck_one(dir: &Path, check: impl Fn(&str) -> Outcome) -> std::io::Result<Rechecked> {
    let Ok(src) = std::fs::read_to_string(dir.join("input.nix")) else {
        std::fs::remove_dir_all(dir)?;
        return Ok(Rechecked::Removed);
    };
    match check(&src) {
        Outcome::Diverged(report) => {
            write_atomic(dir, "report", &report)?;
            Ok(Rechecked::Kept)
        }
        Outcome::Skipped => Ok(Rechecked::Inconclusive),
        _ => {
            std::fs::remove_dir_all(dir)?;
            Ok(Rechecked::Removed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOBS: NonZero<usize> = NonZero::new(4).unwrap();

    fn finding(findings: &Path, src: &str) -> std::path::PathBuf {
        let hash = sha2::Sha256::digest(src.as_bytes());
        findings.join(hex::encode(&hash[..16]))
    }

    #[test]
    fn record_writes_input_and_report_once() {
        let tmp = tempfile::tempdir().unwrap();
        let findings = tmp.path().join("findings");
        record(&findings, "1", "first").unwrap();
        record(&findings, "1", "second").unwrap();
        record(&findings, "2", "other").unwrap();

        let dir = finding(&findings, "1");
        assert_eq!(std::fs::read_to_string(dir.join("input.nix")).unwrap(), "1");
        assert_eq!(
            std::fs::read_to_string(dir.join("report")).unwrap(),
            "first"
        );
        assert_eq!(std::fs::read_dir(&findings).unwrap().count(), 2);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            2,
            "no temporary files left"
        );
    }

    #[test]
    fn recheck_keeps_only_what_still_diverges() {
        let tmp = tempfile::tempdir().unwrap();
        let findings = tmp.path();
        for src in ["diverges", "fixed", "too slow", "too big"] {
            record(findings, src, "old report").unwrap();
        }
        std::fs::create_dir(findings.join("no-input")).unwrap();
        std::fs::create_dir(findings.join(".hidden")).unwrap();

        let summary = recheck(
            findings,
            JOBS,
            |src| match src {
                "diverges" => Outcome::Diverged("new report".to_owned()),
                "fixed" => Outcome::Value,
                "too slow" => Outcome::Skipped,
                "too big" => Outcome::Limited,
                other => panic!("unexpected input {other}"),
            },
            |_| {},
        )
        .unwrap();

        assert_eq!(
            summary,
            Recheck {
                kept: 1,
                removed: 3,
                inconclusive: 1
            }
        );
        let report = |src| std::fs::read_to_string(finding(findings, src).join("report"));
        assert_eq!(report("diverges").unwrap(), "new report");
        assert!(!finding(findings, "fixed").exists());
        assert!(!finding(findings, "too big").exists());
        assert_eq!(report("too slow").unwrap(), "old report");
        assert!(!findings.join("no-input").exists());
        assert!(findings.join(".hidden").exists());
    }

    #[test]
    fn recheck_without_findings_does_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let summary = recheck(
            &tmp.path().join("findings"),
            JOBS,
            |_| unreachable!(),
            |_| {},
        )
        .unwrap();
        assert_eq!(summary, Recheck::default());
    }

    #[test]
    fn recheck_logs_progress_every_thousand_findings() {
        let tmp = tempfile::tempdir().unwrap();
        let findings = tmp.path();
        for i in 0..2500 {
            record(findings, &i.to_string(), "report").unwrap();
        }
        let mut lines = Vec::new();
        recheck(
            findings,
            JOBS,
            |src| Outcome::Diverged(src.to_owned()),
            |line| lines.push(line.to_owned()),
        )
        .unwrap();
        assert_eq!(
            lines,
            [
                format!("rechecking findings in {}", findings.display()),
                "rechecked 1000/2500 findings (40.0%)".to_owned(),
                "rechecked 2000/2500 findings (80.0%)".to_owned(),
            ]
        );
    }

    #[test]
    fn recheck_runs_jobs_checks_at_once() {
        let tmp = tempfile::tempdir().unwrap();
        let findings = tmp.path();
        for i in 0..JOBS.get() {
            record(findings, &i.to_string(), "report").unwrap();
        }
        // Each check waits for all the others, so this only returns if
        // they all run at once.
        let barrier = std::sync::Barrier::new(JOBS.get());
        let summary = recheck(
            findings,
            JOBS,
            |src| {
                barrier.wait();
                Outcome::Diverged(src.to_owned())
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(summary.kept, JOBS.get());
    }
}
