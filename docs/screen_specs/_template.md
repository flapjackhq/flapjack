# [Screen Name]

## Task

[One sentence: what is the user doing here?]

## Layout

[Numbered list, top to bottom, what the user sees]

## State contract

### Loading
- [What the user sees while data loads]

### Error
- [What the user sees on failure, including retry mechanism]

### [Primary state]
- [What controls are visible and what they do]

### [Additional states as needed]
- [What controls are visible and what they do]

## Navigation

- Route: [/path or route name]
- Entry: [how users get here, use backticks for screen names]
- Back: [what happens]
- [Action]: [navigates to `Screen Name`]

## Acceptance criteria

- [Given/When/Then statements at user-goal level]

## Edge cases

- [Empty state, permission state, offline, first-run]

## Current implementation gaps

[Optional. Include only when shipped behavior materially differs from this target spec.]

- Current: [What the shipped UI does today]
- Target: [What this spec requires instead]
- Evidence: [Verified code/doc/test reference]
