//! The findings directory: one subdirectory per input on which the two
//! evaluators disagree, named by a hash of the input, holding the input as
//! `input.nix` and a human-readable `report`.

use std::path::Path;

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
    /// No longer diverging, or missing their input, so deleted.
    pub removed: usize,
    /// Nix ran out of time, memory or stack, so left as they were.
    pub inconclusive: usize,
}

/// Run `check` on every finding in `findings` again, keeping only those
/// that still diverge. Must not run while anything records findings there.
pub fn recheck(findings: &Path, check: impl Fn(&str) -> Outcome) -> std::io::Result<Recheck> {
    eprintln!("rechecking findings in {}", findings.display());
    let mut summary = Recheck::default();
    let entries = match std::fs::read_dir(findings) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(summary),
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
    for (index, dir) in dirs.into_iter().enumerate() {
        match std::fs::read_to_string(dir.join("input.nix")) {
            Ok(src) => match check(&src) {
                Outcome::Diverged(report) => {
                    write_atomic(&dir, "report", &report)?;
                    summary.kept += 1;
                }
                Outcome::Skipped => summary.inconclusive += 1,
                _ => {
                    std::fs::remove_dir_all(&dir)?;
                    summary.removed += 1;
                }
            },
            Err(_) => {
                std::fs::remove_dir_all(&dir)?;
                summary.removed += 1;
            }
        }
        let processed = index + 1;
        if processed % 1000 == 0 {
            eprintln!(
                "rechecked {processed}/{total} findings ({:.1}%)",
                processed as f64 * 100.0 / total as f64
            );
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for src in ["diverges", "fixed", "too slow"] {
            record(findings, src, "old report").unwrap();
        }
        std::fs::create_dir(findings.join("no-input")).unwrap();
        std::fs::create_dir(findings.join(".hidden")).unwrap();

        let summary = recheck(findings, |src| match src {
            "diverges" => Outcome::Diverged("new report".to_owned()),
            "fixed" => Outcome::Value,
            "too slow" => Outcome::Skipped,
            other => panic!("unexpected input {other}"),
        })
        .unwrap();

        assert_eq!(
            summary,
            Recheck {
                kept: 1,
                removed: 2,
                inconclusive: 1
            }
        );
        let report = |src| std::fs::read_to_string(finding(findings, src).join("report"));
        assert_eq!(report("diverges").unwrap(), "new report");
        assert!(!finding(findings, "fixed").exists());
        assert_eq!(report("too slow").unwrap(), "old report");
        assert!(!findings.join("no-input").exists());
        assert!(findings.join(".hidden").exists());
    }

    #[test]
    fn recheck_without_findings_does_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let summary = recheck(&tmp.path().join("findings"), |_| unreachable!()).unwrap();
        assert_eq!(summary, Recheck::default());
    }
}
