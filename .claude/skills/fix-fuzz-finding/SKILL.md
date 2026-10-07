---
name: fix-fuzz-finding
description: Use when fixing a divergence or crash found by ogygia-nix-eval-fuzz, the differential fuzzer that compares ogygia-nix-eval with nix-instantiate. Covers reading a fuzzing directory, choosing and minimising a finding, fixing it, and committing.
---

# Fixing an ogygia-nix-eval fuzz finding

`src/ogygia-nix-eval-fuzz/README.md` describes the fuzzer and is the
reference for what each step compares. Nix is the specification: a fix
makes ogygia-nix-eval do what the pinned `nix-instantiate` does, even
where Nix's behaviour looks wrong.

The rare exception is a finding where Nix is clearly acting on an
artefact of its implementation rather than the language, such as
evaluating input that it should reject. There the right fix is to make the fuzzer skip such inputs,
not to copy the behaviour. If you conclude a finding is one of these,
stop and confirm with the user before changing anything, explaining
what Nix does and why it should not be matched.

## The fuzzing directory

The user gives the directory (`DIR`). It holds:

- `DIR/findings/<hash>/input.nix`: an input on which the evaluators
  disagree, and `report`: the step that diverged (`parse`, `eval` or
  `catch`), the expression run, and both sides' output.
- `DIR/runs/<revision>/artifacts/`: inputs that crashed (`crash-*`) or
  ran out of memory (`oom-*`) in ogygia-nix-eval itself. A crash is a
  bug. An OOM only means the input passed libFuzzer's default 2 GiB RSS
  limit, which a complex but valid expression can need; if it evaluates
  correctly given more memory, it is low priority.
- `DIR/runs/<revision>/stats` and `DIR/corpus/`: not findings; ignore.

Findings are owned by the fuzzer. Never edit or delete them, and never
run `recheck` on them yourself (it must not run while a fuzzer is using
the directory).

## Choosing a finding

Pick findings at random until you reach one you understand:

```sh
ls DIR/findings | shuf -n 1
```

Best efforts are made to keep the directory up to date, but it can fall
behind the working tree, so a finding may already be fixed. Before
working on one, confirm it still diverges with the current build:

```sh
nix run .#ogygia-nix-eval-fuzz -- check DIR/findings/<hash>/input.nix
```

If it agrees, pick another.

Inputs often contain invisible bytes (vertical tab, non-breaking space),
so look at them with `xxd` before reasoning about them.

## Minimising

Reduce the input by hand to the smallest expression that still diverges,
checking each candidate against both sides. Run Nix as the version the
flake compares against, not whatever `nix-instantiate` is on `PATH`:

```sh
nix run .#ogygia-nix-eval-fuzz -- check candidate.nix   # report and non-zero exit while it diverges
nix shell --inputs-from . nixpkgs#nixVersions.latest --command nix-instantiate --parse --option pure-eval true -E '...'
nix shell --inputs-from . nixpkgs#nixVersions.latest --command nix-instantiate --eval --strict --option pure-eval true -E '...'
```

Both evaluators run in pure mode in the fuzzer, so behaviour that only
differs in pure mode (`~/` paths, `currentSystem`, `storePath`) must be
compared in pure mode. Keep the minimised input; it becomes the test.

## Writing the test

Before changing any code, add the minimised input as a test:

- An expression whose result is the same in Nix's impure mode goes in
  `src/ogygia-nix-eval/tests/cases/` as a `.nix` file; the equivalence
  harness compares it against Nix. Run it with
  `cargo test -p ogygia-nix-eval --test equiv -- <case name>`.
- One that depends on pure mode goes in the tests of
  `src/ogygia-nix-eval-fuzz/src/check.rs`, which run the fuzzer's own
  comparison. Run it with `cargo test -p ogygia-nix-eval-fuzz <test name>`.

Run the test and confirm it fails. A test that passes before the fix
does not cover the bug: rework it until it fails for the reason the
finding diverges.

## Fixing

Find the cause in `src/ogygia-nix-eval`, and fix the cause rather than
the one input: the same divergence usually has many spellings. Match
where Nix fails as well as whether it fails: a parse error must be a
compile error (`tryEval` cannot catch it), an evaluation error an
evaluation error.

## Verifying

Run the new test and confirm it now passes, then run the full suites:

```sh
cargo test -p ogygia-nix-eval -p ogygia-nix-eval-fuzz
```

## Committing

Format with `nix fmt`. Commit following `AGENTS.md` and `CONTRIBUTING.md`
as one commit, normally with the subject area `ogygia-nix-eval`. The
commit is about the bug, not the fuzzer: the body says what Nix does
that ogygia-nix-eval did not (with the minimised input), how the fix
works, and why that matches Nix. It may say the bug was found by the
fuzzer, but not how the fix changes the fuzzer's findings. The test plan
names the new test and that it failed before the fix and passes after
it, and the test commands run.
