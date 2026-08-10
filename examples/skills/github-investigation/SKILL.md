---
name: github-investigation
description:
  Handle a new GitHub issue or pull request, or substantive follow-up activity,
  with local follow-up and an isolated investigation.
---

# GitHub investigation

Use the available Aven and workmux skills for command syntax.

1. Classify the decision-relevant content introduced by the current revision
   before searching Aven, workmux, or projects. Earlier issue or pull request
   content is context that prior revisions already presented for triage.
   Acknowledgments, gratitude, agreement, reactions, and social closure with no
   new request, question, evidence, constraint, correction, or material state
   change are `no_action`. Do not update a task or inspect an investigation for
   them.
2. Match `owner/repository` against the verified project inventory. Use a
   verified unregistered candidate without rediscovery and add its canonical
   path to the registry. Otherwise verify a likely repository-name path beneath
   the project roots. Keep an unmatched repository unassigned.
3. Reuse the Aven task for the repository and issue or pull request identity.
   Append only actionable activity and durable facts. For a new task, prefer
   `Issue #<number>: <concise issue title>` or
   `PR #<number>: <concise pull request title>`. Put the URL, request, and
   inferred priority in its description.
4. For substantive follow-up activity that warrants additional investigation,
   resolve `priorHandling.investigationHandle`. When it resolves to an existing
   workmux agent, send the update there regardless of whether its status is
   `working`, `waiting`, or `done`. Do not create another worktree or invoke
   `/investigate` again for the same activity. Include the most specific activity
   permalink present in the intake, such as a comment or review URL. Omit the
   activity sentence when no permalink is available. Send this prompt:

   ```text
   New GitHub activity is available for this <issue-or-pull-request>: <entity-url>. Activity: <activity-permalink>. Reassess it using the existing investigation context. Investigate the activity, perform any additional verification it warrants, update your conclusions and recommended next action, and report only decision-relevant findings.
   ```

   Create a replacement investigation only when the current activity warrants
   additional investigation and the prior handle is confirmed unavailable. A
   lookup or command failure does not establish that the handle is unavailable.
   Handle availability selects update versus replacement and never determines
   whether investigation is warranted.

5. Dispatch `workmux add` from the matched repository for a new issue or pull
   request, or for substantive follow-up activity whose prior handle is confirmed
   unavailable. Use a concise name and
   `--parent-session <repository-directory-name>`. Use the remote default branch
   with `--base` for an issue and `--pr <url>` for a pull request. Pass this
   through `-p`:

   ```text
   /investigate <url>
   ```

   Do not copy notification or discussion content into the prompt.
