//! Outcome counts, to show whether a run is still doing real work.

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use crate::Outcome;

/// Counts outcomes and reports them every 15 seconds, as a Unix time
/// followed by name and count pairs for the inputs since the last report:
/// appended to `file` when given, which is safe from several processes at
/// once, else to standard error.
pub fn count(outcome: &Outcome, file: Option<&Path>) {
    static PENDING: Mutex<Option<(Instant, [u64; 6])>> = Mutex::new(None);
    let mut pending = PENDING.lock().unwrap();
    let (since, counts) = pending.get_or_insert_with(|| (Instant::now(), [0; 6]));
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
fn line(time: u64, counts: &[u64; 6]) -> String {
    let mut line = time.to_string();
    for (name, n) in Outcome::NAMES.iter().zip(counts) {
        line += &format!(" {name} {n}");
    }
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_pairs_names_with_counts() {
        assert_eq!(
            line(1700000000, &[1, 2, 3, 4, 5, 6]),
            "1700000000 parse-rejected 1 skipped 2 values 3 caught 4 uncaught 5 diverged 6\n"
        );
    }
}
