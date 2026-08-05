---
name: github-investigation
description:
  Create local follow-up and dispatch an isolated investigation for a new GitHub
  issue or pull request from a discovered repository.
---

# GitHub investigation

Use these skills for command syntax:

- `/Users/raine/code/aven/src/skill.md`
- `/Users/raine/.claude/skills/workmux/SKILL.md`

1. Match `owner/repository` against the verified project inventory. Use a
   verified unregistered candidate without rediscovery and add its canonical
   path to the registry. Otherwise verify a likely repository-name path beneath
   the project roots. Keep an unmatched repository unassigned.
2. Reuse the Aven task for the repository and issue or pull request identity.
   For a new task, prefer `Issue #<number>: <concise issue title>` or
   `PR #<number>: <concise pull request title>`. Put the URL, request, and
   inferred priority in its description.
3. Reuse workmux only when `priorHandling.investigationHandle` identifies the
   investigation. Treat every separate issue or pull request as a separate
   investigation.
4. Dispatch `workmux add` from the matched repository with a concise name and
   `--parent-session <repository-directory-name>`. Use the remote default branch
   with `--base` for an issue and `--pr <url>` for a pull request. Pass this
   single-line prompt through `-p`:

   ```text
   /investigate <url>
   ```

   Do not copy notification or discussion content into the prompt.
