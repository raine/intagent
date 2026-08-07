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
3. When `priorHandling.investigationHandle` resolves to an existing workmux
   agent, send the update there regardless of whether its status is `working`,
   `waiting`, or `done`. Do not create another worktree or invoke `/investigate`
   again for the same issue or pull request. Send this prompt:

   ```text
   New GitHub activity is available for this <issue-or-pull-request>: <url>. Reassess it using the existing investigation context. Investigate the activity, perform any additional verification it warrants, update your conclusions and recommended next action, and report only decision-relevant findings.
   ```

   Create a replacement investigation only when the prior handle is unavailable.

4. Dispatch `workmux add` from the matched repository with a concise name and
   `--parent-session <repository-directory-name>`. Use the remote default branch
   with `--base` for an issue and `--pr <url>` for a pull request. Pass this
   single-line prompt through `-p`:

   ```text
   /investigate <url>
   ```

   Do not copy notification or discussion content into the prompt.
