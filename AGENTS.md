## Agent Commit Guidelines

Agents generating commits **must** follow the commit message rules from
`CONTRIBUTING.md` and use the `.git-commit-template` as the canonical format.

Agents must read:

1. The “Commit Messages” section in `CONTRIBUTING.md`.  
2. The `.git-commit-template` file.  
3. The rules in this AGENTS.md file.

### Required structure

Subject line:

```
CRATE_OR_AREA: what changed (one line)
```

Body must include:

- 1–3 sentences describing the issue being fixed.  
- 1–3 sentences describing how the change was implemented.  
- 1–2 sentences explaining why this approach fixes the issue.

### Test plan

Every commit ends with:

```
Test plan:
- Why existing tests cover this change AND/OR
- What new tests were added AND/OR
- Any manual testing that was performed
```

If the agent cannot run tests:

```
Test plan:
- Unable to run tests in this environment. Developer must verify.
```

That fallback is for tests the environment truly cannot run, and never for the
macOS app: its tests run without a person (see below). A test plan names the
commands that were run and what they showed.

### One–commit rule

This repository strictly follows a **one commit per branch / one commit per PR** model.

Agents must:

- Maintain **exactly one commit** on the branch.  
- **Amend** the commit when making changes during review.  
- Update the commit message to reflect the final, complete understanding of the
  change.  
- Never produce commits like `v2`, `v3`, `fixup!`, “address review comments”, or
  messages referencing earlier iterations.  
- Never leave stale or outdated descriptions; rewrite the message so that it
  reads as if the commit was authored perfectly the first time.

This ensures reviewers see a clean, final commit that accurately represents the
change without iteration noise.

### Additional rules

- Do **not** generate placeholder messages like “WIP”.  
- Do **not** include unrelated files in a commit.  
- Keep commit text concise, accurate, and human-readable.  
- Prefer crate-level `CRATE_OR_AREA` names unless the change spans multiple areas.

## Testing the macOS app

Changes to `app/macos/`, `src/ogygia-macos-nebulad/` or
`nixos/nebula/device.nix` are tested in the layers `app/macos/TESTING.md`
describes, by the agent, before committing:

- `nix run .#test-macos-daemon` for any change to the daemon or what it
  evaluates.
- `nix run .#test-macos-ui` for any change to the app. Open and look at the
  PNG of every pane the change touches; add a fixture scenario in
  `app/macos/Ogygia/Snapshots.swift` for any new state the change adds.
- The system test, through CI on a `test/` branch, for any change to the
  daemon's launchd handling, Nebula supervision, the utun, relays, the admin
  check or update handling, when pushing has been allowed.

Never run the daemon as root, install the app, register or load its launchd
job, or change the firewall on a Mac where `sysctl kern.hv_vmm_present` is 0.
The system test refuses to; do not work around it. Everything else these
layers run is safe on any Mac.

The only steps a test plan may leave to the developer are those
`app/macos/TESTING.md` lists under "What stays manual". Anything else that
cannot yet be tested gets a test, or a change to these layers, in the same
commit.
