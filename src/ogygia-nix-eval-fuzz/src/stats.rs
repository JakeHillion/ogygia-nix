//! Outcome counts, to show whether a run is still doing real work.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Read;
use std::io::Seek;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use crate::Outcome;

/// Counts outcomes and reports them every 15 seconds, as a Unix time
/// followed by name and count pairs for the inputs since the last report:
/// appended to `file` when given, which is safe from several processes at
/// once, else to standard error.
pub fn count(outcome: &Outcome, file: Option<&Path>) {
    static PENDING: Mutex<Option<(Instant, [u64; Outcome::NAMES.len()])>> = Mutex::new(None);
    let mut pending = PENDING.lock().unwrap();
    let (since, counts) =
        pending.get_or_insert_with(|| (Instant::now(), [0; Outcome::NAMES.len()]));
    counts[outcome.index()] += 1;
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
fn line(time: u64, counts: &[u64; Outcome::NAMES.len()]) -> String {
    format!("{time} {}\n", Totals(*counts))
}

/// Outcome counts, summed over report lines.
#[derive(Default)]
struct Totals([u64; Outcome::NAMES.len()]);

impl Totals {
    fn add(&mut self, line: &str) {
        let mut words = line.split_ascii_whitespace().skip(1);
        while let (Some(name), Some(n)) = (words.next(), words.next()) {
            if let (Some(i), Ok(n)) = (
                Outcome::NAMES.iter().position(|o| *o == name),
                n.parse::<u64>(),
            ) {
                self.0[i] += n;
            }
        }
    }
}

impl std::fmt::Display for Totals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, (name, n)) in Outcome::NAMES.iter().zip(&self.0).enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{name} {n}")?;
        }
        Ok(())
    }
}

/// A run, as seen through the stats file its processes report to and the
/// findings directory they record in.
pub struct Summary {
    stats: PathBuf,
    read: u64,
    totals: Totals,
    findings: PathBuf,
    known: BTreeSet<OsString>,
}

impl Summary {
    /// Start from what `stats` and `findings` hold now; neither need exist
    /// yet.
    pub fn new(stats: &Path, findings: &Path) -> std::io::Result<Self> {
        let read = match std::fs::metadata(stats) {
            Ok(m) => m.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e),
        };
        Ok(Summary {
            stats: stats.to_owned(),
            read,
            totals: Totals::default(),
            findings: findings.to_owned(),
            known: finding_names(findings)?,
        })
    }

    /// Lines that are printable ASCII apart from the paths given to
    /// [`Summary::new`]: `new finding: <dir>` for each finding recorded
    /// since the last update, then the outcome counts reported since
    /// [`Summary::new`] and the number of findings.
    pub fn update(&mut self) -> std::io::Result<Vec<String>> {
        let current = finding_names(&self.findings)?;
        let mut lines: Vec<String> = current
            .difference(&self.known)
            .map(|name| format!("new finding: {}", self.findings.join(name).display()))
            .collect();
        self.known = current;
        match std::fs::File::open(&self.stats) {
            Ok(mut f) => {
                f.seek(std::io::SeekFrom::Start(self.read))?;
                let mut text = String::new();
                f.read_to_string(&mut text)?;
                let whole = text.rfind('\n').map_or(0, |i| i + 1);
                text[..whole].lines().for_each(|l| self.totals.add(l));
                self.read += whole as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
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
            line(1700000000, &[1, 2, 3, 4, 5, 6, 7, 8]),
            "1700000000 ignored 1 parse-rejected 2 skipped 3 limited 4 values 5 caught 6 uncaught 7 diverged 8\n"
        );
    }

    #[test]
    fn summary_reports_only_what_is_new() {
        let tmp = tempfile::tempdir().unwrap();
        let stats = tmp.path().join("stats");
        let findings = tmp.path().join("findings");
        let append = |text: &str| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&stats)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .unwrap()
        };
        append(&line(1, &[9; 8]));
        crate::record(&findings, "old", "report").unwrap();
        let mut summary = Summary::new(&stats, &findings).unwrap();

        append(&line(2, &[1, 0, 0, 0, 0, 0, 0, 1]));
        append("3 ignored 2 diverged");
        crate::record(&findings, "new", "report").unwrap();
        let new = findings.join("11507a0e2f5e69d5dfa40a62a1bd7b6e");
        assert!(new.exists());
        assert_eq!(
            summary.update().unwrap(),
            [
                format!("new finding: {}", new.display()),
                "since start: ignored 1 parse-rejected 0 skipped 0 limited 0 values 0 caught 0 uncaught 0 diverged 1; 2 findings".to_owned(),
            ]
        );

        append(" 1\n");
        crate::record(&findings, "new", "again").unwrap();
        assert_eq!(
            summary.update().unwrap(),
            [
                "since start: ignored 3 parse-rejected 0 skipped 0 limited 0 values 0 caught 0 uncaught 0 diverged 2; 2 findings"
            ]
        );
    }
}
