//! Guards the contract between the installed completion shims and the binary.
//!
//! `clap_complete`'s dynamic engine sits behind an unstable feature, and the
//! protocol exercised here is the one the shims in `share/` speak. A breaking
//! upgrade otherwise surfaces as completions that silently stop producing
//! candidates, which nothing else in the build would catch.

use std::env;
use std::path::PathBuf;
use std::process::Command;

/// The binary under test.
///
/// `CARGO_BIN_EXE_ogygia` is substituted at compile time and points into the
/// build tree, which is gone by the time a nextest archive is unpacked on the
/// runner. Nextest republishes the extracted binary through the same variable
/// at run time, so that takes precedence when it is set.
fn ogygia() -> PathBuf {
    env::var_os("CARGO_BIN_EXE_ogygia")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_ogygia")))
}

#[test]
fn emits_a_registration_shim_per_shell() {
    for shell in ["bash", "zsh", "fish"] {
        let output = Command::new(ogygia())
            .env("COMPLETE", shell)
            .output()
            .expect("running ogygia");

        assert!(output.status.success(), "{shell} shim failed: {output:?}");
        let shim = String::from_utf8(output.stdout).expect("shim is utf-8");
        assert!(
            shim.contains("ogygia"),
            "{shell} shim does not reference the command: {shim}"
        );
    }
}

#[test]
fn answers_the_shim_with_filtered_candidates() {
    // `status` carries no feature gate, so this asserts the engine responds
    // rather than asserting which subcommands happen to be compiled in.
    let candidates = |word: &str| {
        let output = Command::new(ogygia())
            .env("COMPLETE", "bash")
            .env("_CLAP_COMPLETE_INDEX", "1")
            .args(["--", "ogygia", word])
            .output()
            .expect("running ogygia");

        assert!(output.status.success(), "completion failed: {output:?}");
        String::from_utf8(output.stdout).expect("candidates are utf-8")
    };

    assert!(candidates("").lines().any(|line| line == "status"));
    assert_eq!(candidates("stat").lines().collect::<Vec<_>>(), ["status"]);
}
