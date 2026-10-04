# ogygia-nix-eval-fuzz

A differential fuzzer for ogygia-nix-eval. It generates Nix source,
evaluates it with both ogygia-nix-eval and a pinned `nix-instantiate`, and
records every input on which they disagree. The equivalence tests in
`src/ogygia-nix-eval/tests` cover the expressions someone thought to write;
this covers the rest, for as long as it is left running.

## What is compared

An input is any Nix source text, so every expression Nix can parse is a
possible input, and Nix rather than a grammar of ours decides what is
valid. Each input is checked in up to three steps, both evaluators in pure
mode:

1. **Parse.** Whether ogygia-nix-eval compiles it must match whether
   `nix-instantiate --parse` accepts it. Nix resolves variables while
   parsing, so this includes undefined variables.
2. **Evaluate.** The deeply evaluated values, as `nix-instantiate --eval
   --strict` prints them, must be equal, or both sides must fail.
3. **Catch.** When both fail, `builtins.tryEval` must catch the failure on
   both sides or on neither.

Error messages and stack traces are never compared: two failures that
`tryEval` treats alike count as agreement, whatever they say.

Nix runs first at each step, with 10 seconds and 1 GiB of address space.
An input on which it runs out of time, memory or stack says nothing about
equivalence, so it is skipped before ogygia-nix-eval sees it. Both
evaluators are pointed at a proxy that refuses connections, so fetches fail
without touching the network. Inputs that are not UTF-8 are ignored, as
ogygia-nix-eval only accepts text, and so are inputs containing a NUL byte:
Nix stops reading at the first one outside a string, so it evaluates only
a prefix of the input.

## How inputs are found

libFuzzer drives the search. It keeps an input in the corpus when it
reaches code in ogygia-nix-eval (or rnix) that no earlier input reached,
and makes new inputs by mutating and combining corpus entries. Its own
byte-level mutations, which can turn any input into any other, run
alongside mutations that replace, delete, copy, swap and splice whole nodes
and tokens of rnix's syntax tree. Those make meaningful Nix likelier but
never make any input unreachable.

The Nix package ships seed inputs to start from (the equivalence cases,
nixpkgs' `lib`, and rnix's parser tests) and a dictionary of Nix tokens and
the name of every builtin of the pinned Nix.

## Running it

The package built by the flake bakes in the pinned `nix-instantiate`, the
seeds and the source revision:

```sh
nix run .#ogygia-nix-eval-fuzz -- run DIR
```

`run DIR` rechecks the findings in `DIR` (see below), then fuzzes until
stopped, with one process per CPU unless `--jobs N` says otherwise. It
keeps:

- `DIR/corpus/`: libFuzzer's corpus. It is plain Nix source, so it stays
  useful across revisions.
- `DIR/findings/<hash>/`: one directory per input on which the evaluators
  disagree, named by a hash of the input, holding the input as `input.nix`
  and a `report` of the step that diverged, the expression evaluated, and
  both sides' full output.
- `DIR/runs/<revision>/stats`: how many inputs had each outcome, reported
  by every process every 15 seconds as a Unix time followed by name and
  count pairs. To sum the last hour:

  ```sh
  awk -v t=$(($(date +%s)-3600)) '$1>t {for(i=2;i<NF;i+=2) s[$i]+=$(i+1)} END{for(k in s) print k, s[k]}' stats
  ```

- `DIR/runs/<revision>/artifacts/`: inputs that crashed, hung or ran out of
  memory in ogygia-nix-eval itself, as libFuzzer names them.

To follow new revisions, stop `run` and start the new revision's.

### Findings

Findings are owned by this tool, not by whoever fixes them. `recheck DIR`
runs every finding again with the current build, using the same check that
found it:

- if it still diverges, it is kept and its report refreshed;
- if the evaluators now agree, it is deleted;
- if Nix runs out of time or memory, it is left as it was.

`run` does this before it starts fuzzing, so the findings shrink as fixes
land. `recheck` must not run while something is fuzzing the same
directory.

To see whether a change fixes an input, without touching the findings:

```sh
nix run .#ogygia-nix-eval-fuzz -- check DIR/findings/<hash>/input.nix
```

It prints the input's outcome, or the report and a non-zero exit status if
the evaluators still disagree.

### From the dev shell

The binary also accepts libFuzzer's own arguments, which is how cargo-fuzz
runs it:

```sh
cargo fuzz run --sanitizer none --fuzz-dir src/ogygia-nix-eval-fuzz ogygia-nix-eval-fuzz
```

There it stops at the first divergence and prints its report, unless
`--findings=DIR` says where to record findings; `--stats=FILE` sends the
outcome counts to a file rather than standard error. `nix-instantiate` is
`$OGYGIA_NIX_EVAL_NIX_INSTANTIATE`, else the path baked in at build time
from `$OGYGIA_NIX_INSTANTIATE_BIN`, else found on `PATH`.

## How far to trust a long run

Nothing is ruled out by construction: every UTF-8 text is a possible input
and Nix is the reference. What a run actually explores is another matter.

- Coverage guidance rewards inputs that reach new code in ogygia-nix-eval.
  It cannot steer towards Nix behaviour that ogygia-nix-eval does not
  implement at all, as there is no code of ours to reach; the seeds are
  what lead it there.
- New code paths stop appearing long before an evaluator's behaviour is
  exhausted, so a run whose coverage has levelled off can still find
  divergences, just more slowly.
- Most inputs do not parse. They are cheap to check, but the stats show
  how much of a run compares real evaluations; a run that only reaches
  parse errors is not exercising the evaluator.
- Impure behaviour (the clock, the environment, the filesystem outside the
  input, the network) is excluded by pure mode, and error messages are not
  compared. Divergences there need other tests.
