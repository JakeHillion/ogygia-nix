---
name: fuzz-fix-loop
description: Use when running a long-lived loop over an ogygia-nix-eval-fuzz fuzzing directory that triages findings and hands each live divergence to a fresh fix agent, building a megamerge of independent fix commits. Covers starting the loop, what each tick does, the fix and placement agent prompts, handling Nix bugs and library workarounds, and when to do cleanups instead.
---

# Running the fuzz-fix loop

This skill drives `fix-fuzz-finding` repeatedly. The main chat triages
findings and coordinates. Each live divergence goes to a fresh fix
agent, which makes one reviewable commit. A placement agent then moves
that commit as close to `trunk()` as it can go, so most fixes become
independent PRs for the merge queue. Read
`.claude/skills/fix-fuzz-finding/SKILL.md` and
`src/ogygia-nix-eval-fuzz/README.md` first.

## The megamerge

Commits are kept as a set of substacks on `trunk()`, tied together by
the megamerge `M`: an empty merge commit whose parents are the tip of
every substack. The working copy is always an empty commit on `M`, so
builds and agents see every fix at once.

```
@        (empty)
○        M
├─┬─┬─╮
│ │ │ ○  F5
│ │ ○ │  F4
│ │ ○ │  F2
│ ○ │ │  F3
○ │ │ │  F1
├─┴─┴─╯
◆  trunk()
```

A commit only sits on another when it needs it. A commit that needs
commits from two substacks sits on an empty merge of exactly those
commits. It waits there until sync simplifies the merge away.

`M` is identified by its change ID, kept in `STATE/megamerge`. Never put
a bookmark on it. The commits are `trunk()..M ~ merges()`.

Loop tooling that hasn't landed yet, such as this skill, is a substack
under `M` like any other, so every agent sees it.

## Starting the loop

Ask the user for the fuzzing directory (`DIR`) if they haven't given it.
Ask whether they keep a tracking document for Nix bugs and workarounds.
Then:

1. Choose a state directory (`STATE`) in your scratchpad. It holds the
   triage cache and `M`'s change ID between ticks. Name it after `DIR`
   so a resumed loop finds it again.
2. If `STATE/megamerge` exists, check that it still names an empty
   commit with `jj log -r <M>`. Otherwise create `M` on `trunk()` and the
   tips of any substacks the user wants kept, such as unlanded loop
   tooling: `jj new 'trunk()' <tips…> --no-edit -m megamerge`. Save
   its change ID to `STATE/megamerge`. Use `jj simplify-parents -r <M>`
   to drop `trunk()` if a tip already sits on it.
3. Make sure the working copy is clean and is an empty commit on `M`
   (`jj new <M>`).
4. Start a self-paced loop with the `loop` skill, passing a prompt like
   "Run one tick of the fuzz-fix-loop skill on DIR, with state in STATE."
   Leave out the interval: each tick decides what wakes the next.

## The machines

A remote fuzzer usually runs on `DIR` all the time. After each merge it
rechecks every finding and deletes the ones that no longer diverge, so
the findings shrink over time without any help from this loop. Don't
duplicate that work locally: never run `recheck`, never check every
finding at once, and never run your own fuzzing campaign. Builds of any
size are fine.

## Each tick

1. **Sync.** If no agent is working in the repo, rebase everything under
   `M` onto the latest main, then drop commits and merges that landing
   made redundant:

   ```
   jj git fetch
   jj rebase -b <M> -d 'trunk()' --skip-emptied
   jj simplify-parents -s 'trunk()..<M>'
   jj abandon '(trunk()..<M> & empty() & ~merges()) ~ <M>'
   ```

   Landed commits keep their change IDs, so a local copy of one becomes
   empty when rebased and `--skip-emptied` drops it. A merge loses each
   parent that lands, and the last two commands remove it once one
   parent is left. Give any resulting conflicts to an agent to resolve.
2. **Build.** If anything under `M` changed, rebuild the fuzzer from the
   working copy:
   `nix build '.#ogygia-nix-eval-fuzz' -o STATE/fuzz`.
3. **Triage.** Run
   `.claude/skills/fuzz-fix-loop/triage.sh DIR/findings STATE/fuzz/bin/ogygia-nix-eval-fuzz STATE`.
   It checks a random sample of 512 findings, one at a time, and stops
   at the first that diverges for a cause not in `STATE/skips.tsv`. The
   remote prunes a finding only after its fix merges, so while fixes are
   unmerged most findings already agree; the sample bounds a tick's
   cost, at the price of finding rare divergences more slowly. Findings
   that agreed or matched a skip aren't drawn again until the fuzzer
   binary or `skips.tsv` changes, so a megamerge that stands still is
   covered a little more each tick. Run it as a tracked background
   command or with a timeout.
4. **Act on the result:**
   - On `DIVERGES <hash>`, read the report and hand the finding to a fix
     agent (below).
   - On `No unexplained divergence`, see "When nothing diverges".
5. **Schedule the next tick.** While an agent or a long triage runs, its
   completion wakes the loop; schedule a fallback wakeup of about 30
   minutes. Otherwise check again in about 30 minutes.

Only one agent writes to the repo at a time, because they all share the
one working copy. Never start a second agent while one is still running.

## The fix agent

Use a fresh general-purpose subagent for every finding. Its prompt must
give:

- the finding's path and what `check` showed, with your first guess at
  the cause if you have one;
- an instruction to read and follow `fix-fuzz-finding`, `AGENTS.md`,
  `CONTRIBUTING.md` and `.git-commit-template`;
- the reference Nix command: `nix shell '.#reference-nix' --command nix-instantiate ...`;
- the clean-room rule: never read Nix's source, or the source of any
  library Nix uses that we replicate. That means no GitHub fetches, no
  store-path source trees, no vendored copies. Docs and published papers
  are fine;
- if the cause is a Nix bug, or a Nix artefact we shouldn't match, stop
  with no changes and report: the minimal input, Nix vs ours, and why;
- no fuzzer skips;
- regression test first, confirmed failing before the fix and passing
  after;
- fix the cause, not just the one input;
- format with `nix fmt`;
- before committing, run all of `cargo test -p ogygia-nix-eval -p ogygia-nix-eval-fuzz`
  with no filter, then the full `nix flake check`;
- `M`'s change ID, and that it must make exactly one new commit whose
  only parent is `M`, mergeable on its own as one PR (see `AGENTS.md`),
  leave the working copy as an empty commit on that commit, and not
  move it or push;
- the commit rules: subject ≤72 characters including the prefix (aim for
  about 50), body per `AGENTS.md`, a test plan, and no Co-Authored-By
  line;
- if the change redirects or silences an output channel, check every
  writer to that channel, and size the manual test so that every output
  path actually fires;
- a final report that gives the outcome, the minimal input with Nix vs
  ours, the commit, and whether the fix works around rnix (and which
  mechanism) or another Rust library.

When it reports, confirm the new commit's only parent is `M` and the
working copy is clean, then start the placement agent. Do this even if
you doubt the fix, for instance because it matches behaviour that should
have been reported as a Nix bug or artefact. Every commit under `M`
waits for the user's review anyway; raise the doubt with them when
relaying the result, and never move a commit off `M` yourself. Relay the
result to the user once placement is done: the commit, what was wrong,
how it's tested, and where it was placed.

## The placement agent

Every new commit `X`, from a fix or a cleanup agent, goes through a
fresh general-purpose placement agent before the next agent starts. Its
prompt must give:

- the change IDs of `X` and `M`, and that the base is `trunk()`;
- the goal: give `X` the fewest parents it needs, so that it is as
  independent as possible;
- first try `trunk()`:
  `jj rebase -r <X> -A 'trunk()' -B <M>`. That placement stands if `X`
  has no conflicts and the full `nix flake check` passes with the
  working copy on `X`;
- otherwise, work out which of `trunk()..<M> ~ merges()` `X` needs:
  commits it conflicts without, and commits whose code or tests its
  diff or test relies on. Place `X` on the heads of that set, with
  `jj rebase -r <X> -A <parent> -B <M>`. A single head can be in the
  middle of a substack, so `X` branches off there. Several heads need an
  empty merge of exactly those commits (`jj new <heads…> --no-edit`),
  unless one already exists. Check it the same way as `trunk()`, and
  widen the set if the check fails. `M`'s parents always work, since the
  fix agent tested `X` there;
- never change `X`'s content or message, and touch no other commit
  except to create a merge for `X`;
- afterwards, run `jj simplify-parents -r <M>`, confirm `M` is empty and
  has no conflicts, and run the full `nix flake check` with the working
  copy on `M`;
- leave the working copy as an empty commit on `M`, and don't push;
- a final report that gives `X`'s parents and why it needs each, and
  both check results.

If the check on `M` fails, don't start another fix agent. A bad `M` is
the next agent's job, so that no fix agent starts on a broken base.

## Nix bugs and things not to match

When an agent reports a Nix bug or an implementation artefact:

- Reproduce it yourself first, in the fuzzer's setup. `check.rs` shows how
  Nix is run, including the writable chroot store.
- Report it to the user in the chat with the minimal input, Nix vs ours,
  and the likely upstream fix. Wait for their decision.
- Never file upstream issues or PRs yourself.
- Never add a fuzzer skip for a fixable Nix bug unless the user has
  decided to and there is an upstream issue or PR to cite.
- Add a narrow pattern to `STATE/skips.tsv` so triage stops stopping on
  it: an output regex, plus an input regex where the output alone is too
  broad. Note the reason in the third column.
- Remove a skip pattern once the cause is fixed and has reached the
  fuzzer, about an hour after the merge. Otherwise the pattern hides new
  findings that happen to match it.
- Regex (`builtins.match`, `builtins.split`) is excluded from the fuzzer
  by design. If a minimised divergence turns out to be regex, it's a
  fuzzer bug: report it.

## The tracking document

If the user keeps a tracking document, update it in the same tick as
anything worth recording. Don't batch updates:

- a new Nix bug or artefact, with its minimal input, Nix vs ours, the
  likely upstream fix, and the finding;
- a workaround for rnix lexing or parsing differently from Nix, with the
  commit and which mechanism in `compile.rs` it uses;
- a workaround for another Rust library that behaves differently from
  the one Nix uses;
- a status change: an upstream PR merged or released, or a bug that no
  longer reproduces.

## When nothing diverges

`No unexplained divergence` means every finding triage checked either
agreed or matched a known skip. It says whether that was a sample or
every finding. Only then may a tick spend time on cleanup. Never do it
while a checked finding is unexplained.

Send a cleanup agent to code touched by the last day or so of commits.
Its job is to make the code better as it is: unify duplicated logic,
remove leftovers, and simplify control flow. It is not there to make the
code correct for more untested cases. Give it these rules:

- no behaviour change;
- existing test files untouched, though new tests are welcome;
- one focused commit whose only parent is `M`, or none if nothing is
  clearly worth it;
- the full test suites and full `nix flake check` before committing;
- any behaviour difference it finds is reported, not fixed in that
  commit;
- architectural changes come back as proposals;
- tell it nothing about the fuzzer.

Its commit goes through the placement agent like a fix. A behaviour
difference it reports becomes a normal fix agent's job in a later tick.
When cleanup candidates run dry, let the loop sit quiet and wait for new
findings.

## Working alongside the user

- The user merges commits through the merge queue in any order while
  the loop adds more. That's expected: sync each tick.
- Many commits under `M` is fine. It means the remote fuzzer's pruning
  lags behind the fixes, so many findings already agree locally.
- If the user needs the repo, tell any running agent to stop touching it,
  and hold the loop until they hand it back. Then check the working copy
  and `M` before carrying on.
- Work outside the loop, such as fuzzer tooling the user asks for, goes
  in its own commit on `trunk()` and becomes a parent of `M`
  (`jj rebase -r <commit> -A 'trunk()' -B <M>`). Put the working copy
  back on `M` afterwards.
