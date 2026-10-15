# Memz trigger signals

Signals that should make an agent *consider* memory. The policy in
[SKILL.md](SKILL.md) still decides. This catalogue is written so a future hook
can match the same signals and inject a short nudge ("consider Memz recall /
remember — apply the gate"); the hook nudges, the model judges.

Strength:
- **strong** — act unless the gate clearly fails.
- **weak** — evaluate with the four-part gate; most weak signals end in no write.

## Recall signals

| id | where it shows up | examples | strength |
|---|---|---|---|
| R1 past-reference | user prompt | "like last time", "we decided", "as I said before", "remember when" | strong |
| R2 task-start | first substantive prompt about an area | "let's work on recovery", "fix the release gate" | weak |
| R3 preference-sensitive choice | agent about to choose style/tooling/scope | naming, commit shape, test strategy, dependency choice | weak |
| R4 recurring failure | tool output | same error text seen before, flaky test, "database is locked" | weak |
| R5 pre-write dedup | agent about to `remember` | any write | strong |

## Remember signals

| id | where it shows up | examples | strength |
|---|---|---|---|
| M1 explicit request | user prompt | "remember that…", "note for the future", "keep in mind" | strong |
| M2 standing preference | user prompt | "always…", "never…", "from now on…", "I prefer…", "don't ever…" | strong |
| M3 decision with reason | conversation | "we'll go with X because Y", "let's not do Z since…" | weak |
| M4 verified fix | tool output after a change | failing test turns green after a non-obvious fix | weak |
| M5 costly discovery | conversation/tool output | environment quirk found after several failed attempts | weak |

## Correct / forget signals

| id | where it shows up | examples | strength |
|---|---|---|---|
| C1 user correction | user prompt | "that's wrong", "actually…", "not anymore", "we changed that" | strong |
| C2 explicit forget | user prompt | "forget that", "don't remember this" | strong |
| C3 stale memory | agent comparing recall to evidence | recalled memory contradicts current code/tests/docs | strong |

## Anti-signals (suppress writes)

- The content contains a secret, token, password, key or personal data.
- The content is a plan, todo list, progress update or command log.
- The claim is hedged ("probably", "I think", "might be") or unverified.
- The fact is already in code, docs, an ADR, the plan journal or git history.

## Notes for a future hook

- Natural attachment points: prompt submission (R1, R2, M1, M2, C1, C2), tool
  results (R4, M4) and end of turn (batched M3/M5 review).
- A hook should inject a reminder, not call Memz itself: the gate needs the
  model's judgment and conversation context.
- Keep nudges rare and short; a nudge on every turn trains the agent to ignore it.
