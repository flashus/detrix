---
description: "Detect code duplication across crates and clients"
allowed-tools: "Bash(git diff:*), Bash(git log:*), Bash(git status:*), Bash(mkdir:*), Bash(wc:*), Bash(sort:*)"
---

You are a senior developer analyzing the Detrix codebase for cross-crate and cross-language code duplication. Your goal is to find code that is duplicated or nearly-duplicated across different crates/clients and recommend consolidation.

## Scope

"$ARGUMENTS" — if empty, analyze the entire project. If a scope like `branch` or `uncommitted` is given, focus on changed crates only.

## Strategy: Fingerprint → Compare → Confirm

You CANNOT read all crates at once (context limit). Instead, use a 3-phase approach:

### Phase 1: Automated Pattern Search

Use Grep to search across ALL crates simultaneously for duplication signals. Run these searches in parallel — each one is cheap and covers the whole codebase:

**1a. Identical/similar function signatures:**
```
Search for common function name prefixes that appear in multiple crates:
- "fn new(" across crates (constructor patterns)
- "fn from_" / "fn to_" / "fn into_" (conversion functions)
- "fn validate" / "fn parse" / "fn format" (utility patterns)
- "fn get_" / "fn find_" / "fn list_" (data access patterns)
```

**1b. Similar struct/enum definitions:**
```
Search for struct/enum names that appear in multiple crates.
Look for structs with similar field sets (e.g., same fields in different DTOs).
```

**1c. Error type duplication:**
```
Search for error enum variants across crates.
Look for similar error messages/patterns.
```

**1d. Conversion boilerplate:**
```
Search for From/TryFrom/Into implementations.
Look for similar .map() / .map_err() / conversion chains.
```

**1e. Similar imports:**
```
Search for identical multi-line use statements that suggest shared patterns.
```

**1f. Copy-pasted blocks:**
```
Search for distinctive string literals, magic numbers, or unique code patterns
that appear in multiple files across different crates.
```

Write raw search results to `.agents/audit/_duplication_raw.md`.

### Phase 2: Catalog Extraction (via subagents)

Launch subagents in parallel — one per "cluster" of related crates. Each subagent:
- Reads the crate using get_symbols_overview (NOT full file reads)
- Extracts a structured catalog of public API surface
- Writes to `.agents/audit/_catalog_<cluster>.md`

**Clusters** (group crates that are likely to share patterns):

1. **Domain cluster**: `detrix-core`, `detrix-config`, `detrix-ports`
   - Catalog: all public structs, enums, error types, trait definitions with field names/variant names
   - Focus: DTO shapes, error variants, trait signatures

2. **Infrastructure cluster**: `detrix-storage`, `detrix-dap`, `detrix-lsp`, `detrix-output`
   - Catalog: trait implementations, conversion functions, error handling patterns
   - Focus: repeated impl patterns, similar adapter logic

3. **Application cluster**: `detrix-application`, `detrix-api`, `detrix-cli`
   - Catalog: service methods, controller handlers, DTO mappings
   - Focus: similar service patterns, repeated validation, DTO conversions

4. **Client cluster**: `clients/python`, `clients/go`, `clients/rust`
   - Catalog: public API functions, data models, HTTP/gRPC call patterns
   - Focus: logic that should be identical across languages

Each catalog file format:
```markdown
# Catalog: <cluster>

## Structs (name → fields)
- `CrateName::StructName` { field1: Type, field2: Type }

## Enums (name → variants)
- `CrateName::EnumName` { Variant1(Type), Variant2 { field: Type } }

## Error Types (name → variants with messages)
- `CrateName::Error::VariantName` — "error message pattern"

## Trait Impls (what implements what)
- `CrateName::Type` impl `TraitName`

## Conversion Functions (From/TryFrom/Into)
- `From<A> for B` in crate_name

## Public Functions (signature only)
- `crate::module::fn_name(params) -> ReturnType`
```

### Phase 3: Compare & Confirm

Launch ONE subagent that:
1. Reads ALL `_catalog_*.md` files and `_duplication_raw.md`
2. Identifies duplication candidates by comparing:
   - Structs with >70% field overlap
   - Error variants with similar names or messages
   - Functions with similar signatures in different crates
   - Conversion impls that follow the same pattern
   - Client code that implements the same logic differently
3. For each candidate, does TARGETED reads (specific symbols only) to confirm it's real duplication
4. Writes confirmed findings to `.agents/audit/_duplication.md`

### Output Format

Write `.agents/audit/_duplication.md`:

```markdown
# Cross-Crate Duplication Report
**Date:** [date]

## Summary
- Confirmed duplications: [count]
- Estimated lines that could be removed: [count]

## Confirmed Duplications

### 1. [TITLE] — [severity: high/medium/low]
**Locations:**
- `crate-a/src/file.rs:LINE` — `StructOrFnName`
- `crate-b/src/file.rs:LINE` — `StructOrFnName`

**What's duplicated:** [description]

**Consolidation recommendation:**
- [ ] Extract to `detrix-core` / `detrix-ports` / new shared module
- Pros: [...]
- Cons: [...]
- Estimated effort: [small/medium/large]

```rust
// Example of what the consolidated version would look like (optional)
```

### 2. ...

## DTO Consolidation Map
| DTO | Used In | Should Be | Proto? |
|-----|---------|-----------|--------|
| SomeDto | api, application | core or proto | Yes/No |

## Client Duplication
| Logic | Python | Go | Rust | Consolidation |
|-------|--------|-----|------|---------------|
| [feature] | file:line | file:line | file:line | [recommendation] |

## Recommendations (prioritized)
1. [highest impact consolidation]
2. ...
```

Print a summary to the user and the path to the full report.
