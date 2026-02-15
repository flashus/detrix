---
description: "Comprehensive code audit with per-crate reports via subagents"
allowed-tools: "Bash(run tests:*), Bash(cargo check:*), Bash(cargo clippy:*), Bash(git diff:*), Bash(git log:*), Bash(git status:*), Bash(mkdir:*), Bash(cargo udeps:*), Bash(cat:*), Bash(wc:*)"
---

You are a PROJECT MANAGER coordinating a comprehensive code audit. You do NOT read source code yourself. You dispatch subagents and compile their reports.

## Scope

The argument provided is: "$ARGUMENTS"

If the argument is empty or blank, you MUST use AskUserQuestion to ask the user BEFORE doing anything else:

```
Question: "What scope should the audit cover?"
Options:
- "Current branch" — "Audit changes on current branch vs main"
- "Uncommitted changes" — "Audit only uncommitted/staged changes"
- "Full project" — "Audit all 13 crates + clients (takes a while)"
- "Single crate" — "I'll specify which crate to audit"
```

If the user picks "Single crate", follow up: "Which crate? (e.g. detrix-core, detrix-api)"
Then use their answer as the scope.

If the argument IS provided, map it:
- `branch` — audit changes on current branch vs main
- `uncommitted` — audit uncommitted changes only
- `project` — audit entire project
- A crate name (e.g. `detrix-core`) — audit just that crate

## CRITICAL: Context Window Management

**You MUST NOT read source code files yourself.** Your job is to:
1. Set up the audit directory
2. Run automated tooling (clippy, etc.)
3. Dispatch one subagent per crate (they each get a fresh context window)
4. Read only the subagent OUTPUT FILES to compile the final report

This architecture lets us audit 13 crates without exhausting the context.

## Phase 1: Setup & Automated Checks

```bash
mkdir -p .agents/audit
```

Check which crates were already audited (for resumability):
```bash
ls .agents/audit/*.md 2>/dev/null
```

If audit files already exist, ask the user: "Found existing audit files for [crates]. Skip already-audited crates, or start fresh?"

Run automated checks and save output:
```bash
cargo clippy --all -- -D warnings 2>&1 | head -200 > .agents/audit/_clippy.txt
```

## Phase 2: Determine Crates to Audit

For scope `branch`: identify which crates have changes:
```bash
git diff main...HEAD --name-only | grep "^crates/" | cut -d/ -f2 | sort -u
```

For scope `uncommitted`:
```bash
git diff --name-only | grep "^crates/" | cut -d/ -f2 | sort -u
```

For scope `project`: all crates:
```
detrix-core, detrix-config, detrix-ports, detrix-application, detrix-storage,
detrix-dap, detrix-lsp, detrix-output, detrix-logging, detrix-api, detrix-cli,
detrix-testing, detrix-tui
```

Also include cross-cutting targets:
- `clients/python`, `clients/go`, `clients/rust`

## Phase 3: Dispatch Subagents

Launch subagents in batches of 3-4 (parallel). Each subagent:
- Gets the crate name and scope
- Reads code using its own fresh context window
- Writes findings to `.agents/audit/<crate-name>.md`

Use the Task tool with `subagent_type: "general-purpose"` for each crate. Give each subagent this prompt template:

```
You are auditing the crate `<CRATE>` in the Detrix project at /Users/ilyadyachenko/Documents/Yandex.Disk/_src/detrix/detrix-release

Read CLAUDE.md first to understand architecture rules.

Scope: <SCOPE>
[If branch/uncommitted, include the relevant git diff output for this crate]

Perform a comprehensive audit checking:
1. Architecture violations (see CLAUDE.md dependency rules)
2. .map_err() that should be From/TryFrom impls
3. unwrap/expect/panic in non-test code
4. Hardcoded strings/numbers that should be constants or config
5. God files, doom pyramids, over-complex code
6. Commented-out code, dead code, unused deps
7. TODO items
8. N+1 queries, data races
9. Arc<dyn Trait + Send + Sync> without type aliases
10. Duplicate DTOs (should use proto-generated or core)
11. Default values — sensible? Could break things?
12. Similar error variants that should be consolidated
13. Config constants must also exist in detrix.toml
14. Test quality — fake tests, missing coverage, edge cases
15. Code duplication / extractable patterns
16. Unwired code (defined but never called)

Write findings to .agents/audit/<CRATE>.md in this format:

# Audit: <CRATE>
**Audited at:** [timestamp]
**Scope:** [scope]

## Summary
- Critical: [count]
- Warning: [count]
- Info: [count]

## Critical
- **[TITLE]** `file:line` — description (3-5 line code snippet if relevant)

## Warning
- **[TITLE]** `file:line` — description

## Info
- **[TITLE]** `file:line` — description

## TODOs Found
- `file:line` — text

Every finding MUST have file:line reference. Write the file even if no issues found.
```

**Launch order** (respect dependency layers, audit bottom-up):
- Batch 1: `detrix-core`, `detrix-config`, `detrix-logging`
- Batch 2: `detrix-ports`, `detrix-storage`, `detrix-lsp`
- Batch 3: `detrix-application`, `detrix-dap`, `detrix-output`
- Batch 4: `detrix-api`, `detrix-cli`, `detrix-tui`, `detrix-testing`
- Batch 5: `clients/python`, `clients/go`, `clients/rust` (if in scope)

Wait for each batch to complete before starting the next — earlier batches may inform later findings.

## Phase 4: Cross-Crate Duplication Analysis

Code duplication CANNOT be detected by per-crate subagents (they only see one crate each). Use a dedicated fingerprint-compare-confirm approach.

Launch a subagent with the full prompt from `/audit-duplication`. It will:
1. Grep for duplication signals across the whole codebase (cheap — no file reads)
2. Extract lightweight API catalogs per crate cluster (structs, enums, errors, signatures)
3. Compare catalogs to find candidates, then do targeted reads to confirm
4. Write findings to `.agents/audit/_duplication.md`

This is the most context-intensive phase — it gets its own subagent with a fresh window.

## Phase 5: Cross-Cutting Consistency

After duplication analysis is done, launch ONE more subagent for non-duplication cross-cutting concerns:

```
Read all files in .agents/audit/*.md (per-crate reports + duplication report). Then check:
1. Wiring consistency — are all components properly connected across crates?
2. Config dataflow — do config params flow through all layers correctly?
3. Error type consistency across crate boundaries
4. Patterns used differently in different crates (pick best approach)
5. Any findings from per-crate reports that are actually the same root cause

Write findings to .agents/audit/_cross-cutting.md
```

## Phase 6: Compile Final Report

Read ONLY the output files from `.agents/audit/`. Do NOT re-read source code.

Compile `.agents/audit/AUDIT_REPORT.md`:

```markdown
# Detrix Audit Report
**Date:** [date]
**Scope:** [scope]
**Branch:** [branch name]

## Executive Summary
| Crate | Critical | Warning | Info |
|-------|----------|---------|------|
| detrix-core | X | Y | Z |
| ... | | | |
| **Total** | **X** | **Y** | **Z** |

## Critical Findings (all crates)
[Consolidated list of all critical findings]

## Per-Crate Details
[Include each crate's full report]

## Cross-Crate Duplication
[From _duplication.md — confirmed duplications, DTO consolidation map, client duplication]

## Cross-Cutting Concerns
[From _cross-cutting.md — wiring, config dataflow, error consistency]

## Recommendations
[Top 10 prioritized action items]
```

Print the executive summary table to the user and the path to the full report.
