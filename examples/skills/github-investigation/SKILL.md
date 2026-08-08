---
name: github-investigation
description:
  Create local follow-up and dispatch an isolated investigation for a GitHub
  issue or pull request from a discovered repository.
---

# GitHub investigation

1. Follow the available Aven and investigation skills for command syntax.
2. Match `owner/repository` against the verified project inventory. Use a
   verified unregistered candidate without rediscovery and add its canonical
   path to the registry. Otherwise verify a likely repository-name path beneath
   the project roots. Keep an unmatched repository unassigned.
3. Reuse the Aven task for the repository and issue or pull request identity. For
   a new task, prefer `Issue #<number>: <concise issue title>` or
   `PR #<number>: <concise pull request title>`. Put the URL, request, and
   inferred priority in its description.
4. When the prior handling data identifies an available investigation, send the
   update there instead of creating another investigation. Include the most
   specific activity permalink present in the intake, such as a comment or
   review URL. Omit the activity when no permalink is available.
5. Create a replacement investigation only when the prior investigation is
   unavailable. Run the configured investigation command from the matched
   repository and follow the available investigation skill for branch selection
   and prompt content.
6. Keep notification and discussion content out of command arguments unless the
   configured investigation skill explicitly defines a safe transport for it.
