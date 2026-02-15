---
description: "Audit a single crate (used by /audit coordinator or standalone)"
allowed-tools: "Bash(run tests:*), Bash(cargo check:*), Bash(cargo clippy:*), Bash(git diff:*), Bash(git log:*), Bash(git status:*), Bash(mkdir:*)"
---

You are a senior Rust developer performing a focused code audit of a single crate in the Detrix project.

## Target

Audit the crate specified in: "$ARGUMENTS"

Parse the arguments as: `<crate-name> [scope]`
- `<crate-name>` is required (e.g. `detrix-core`, `detrix-api`)
- `[scope]` is optional: `branch`, `uncommitted`, or `project` (default: `project`)

If scope is `branch`: only audit files changed on current branch vs main (use `git diff main...HEAD -- crates/<crate-name>/`).
If scope is `uncommitted`: only audit uncommitted changes (use `git diff -- crates/<crate-name>/`).
If scope is `project`: audit the entire crate.

## Instructions

Read CLAUDE.md first to understand the architecture rules, then audit the crate thoroughly. Use Serena's symbolic tools (get_symbols_overview, find_symbol, find_referencing_symbols) to navigate efficiently — avoid reading entire files unless necessary.

## Checklist

### Architecture & Design
- Clean Architecture layer violations (check dependency rules: core depends on NOTHING, ports on core+config only, application on ports+core+config only, infrastructure implements port traits)
- God files / god modules (files with too many responsibilities)
- Over-complex code / doom pyramids (deeply nested if/match/loop)
- Proper component wiring — compare patterns across the crate
- Config parameter dataflow — params properly passed through layers

### Error Handling
- `.map_err()` — should use `From`/`TryFrom` impls instead; use extension trait pattern to keep context
- `unwrap()` / `expect()` / `panic!()` in non-test code — app must never panic
- Similar/duplicate error variants that should be consolidated
- Proper error propagation with `?` operator

### Type System & Constants
- `Arc<dyn Trait + Send + Sync>` — verify proper type aliases are used
- Hardcoded strings or magic numbers — should be enums/constants
- Hardcoded values where config values should be forwarded
- Default values — are they sensible? Could they cause issues?
- Constants should be defaults only; actual values from detrix.toml

### DTOs & Data
- Duplicate DTOs — should use proto-generated or core DTOs
- N+1 SQL queries in loops (if applicable)
- Possible data races (shared mutable state, Arc<Mutex> patterns)

### Code Quality
- Commented-out code (remove it, that's what git is for)
- Dead / unused code
- Unwired code (defined but never connected/called)
- TODO items — list with file:line
- Unused dependencies in Cargo.toml
- Code duplication / patterns that could be extracted

### Testing
- Missing tests for critical paths
- Fake tests (print warning + return Ok, or no meaningful assertions)
- Edge cases not covered

## Output

Write your findings to `.agents/audit/<crate-name>.md` using this EXACT format:

```markdown
# Audit: <crate-name>
**Audited at:** [timestamp]
**Scope:** [branch/uncommitted/project]
**Files examined:** [count]

## Summary
- Critical: [count]
- Warning: [count]
- Info: [count]

## Critical
- **[SHORT_TITLE]** `file/path.rs:LINE` — description of the issue
  ```rust
  // offending code snippet (keep short)
  ```

## Warning
- **[SHORT_TITLE]** `file/path.rs:LINE` — description

## Info
- **[SHORT_TITLE]** `file/path.rs:LINE` — description

## TODOs Found
- `file/path.rs:LINE` — TODO text
```

IMPORTANT:
- Every finding MUST have a `file:line` reference
- Keep code snippets short (3-5 lines max)
- Write the file even if no issues found (report "No issues found")
- Do NOT print the full report to the user — just write to the file and confirm completion with a count summary
