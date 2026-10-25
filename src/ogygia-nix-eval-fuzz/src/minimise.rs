//! Shrinking the corpus to the inputs that add coverage, in parallel.

use std::collections::HashSet;
use std::ffi::CString;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::num::NonZero;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;

use sha2::Digest;

/// What [`minimise`] did.
#[derive(Debug, Default, PartialEq)]
pub struct Minimised {
    /// Distinct inputs merged.
    pub inputs: usize,
    /// Inputs in the corpus after.
    pub kept: usize,
    /// Inputs set aside after because they crashed, hung or ran out of
    /// memory.
    pub crashed: usize,
}

/// Replace the corpus in `corpus` with the inputs that add coverage, and
/// `crashed` with those that crashed, hung or ran out of memory.
///
/// The inputs are those in `corpus`, `crashed` and each of `extra`, by
/// file name. They are split at random into up to `jobs` shards, and
/// `merge` is called on each at once with the shard's directory, an empty
/// directory to write the shard's inputs that add coverage to, and the
/// path to write libFuzzer's merge control file to.
/// The inputs the control file shows as started but not finished are those
/// that crashed, hung or ran out of memory.
///
/// `corpus` and `crashed` must be on the same file system. If any `merge`
/// fails, or `corpus` or `crashed` holds anything other than files, both
/// are left as they were, and at no point is an input in neither. Must
/// not run while anything uses them.
pub fn minimise(
    corpus: &Path,
    crashed: &Path,
    extra: &[&Path],
    jobs: NonZero<usize>,
    merge: impl Fn(&Path, &Path, &Path) -> std::io::Result<()> + Sync,
) -> std::io::Result<Minimised> {
    let work = tempfile::Builder::new()
        .prefix("ogygia-nix-eval-fuzz-minimise-")
        .tempdir()?;
    let result = minimise_in(corpus, crashed, extra, work.path(), jobs, merge);
    let cleaned = work.close();
    let summary = result?;
    cleaned?;
    Ok(summary)
}

fn minimise_in(
    corpus: &Path,
    crashed: &Path,
    extra: &[&Path],
    work: &Path,
    jobs: NonZero<usize>,
    merge: impl Fn(&Path, &Path, &Path) -> std::io::Result<()> + Sync,
) -> std::io::Result<Minimised> {
    std::fs::create_dir_all(crashed)?;
    let mut names = HashSet::new();
    let mut inputs = Vec::new();
    for dir in [corpus, crashed].into_iter().chain(extra.iter().copied()) {
        for input in files(dir)? {
            if names.insert(input.file_name().unwrap().to_owned()) {
                inputs.push(input);
            }
        }
    }
    if inputs.is_empty() {
        return Ok(Minimised::default());
    }
    // A fresh split each time, so inputs that duplicate each other's
    // coverage from different shards eventually share one.
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_le_bytes();
    let mut split = vec![Vec::new(); jobs.get().min(inputs.len())];
    for input in &inputs {
        let hash = sha2::Sha256::new()
            .chain_update(seed)
            .chain_update(input.file_name().unwrap().as_bytes())
            .finalize();
        let k = u64::from_le_bytes(hash[..8].try_into().unwrap()) % split.len() as u64;
        split[k as usize].push(input);
    }
    let mut shards = Vec::new();
    for (k, inputs) in split.iter().enumerate().filter(|(_, i)| !i.is_empty()) {
        let shard = Shard::new(work, k)?;
        for input in inputs {
            link_or_copy(input, &shard.inputs.join(input.file_name().unwrap()))?;
        }
        shards.push(shard);
    }
    std::thread::scope(|s| {
        let merges: Vec<_> = shards
            .iter()
            .map(|shard| s.spawn(|| merge(&shard.inputs, &shard.kept, &shard.control)))
            .collect();
        merges
            .into_iter()
            .try_for_each(|m| m.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
    })?;

    // The exchanges need the new directories on the corpus's file system.
    let stage = tempfile::Builder::new()
        .prefix(".minimise-")
        .tempdir_in(corpus.parent().unwrap_or(Path::new(".")))?;
    let new_corpus = stage.path().join("corpus");
    let new_crashed = stage.path().join("crashed");
    std::fs::create_dir(&new_corpus)?;
    std::fs::create_dir(&new_crashed)?;
    for shard in &shards {
        for input in files(&shard.kept)? {
            link_or_copy(&input, &new_corpus.join(input.file_name().unwrap()))?;
        }
        for name in unfinished(&shard.control)? {
            link_or_copy(&shard.inputs.join(&name), &new_crashed.join(&name))?;
        }
    }
    let summary = Minimised {
        inputs: inputs.len(),
        kept: files(&new_corpus)?.len(),
        crashed: files(&new_crashed)?.len(),
    };
    // The new crashed inputs first: until the corpus follows, an input is
    // in both rather than neither.
    exchange(&new_crashed, crashed)?;
    if let Err(e) = exchange(&new_corpus, corpus) {
        exchange(&new_crashed, crashed)?;
        return Err(e);
    }
    stage.close()?;
    Ok(summary)
}

/// The names of the inputs the merge control file `control` shows as
/// started but not finished, failing if any was never started.
fn unfinished(control: &Path) -> std::io::Result<Vec<OsString>> {
    let malformed = || std::io::Error::other(format!("{} is malformed", control.display()));
    let text = std::fs::read(control)?;
    let mut lines = text.split(|&b| b == b'\n');
    let count: usize = lines
        .next()
        .and_then(|l| std::str::from_utf8(l).ok()?.parse().ok())
        .ok_or_else(malformed)?;
    lines.next().ok_or_else(malformed)?;
    let names: Vec<&OsStr> = lines
        .by_ref()
        .take(count)
        .map(|l| Path::new(OsStr::from_bytes(l)).file_name())
        .collect::<Option<_>>()
        .filter(|n: &Vec<_>| n.len() == count)
        .ok_or_else(malformed)?;
    let mut started = vec![false; count];
    let mut finished = vec![false; count];
    for line in lines.filter(|l| !l.is_empty()) {
        let mut words = std::str::from_utf8(line)
            .map_err(|_| malformed())?
            .split(' ');
        let marked = match words.next() {
            Some("STARTED") => &mut started,
            Some("FT") => &mut finished,
            Some("COV") => continue,
            _ => return Err(malformed()),
        };
        let i: usize = words
            .next()
            .and_then(|i| i.parse().ok())
            .ok_or_else(malformed)?;
        *marked.get_mut(i).ok_or_else(malformed)? = true;
    }
    if started.contains(&false) {
        return Err(std::io::Error::other(format!(
            "{} shows inputs never started",
            control.display()
        )));
    }
    Ok(names
        .into_iter()
        .zip(finished)
        .filter(|(_, finished)| !finished)
        .map(|(name, _)| name.to_owned())
        .collect())
}

struct Shard {
    inputs: PathBuf,
    kept: PathBuf,
    control: PathBuf,
}

impl Shard {
    fn new(work: &Path, k: usize) -> std::io::Result<Shard> {
        let dir = work.join("shards").join(k.to_string());
        let shard = Shard {
            inputs: dir.join("inputs"),
            kept: dir.join("kept"),
            control: dir.join("control"),
        };
        for d in [&shard.inputs, &shard.kept] {
            std::fs::create_dir_all(d)?;
        }
        Ok(shard)
    }
}

/// The files in `dir`, failing if it holds anything else.
fn files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            return Err(std::io::Error::other(format!(
                "{} is not a file",
                entry.path().display()
            )));
        }
        files.push(entry.path());
    }
    Ok(files)
}

/// Hard-link `from` to `to`, or copy it where that is refused, such as
/// across file systems or from the Nix store.
fn link_or_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::hard_link(from, to).or_else(|_| {
        // A new file rather than `std::fs::copy`, so that its permissions
        // come from where it lands, such as a default ACL, not from `from`.
        let mut to = std::fs::File::create(to)?;
        std::io::copy(&mut std::fs::File::open(from)?, &mut to).map(drop)
    })
}

/// Swap the directories `a` and `b` atomically.
fn exchange(a: &Path, b: &Path) -> std::io::Result<()> {
    let a = CString::new(a.as_os_str().as_bytes())?;
    let b = CString::new(b.as_os_str().as_bytes())?;
    // SAFETY: both are NUL-terminated paths that outlive the call.
    let ret = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOBS: NonZero<usize> = NonZero::new(4).unwrap();

    struct Dirs {
        tmp: tempfile::TempDir,
        corpus: PathBuf,
        crashed: PathBuf,
    }

    /// A corpus holding an input named after each of `inputs`, holding its
    /// name.
    fn dirs(inputs: &[&str]) -> Dirs {
        let tmp = tempfile::tempdir().unwrap();
        let corpus = tmp.path().join("corpus");
        write(&corpus, inputs);
        Dirs {
            corpus,
            crashed: tmp.path().join("crashed"),
            tmp,
        }
    }

    fn write(dir: &Path, inputs: &[&str]) {
        std::fs::create_dir_all(dir).unwrap();
        for input in inputs {
            std::fs::write(dir.join(input), input).unwrap();
        }
    }

    fn run(
        d: &Dirs,
        extra: &[&Path],
        merge: impl Fn(&Path, &Path, &Path) -> std::io::Result<()> + Sync,
    ) -> std::io::Result<Minimised> {
        minimise(&d.corpus, &d.crashed, extra, JOBS, merge)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = files(dir)
            .unwrap()
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// What is beside the corpus, files or not.
    fn beside(d: &Dirs) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(d.tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Keeps the inputs named `keep*` and crashes on those named `crash*`.
    fn fake_merge(inputs: &Path, kept: &Path, control: &Path) -> std::io::Result<()> {
        let inputs = files(inputs)?;
        let mut text = format!("{}\n0\n", inputs.len());
        for input in &inputs {
            text += &format!("{}\n", input.display());
        }
        for (i, input) in inputs.iter().enumerate() {
            let name = input.file_name().unwrap().to_str().unwrap();
            text += &format!("STARTED {i} {}\n", name.len());
            if !name.starts_with("crash") {
                text += &format!("FT {i} 1 2\nCOV {i} 3\n");
                if name.starts_with("keep") {
                    std::fs::copy(input, kept.join(name))?;
                }
            }
        }
        std::fs::write(control, text)
    }

    #[test]
    fn keeps_what_merge_keeps_and_sets_aside_what_crashed() {
        let d = dirs(&["keep1", "keep2", "drop1", "drop2", "crash1"]);
        let summary = run(&d, &[], fake_merge).unwrap();
        assert_eq!(
            summary,
            Minimised {
                inputs: 5,
                kept: 2,
                crashed: 1
            }
        );
        assert_eq!(names(&d.corpus), ["keep1", "keep2"]);
        assert_eq!(names(&d.crashed), ["crash1"]);
        assert_eq!(
            std::fs::read_to_string(d.crashed.join("crash1")).unwrap(),
            "crash1"
        );
        assert_eq!(beside(&d), ["corpus", "crashed"]);
    }

    #[test]
    fn unfinished_merge_leaves_both() {
        let d = dirs(&["keep1", "drop1"]);
        write(&d.crashed, &["crash1"]);
        let err = run(&d, &[], |inputs, _, control| {
            let n = files(inputs)?.len();
            std::fs::write(control, format!("{n}\n0\n{}\n", "x\n".repeat(n)))
        })
        .unwrap_err();
        assert!(
            err.to_string().ends_with("shows inputs never started"),
            "{err}"
        );
        assert_eq!(names(&d.corpus), ["drop1", "keep1"]);
        assert_eq!(names(&d.crashed), ["crash1"]);
    }

    #[test]
    fn merges_no_empty_shard() {
        // Five inputs in four shards at random often leave one empty.
        for _ in 0..20 {
            let d = dirs(&["keep1", "keep2", "keep3", "keep4", "keep5"]);
            let summary = run(&d, &[], |inputs, kept, control| {
                assert!(!files(inputs)?.is_empty(), "merged an empty shard");
                fake_merge(inputs, kept, control)
            })
            .unwrap();
            assert_eq!(summary.kept, 5);
        }
    }

    #[test]
    fn merges_crashed_and_extra_inputs_too() {
        let d = dirs(&["keep1", "drop1"]);
        write(&d.crashed, &["keep2", "crash1", "drop2"]);
        let seeds = d.tmp.path().join("seeds");
        write(&seeds, &["keep3", "keep1", "crash2"]);
        let summary = run(&d, &[&seeds], fake_merge).unwrap();
        assert_eq!(
            summary,
            Minimised {
                inputs: 7,
                kept: 3,
                crashed: 2
            }
        );
        assert_eq!(names(&d.corpus), ["keep1", "keep2", "keep3"]);
        assert_eq!(names(&d.crashed), ["crash1", "crash2"]);
        assert_eq!(names(&seeds), ["crash2", "keep1", "keep3"]);
    }

    #[test]
    fn failed_merge_leaves_both() {
        let d = dirs(&["keep1", "drop1", "drop2", "drop3", "drop4"]);
        write(&d.crashed, &["crash1"]);
        let err = run(&d, &[], |_, _, _| {
            Err(std::io::Error::other("merge failed"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "merge failed");
        assert_eq!(
            names(&d.corpus),
            ["drop1", "drop2", "drop3", "drop4", "keep1"]
        );
        assert_eq!(names(&d.crashed), ["crash1"]);
        assert_eq!(beside(&d), ["corpus", "crashed"]);
    }

    #[test]
    fn refuses_a_corpus_holding_more_than_files() {
        let d = dirs(&["keep1"]);
        std::fs::create_dir(d.corpus.join("sub")).unwrap();
        let err = run(&d, &[], |_, _, _| unreachable!()).unwrap_err();
        assert!(err.to_string().ends_with("sub is not a file"), "{err}");
        assert_eq!(names(&d.corpus.join("sub")), Vec::<String>::new());
        assert_eq!(beside(&d), ["corpus", "crashed"]);
    }

    #[test]
    fn ignores_a_leftover_it_cannot_remove_beside_the_corpus() {
        use std::os::unix::fs::PermissionsExt;
        let d = dirs(&["keep1"]);
        let shards = d.tmp.path().join("minimise/shards");
        std::fs::create_dir_all(shards.join("0")).unwrap();
        let mode = |m| std::fs::set_permissions(&shards, std::fs::Permissions::from_mode(m));
        mode(0o555).unwrap();
        let result = run(&d, &[], fake_merge);
        mode(0o755).unwrap();
        result.unwrap();
        assert_eq!(names(&d.corpus), ["keep1"]);
    }

    #[test]
    fn merges_outside_the_corpus_directory() {
        let d = dirs(&["keep1", "drop1"]);
        run(&d, &[], |inputs, kept, control| {
            assert!(!inputs.starts_with(d.tmp.path()), "{}", inputs.display());
            fake_merge(inputs, kept, control)
        })
        .unwrap();
        assert_eq!(names(&d.corpus), ["keep1"]);
    }

    #[test]
    fn empty_corpus_runs_no_merge() {
        let d = dirs(&[]);
        let summary = run(&d, &[], |_, _, _| unreachable!()).unwrap();
        assert_eq!(summary, Minimised::default());
    }

    #[test]
    fn merges_jobs_shards_at_once() {
        // Enough inputs that no shard is left empty.
        let inputs: Vec<String> = (0..100).map(|i| format!("keep{i}")).collect();
        let d = dirs(&inputs.iter().map(String::as_str).collect::<Vec<_>>());
        // Each merge waits for all the others, so this only returns if
        // they all run at once.
        let barrier = std::sync::Barrier::new(JOBS.get());
        let summary = run(&d, &[], |i, k, c| {
            barrier.wait();
            fake_merge(i, k, c)
        })
        .unwrap();
        assert_eq!(summary.kept, 100);
    }
}
