---
description: "Review a plan/design doc as architect + staff developer"
---

## Input

The argument provided is: "$ARGUMENTS"

If the argument is empty or blank, you MUST use AskUserQuestion to ask the user BEFORE doing anything else:

```
Question: "What would you like me to review?"
Options:
- "Paste text" — "I'll paste the plan text directly in the chat"
- "Read a file" — "I'll provide a file path to read"
- "Read from clipboard" — "Use the most recent clipboard content"
```

If the user picks "Paste text", say: "Please paste your plan text and I'll review it."
If the user picks "Read a file", say: "Please provide the file path."
Then wait for their response before proceeding.

If the argument IS provided, proceed directly with the review below.

---

## Review Instructions

Please review the following plan as a **project architect** and as a **staff developer**.

I need you to find any possible architectural flaws, inconsistencies, poor design decisions, missing considerations, and potential risks.

Specifically evaluate:
- Architectural soundness and alignment with existing patterns
- Separation of concerns and dependency direction
- Error handling strategy
- Edge cases and failure modes
- Performance implications
- Security considerations
- Testability of the proposed design
- Whether the plan follows SOLID principles
- Any missing steps or overlooked dependencies

## Plan to review:

$ARGUMENTS
