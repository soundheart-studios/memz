---
name: memz
description: Judgment policy for Memz project memory. Use when starting non-trivial work in an area, when the user refers to past context, states a preference or decision, corrects a fact, or when a fix is verified — to decide whether to recall, remember, correct, forget, or do nothing.
version: 1
---

# Memz memory policy

Memz is shared, durable project memory across sessions and harnesses (Claude
and Codex read the same store). Its value depends on what you put in it. Most
turns should touch memory **not at all**. When the Memz tools (`recall`,
`remember`, `inspect`, `correct`, `forget`, `status`) are absent, say so once
and continue without memory; do not imitate it with files.

Trigger signals and their strength are catalogued in [triggers.md](triggers.md).
A signal is a reason to *consider* memory, never a reason to skip judgment.

## 1. Recall — before you act, not after

Recall when one of these holds:

- You are starting non-trivial work in an area (a subsystem, workflow, release).
- The user refers to the past: "like last time", "we decided", "as I said".
- You are about to make a choice the user may already have a preference on
  (style, tooling, naming, scope, risk tolerance).
- An error or failure looks familiar or recurring.
- You are about to `remember` something (check for an existing memory first).

How to query:

- Use 2–5 distinctive keywords, not a full question: `release validation
  manifest`, not "how do we run the release checks?". Lexical recall matches
  terms; semantic recall works only when embeddings are configured and built.
- One or two queries per topic. Use `budget` around 1500; raise it only when
  results are clearly truncated.
- Treat results as **leads**. Hybrid recall returns much irrelevant context;
  use only items that actually bear on the task.

Precedence: **the user's latest instruction > current code, tests and docs >
memory.** A memory that contradicts current evidence is stale. Follow the
evidence, then `correct` or `forget` the memory and tell the user in one line.
Never present a memory as current fact without checking it when checking is
cheap.

## 2. Remember — the four-part gate

Remember only when **all four** hold:

1. **Durable** — still true next week; not a step in today's task.
2. **Not derivable** — not recoverable with one search of the code, git log,
   docs, ADRs or plan. Memory holds the *why* and the unwritten, not the *what*.
3. **Actionable** — a future agent would act differently knowing it.
4. **Grounded** — the user stated it, or you verified it with evidence this
   session (a passing test, an observed failure, a confirmed measurement).

Regret test when unsure: *would a future session make a mistake or re-ask the
user without this?* If not, skip it.

Typically worth remembering:

- Confirmed user preferences ("prefers small task-prefixed commits").
- Decisions **with their reasons** and rejected alternatives.
- Constraints and invariants that are easy to violate and not enforced by tests.
- Verified fixes for non-obvious, likely-recurring problems: symptom → cause → fix.
- Environment facts that cost time to rediscover (where model files live, which
  command needs which env var).

Never remember:

- Secrets, tokens, credentials, private personal data. `forget` stops retrieval
  but cannot erase exports, backups or disk bytes — so never write them at all.
- Progress chatter, plans for this session, todo lists, command transcripts.
- Speculation, hypotheses, unverified root causes, your own guesses.
- Anything already in the repo, an ADR, the plan journal, or a commit message.
- Restatements of a memory that already exists — `correct` it instead.

Example — remember:
`Keep runtime state under ~/.memz, never inside the repository, because git
worktrees must not share or corrupt repository-local state.`
Do not remember the commands used to implement it.

## 3. Write well

- **One fact per memory**, self-contained: name the subsystem, file or tool so
  it reads correctly with no conversation context. Two or three sentences max.
- **Attribute it in the text.** Start with `User:` for things the user stated
  or confirmed, `Verified:` for things you established with evidence. (The tool
  currently records every MCP write as user-origin; the prefix is the honest
  signal until provenance is fixed.)
- `kind`: one of `preference`, `decision`, `procedure`, `incident`, `fact`,
  `task`, `entity`. Use `incident` for symptom→cause→fix, `procedure` for
  how-tos.
- `slot`: set it for anything with exactly one current value (e.g.
  `pref:commit-style`, `env:model-dir`). A new write to the same slot replaces
  the old value instead of creating a contradiction.
- `request_id`: stable and descriptive, `<kind>:<topic-slug>` (e.g.
  `decision:runtime-state-location`), so retries are idempotent.
- `visibility`: default `project`. Use `session` only for something the user
  wants kept out of other sessions.

## 4. Correct and forget

- Facts changed → `recall` the topic, `inspect` the item for its revision,
  then `correct` with `expected_revision`. Do not add a second, newer copy.
- The user says something is wrong or no longer applies → `correct` or `forget`.
- On a revision conflict, re-`inspect` and retry once; never blindly overwrite.
- Use `inspect` with `"impact": true` before correcting something other memories
  may depend on.

## 5. Timing and visibility to the user

- Explicit request ("remember that…", "forget that…") → act immediately.
- Everything else → batch at a natural checkpoint (task done, fix verified,
  decision settled). Usually zero to three writes per session; many writes in
  one session means the gate is too loose.
- After any write, correction or forget, tell the user in one short line what
  you stored or changed, so they can veto it.

## 6. Where else knowledge belongs

- Governed project decisions → ADRs / plan journal (Planwright), not Memz. Do
  not copy ADRs into memory; at most remember a pointer if it is easy to miss.
- Code-level facts → code comments, docs or tests.
- Project working knowledge shared across harnesses → Memz.
- Do not write the same fact to Memz and to a host's own memory system.
