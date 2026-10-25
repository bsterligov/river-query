---
name: Implement from pack
description: >-
  Implements one task from a context pack compiled by river-spec.
  This repo holds no specs or agent instructions; all context arrives in the pack.
on:
  workflow_dispatch:
    inputs:
      task:
        description: "Task id, e.g. RIVER-123"
        required: true
        type: string
      pack:
        description: "Pack path inside river-spec, e.g. runs/RIVER-123/river-query"
        required: true
        type: string
      spec_ref:
        description: "river-spec commit SHA the pack was taken from"
        required: true
        type: string
permissions:
  contents: read
  pull-requests: read
engine:
  id: gemini
  model: gemini-3.5-flash   # free tier is Flash-only; Pro has no free quota
  version: "0.43.0"   # gemini-cli >=0.55 fails auth behind the gh-aw API proxy (github/gh-aw#58445)
timeout-minutes: 45
concurrency:
  job-discriminator: ${{ inputs.task }}
network:
  allowed:
    - defaults
    - github
    - rust
    - dart
steps:
  - name: Fetch context pack from river-spec
    uses: actions/checkout@v7.0.1
    with:
      repository: bsterligov/river-spec
      ref: ${{ inputs.spec_ref }}
      path: .pack-src
      sparse-checkout: ${{ inputs.pack }}
      sparse-checkout-cone-mode: false
      persist-credentials: false
  - name: Stage pack outside the repo history
    env:
      PACK: ${{ inputs.pack }}
    run: |
      mv ".pack-src/${PACK}" .pack
      rm -rf .pack-src
      echo ".pack/" >> .git/info/exclude
  - name: Install toolchain
    uses: jdx/mise-action@v4
tools:
  edit:
  bash: true
safe-outputs:
  threat-detection:
    engine: false   # gemini cannot run detection; keep the non-AI checks only
  create-pull-request:
    title-prefix: "[agent] "
    labels: [agent]
    draft: false
    preserve-branch-name: true
    allowed-branches: ["impl/*"]
    excluded-files: [".pack/**"]
    if-no-changes: "error"
    github-token-for-extra-empty-commit: ${{ secrets.RIVER_CI_TOKEN }}
---

# Implement task ${{ inputs.task }}

You are implementing one task in this repository. This repository contains only code.
Everything you need to know was compiled for you by the supervisor repo and placed in `.pack/`.
Do not look for specs, plans or agent instructions anywhere else.

## 1. Load the pack

Read, in this order:

1. `.pack/manifest.yaml` — target branch, allowed files, required checks, contracts you need and must produce.
2. `.pack/task.md` — goal, scope in and out, decisions, test approach. Scope Out is binding.
3. `.pack/context.md` — the minimum system context for this repo.
4. `.pack/memory.md` — lessons from earlier tasks in this repo. Follow them.
5. Every `.pack/skills/*/SKILL.md` — coding rules. They are mandatory.
6. `.pack/contracts/` — interface files from other repos (for example `openapi.json`).

If `manifest.yaml` is missing, or its `repo` is not `${{ github.repository }}`, stop and report it with the `missing_data` tool.

## 2. Implement

- Work on the branch named in `manifest.yaml` `branch`.
- Only change files that match `allowed-files`. Never change anything under `.pack/`.
- Implement everything in Scope In and nothing in Scope Out.
- Write the tests the test approach asks for.

## 3. Verify

Run every command in `manifest.yaml` `checks` and fix failures until all pass.
If a check cannot run in this environment, say so explicitly in the PR body; never claim it passed.

## 4. Open the pull request

Create one pull request with the `create_pull_request` tool:

- branch: the `branch` from the manifest
- title: `${{ inputs.task }}: <task title>`
- body, with exactly these sections:

```
## Summary
<what changed, 3-6 bullets>

## Checks
<each check command and its result>

## Outputs
```yaml
# one line per name in manifest `produces`, e.g. openapi: <changed paths or "unchanged">
```

## Notes for memory
<0-3 one-line lessons a future agent in this repo should know, or "none">

## Open questions
<anything the spec did not answer, or "none">
```

The supervisor reads `## Outputs` and `## Notes for memory` mechanically, so keep their format.
