//! Outcome counts, to show whether a run is still doing real work.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use strum::EnumCount;
use strum::IntoEnumIterator;

use crate::Outcome;
use crate::check::Kind;

/// Counts outcomes and reports them every 15 seconds, as a Unix time
/// followed by name and count pairs for the inputs since the last report:
/// appended to `file` when given, which is safe from several processes at
/// once, else to standard error.
pub fn count(outcome: &Outcome, file: Option<&Path>) {
    static PENDING: Mutex<Option<(Instant, [u64; Kind::COUNT])>> = Mutex::new(None);
    let mut pending = PENDING.lock().unwrap();
    let (since, counts) = pending.get_or_insert_with(|| (Instant::now(), [0; Kind::COUNT]));
    counts[Kind::from(outcome) as usize] += 1;
    if since.elapsed() < Duration::from_secs(15) {
        return;
    }
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let line = line(time, counts);
    match file {
        // One write per line, so lines from concurrent processes stay whole.
        Some(path) => std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(line.as_bytes()))
            .expect("writing fuzzing statistics"),
        None => eprint!("outcomes: {line}"),
    }
    *pending = None;
}

/// A report line: `time`, then each outcome's name and count.
fn line(time: u64, counts: &[u64; Kind::COUNT]) -> String {
    format!("{time} {}\n", Totals(*counts))
}

/// Outcome counts, summed over report lines.
#[derive(Default)]
struct Totals([u64; Kind::COUNT]);

impl Totals {
    fn add(&mut self, line: &str) {
        let mut words = line.split_ascii_whitespace().skip(1);
        while let (Some(name), Some(n)) = (words.next(), words.next()) {
            if let (Ok(kind), Ok(n)) = (name.parse::<Kind>(), n.parse::<u64>()) {
                self.0[kind as usize] += n;
            }
        }
    }
}

impl std::fmt::Display for Totals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, (kind, n)) in Kind::iter().zip(&self.0).enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            let name: &str = kind.into();
            write!(f, "{name} {n}")?;
        }
        Ok(())
    }
}

/// A run, as seen through the outcome counts its processes report and the
/// findings directory they record in.
pub struct Summary {
    totals: Totals,
    findings: PathBuf,
    known: BTreeSet<OsString>,
}

impl Summary {
    /// Start from what `findings` holds now, which need not exist yet.
    pub fn new(findings: &Path) -> std::io::Result<Self> {
        Ok(Summary {
            totals: Totals::default(),
            findings: findings.to_owned(),
            known: finding_names(findings)?,
        })
    }

    /// Add the counts in `line`, a line [`count`] reports.
    pub fn add(&mut self, line: &str) {
        self.totals.add(line);
    }

    /// Lines that are printable ASCII apart from the paths given to
    /// [`Summary::new`]: `new finding: <dir>` for each finding recorded
    /// since the last update, then the outcome counts
    /// [added](Summary::add) so far and the number of findings.
    pub fn update(&mut self) -> std::io::Result<Vec<String>> {
        let current = finding_names(&self.findings)?;
        let mut lines: Vec<String> = current
            .difference(&self.known)
            .map(|name| format!("new finding: {}", self.findings.join(name).display()))
            .collect();
        self.known = current;
        lines.push(format!(
            "since start: {}; {} findings",
            self.totals,
            self.known.len()
        ));
        Ok(lines)
    }
}

fn finding_names(findings: &Path) -> std::io::Result<BTreeSet<OsString>> {
    let entries = match std::fs::read_dir(findings) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(e) => return Err(e),
    };
    let mut names = BTreeSet::new();
    for entry in entries {
        let name = entry?.file_name();
        if !name.to_string_lossy().starts_with('.') {
            names.insert(name);
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_pairs_names_with_counts() {
        assert_eq!(
            line(1700000000, &[1, 2, 3, 4, 5, 6, 7]),
            "1700000000 parse-rejected 1 skipped 2 deliberate-divergence 3 values 4 caught 5 uncaught 6 diverged 7\n"
        );
    }

    #[test]
    fn summary_reports_only_what_is_new() {
        let tmp = tempfile::tempdir().unwrap();
        let findings = tmp.path().join("findings");
        crate::record(&findings, "old", "report").unwrap();
        let mut summary = Summary::new(&findings).unwrap();

        summary.add(line(2, &[1, 0, 0, 0, 0, 0, 1]).trim_end());
        crate::record(&findings, "new", "report").unwrap();
        let new = findings.join("11507a0e2f5e69d5dfa40a62a1bd7b6e");
        assert!(new.exists());
        assert_eq!(
            summary.update().unwrap(),
            [
                format!("new finding: {}", new.display()),
                "since start: parse-rejected 1 skipped 0 deliberate-divergence 0 values 0 caught 0 uncaught 0 diverged 1; 2 findings".to_owned(),
            ]
        );

        summary.add("3 parse-rejected 2 diverged 1");
        crate::record(&findings, "new", "again").unwrap();
        assert_eq!(
            summary.update().unwrap(),
            [
                "since start: parse-rejected 3 skipped 0 deliberate-divergence 0 values 0 caught 0 uncaught 0 diverged 2; 2 findings"
            ]
        );
    }
}
