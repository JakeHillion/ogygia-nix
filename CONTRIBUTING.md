## Commit Messages

We use a structured commit style to keep history clear and reviewable.
Each branch or pull request must contain **exactly one commit** representing the
final, polished change. This means you should update and rewrite your commit
message as the branch evolves instead of adding new commits.

### Subject line

Use:

```
CRATE_OR_AREA: what changed (one line)
```

`CRATE_OR_AREA` is usually a crate name (e.g. `ogygia`) or module area (e.g. `nixos`).
If the change spans multiple areas, use the most specific shared area
(e.g. `config`, `docs`, `ci`).

Example:

```
ogygia: add support for custom backup schedules
```

### Body

Write the body as short prose with the following structure:

- 1–3 sentences describing the **issue or behaviour** this commit is fixing.  
- 1–3 sentences describing **how** the change was implemented.  
- 1–2 sentences explaining **why** this implementation fixes the issue.

Reference example:

```
The backup scheduler did not support custom intervals, requiring users
to rely on the default daily schedule. This limitation prevented users
from implementing more frequent backups for critical data or less
frequent backups for archival purposes.

This commit adds a schedule field to the BackupConfig struct that accepts
a cron-like expression. The scheduler parses this expression using the
cron crate and calculates the next backup time based on the provided
pattern. A new validate_schedule() function checks the expression syntax
during configuration loading and returns clear error messages for invalid
patterns.

This allows users to define custom backup intervals by specifying cron
expressions in their config files, providing the flexibility needed for
different backup strategies while maintaining backward compatibility with
the default daily schedule.
```

### Test plan

Every commit message ends with a `Test plan:` block:

```
Test plan:
- All existing tests pass.
- Added new tests for <feature>.
- Verified error messages via snapshot tests.
- Manually tested <workflow>.
```

### One–commit rule

This repository follows a **one commit per branch / one commit per PR** policy.

- Do not create `fixup!` commits, `v2`, `v3`, or iterative commits.
- Instead, **amend** your single commit as your work changes.
- Before opening or updating a PR, ensure the single commit contains the
  final, polished message and complete description of the change.

### Template

The `.git-commit-template` file at the repository root provides a ready-to-use
skeleton. Enable it locally with:

```
git config commit.template "$(git rev-parse --show-toplevel)/.git-commit-template"
```
