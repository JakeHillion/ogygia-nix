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
