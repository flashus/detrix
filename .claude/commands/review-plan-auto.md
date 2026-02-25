---
description: "Automated plan review-revise cycle (up to 4 rounds)"
---

You are orchestrating an automated plan review-feedback-revision cycle. You act as a **project manager** coordinating two personas via subagents:
- **Reviewer**: senior architect + staff developer who finds flaws
- **Reviser**: the plan author who incorporates feedback

## Input

The argument provided is: "$ARGUMENTS"

If the argument is empty or blank, use AskUserQuestion:
```
Question: "What plan should I review?"
Options:
- "Paste text" — "I'll paste the plan text directly in the chat"
- "Read a file" — "I'll provide a file path to read"
```
Wait for the user's response before proceeding.

If the argument IS provided:
- If it looks like a file path (contains `/` or `.md`), read the file
- Otherwise, treat it as the plan text itself

## Setup

Extract a short plan name from the plan's title/first heading (e.g., "auth_fix", "cloud_deploy"). Sanitize to lowercase, underscores, no spaces.

```bash
mkdir -p .claude/reviews/<plan_name>
```

Save the original plan as `.claude/reviews/<plan_name>/rev_0.md`.

Tell the user: "Starting automated review cycle for plan: **<plan_name>**. Up to 4 review rounds, stopping early if approved."

## Phase 0: Code Verification (before first review)

Launch a subagent to verify the plan's assumptions against the actual codebase:

```
Prompt: "You are verifying assumptions in a plan against the actual codebase.

Read the plan below, then check every file path, line number, struct name, function name,
and behavioral assumption against the real code. Report:
1. Confirmed assumptions (file exists, line matches, behavior is as described)
2. Wrong assumptions (file moved, line numbers off, struct has different fields, etc.)
3. Missing context (things the plan doesn't mention that are relevant)

Be specific — include actual file:line references from the codebase.

Plan:
<plan text>

Write findings to: .claude/reviews/<plan_name>/verification.md"
```

Read the verification output. If there are wrong assumptions, they'll be included in the first review.

## Review-Revise Loop

Loop for `round` in 0..4:

### Step A: Review

Launch a **reviewer** subagent:

```
Prompt: "You are a senior project architect and staff developer reviewing a plan.

Round: <round>/4
Plan file: .claude/reviews/<plan_name>/rev_<round>.md
[If round == 0]: Code verification findings: .claude/reviews/<plan_name>/verification.md
[If round > 0]: Previous feedback: .claude/reviews/<plan_name>/feedback_<round-1>.md

Read the plan file. [If round 0, also read verification.md and incorporate any wrong assumptions as findings.]
[If round > 0, also read the previous feedback to check if issues were properly addressed.]

Evaluate:
- Architectural soundness and alignment with existing codebase patterns
- Separation of concerns and dependency direction
- Error handling strategy
- Edge cases and failure modes
- Performance implications
- Security considerations
- Testability of the proposed design
- Whether the plan follows SOLID principles
- Missing steps or overlooked dependencies
- [If round > 0]: Whether previous feedback was properly addressed

Output format — write to .claude/reviews/<plan_name>/feedback_<round>.md:

# Review Round <round>

## Verdict: APPROVED | APPROVED_WITH_NOTES | REVISE
[APPROVED = no issues, stop the cycle]
[APPROVED_WITH_NOTES = only info-level items remain, stop but note them]
[REVISE = critical or warning items require another revision]

## Previous Feedback Status (round > 0 only)
| Previous Issue | Status |
|---|---|
| ... | Resolved / Partially resolved / Not addressed |

## Findings

### Critical (must fix)
- ...

### Warning (should fix)
- ...

### Info (optional)
- ...

## Future Improvements
[Anything out of scope but worth noting for later — architectural debt,
follow-up tasks, optimization opportunities, related features to consider.
Accumulate from previous rounds — don't drop items.]

IMPORTANT: Be strict in early rounds, more lenient in later rounds.
Round 0-1: flag everything. Round 2-3: only flag genuine remaining issues.
If you keep finding the same issues, mark as APPROVED_WITH_NOTES and list them
as future improvements instead of blocking."
```

### Step B: Check Verdict

Read `feedback_<round>.md`. Extract the verdict line.

- If **APPROVED** or **APPROVED_WITH_NOTES**: stop the loop, proceed to final output.
- If **REVISE**: continue to Step C.
- If this is round 3 (last possible review): force stop after this round regardless.

### Step C: Revise

Launch a **reviser** subagent:

```
Prompt: "You are the author of a technical plan, incorporating reviewer feedback.

Plan: .claude/reviews/<plan_name>/rev_<round>.md
Feedback: .claude/reviews/<plan_name>/feedback_<round>.md
[If round == 0]: Verification: .claude/reviews/<plan_name>/verification.md

Read the plan and the feedback. Produce a revised plan that:
1. Addresses ALL critical and warning items from the feedback
2. Fixes any wrong assumptions found in verification (round 0)
3. Preserves the plan's structure and good parts
4. Adds explicit notes for each addressed item (so the next review can verify)
5. Includes a 'Future Improvements' section at the end, carrying forward all items
   from the reviewer's future improvements list

Do NOT:
- Over-engineer or add scope creep
- Remove sections that weren't flagged
- Change the plan's fundamental approach unless the feedback specifically calls for it

Write the revised plan to: .claude/reviews/<plan_name>/rev_<round+1>.md

At the end of the file, add a change log:
---
Changes in rev_<round+1> (addressing feedback_<round>):
- [item]: [what changed]
"
```

### Step D: Report Progress

Tell the user:
```
Round <round> complete:
- Findings: X critical, Y warning, Z info
- Verdict: REVISE → generating rev_<round+1>
```

Repeat the loop.

## Final Output

After the loop ends (approved or max rounds reached):

1. Read the final `rev_N.md` and the last `feedback_N.md`

2. Create `.claude/reviews/<plan_name>/SUMMARY.md`:

```markdown
# Plan Review Summary: <plan_name>

**Rounds:** <N+1> (rev_0 through rev_<N>)
**Final verdict:** <verdict>
**Final plan:** rev_<N>.md

## Review History
| Round | Critical | Warning | Info | Verdict |
|-------|----------|---------|------|---------|
| 0     | X        | Y       | Z    | REVISE  |
| 1     | X        | Y       | Z    | REVISE  |
| ...   |          |         |      |         |

## Key Changes Across Revisions
- rev_0 → rev_1: [summary of changes]
- rev_1 → rev_2: [summary of changes]
- ...

## Remaining Items (if APPROVED_WITH_NOTES)
- ...

## Future Improvements
[Consolidated from all rounds — deduplicated]

## Files
- Original plan: rev_0.md
- Final plan: rev_<N>.md
- All reviews: feedback_0.md through feedback_<N>.md
- Code verification: verification.md
```

3. Print to the user:
   - The summary table
   - Path to the final plan
   - Path to the summary
   - The "Future Improvements" section inline (so they don't have to open a file)
