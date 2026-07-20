---
name: rules-logging
description: Stack-neutral logging rules. Treat logs as a contract structured records (not free text), fixed schema (timestamp, severity, event, trace context, resource/service, attributes), stable event field, message grammar (ACTION/PROGRESS/STATUS), severity levels, log-trace correlation. Based on the OpenTelemetry Logs Data Model and the "logs as a language" practice. Use when adding logging to a service, standardizing logs across services, setting up observability, or designing a log contract.
---

# Logging

## Treat logs as a contract, not an afterthought

Logging appears "on the way" — write a service, scatter a few `info()` calls, move on. Across many services this produces a language with no grammar or dictionary: the same event is described dozens of ways, dashboards parse text with regex, and incident response slows to a crawl.

The fix is not a new tool. The fix is to treat logs as a **language with a contract**: a fixed record schema, a stable event vocabulary, a message grammar, and rules for severity and context. Standardizing the text is harder than standardizing the code — code can be linted, the language has to be specified.

## Structured logs, not free text

A log record is a set of named fields, never an interpolated string.

Bad — the message is unparseable, the same event worded differently each time:

```
info(f"{uuid} - New task created, total tasks: {counter}")
info(f"{uuid} - New task created")
info(f"{uuid} - Task created with params: {params}")
```

Good — stable event, structured fields, context bound once:

```
logger = log.bind(request_id=uuid)
logger.info("Created task", total=counter)
logger.info("Created task", params=params)
```

Why structured: monitoring backends auto-parse named fields into columns; search becomes filtering by field values instead of grepping text; the same event always reads the same way.

## The log record schema

Model each record after the OpenTelemetry Logs Data Model — two timestamp kinds, severity, a stable body, trace context, resource, and attributes.

| Field | Meaning | Notes |
|---|---|---|
| `Timestamp` | When the event **occurred** (origin clock). | Prefer this for event time. Optional if unknown. |
| `ObservedTimestamp` | When the collection system **observed** it. | For first-party logs often equals `Timestamp`. If a backend supports only one timestamp, use `Timestamp` if present, else `ObservedTimestamp`. |
| `SeverityText` | Log level as a string (`INFO`, `ERROR`...). | Original representation from the source. |
| `SeverityNumber` | Numeric severity (see table below). | Enables unambiguous mapping across formats. |
| `Body` / `event` | Human-readable description of the event. | **Must be stable** for a given event class — see below. |
| `TraceId` / `SpanId` / `TraceFlags` | W3C trace context. | If `SpanId` is present, `TraceId` SHOULD be too. |
| `Resource` | The entity that emitted the log — `service.name`, `host.name`, etc. | Identifies who logged. |
| `InstrumentationScope` | The scope (module/library) that emitted the log. | |
| `Attributes` | Arbitrary additional key-value data. | Use for params, durations, paths — anything event-specific. |
| `EventName` | Identifies the class/type of event. | Optional but powerful for analytics. |

Rules for the schema:

- **Fixed set of fields, fixed order.** Eyes find fields fast when they're always in the same place.
- **Mandatory correlation id.** Whether it's `TraceId`, `request_id`, or a domain `uuid` — every record carries one. Don't rely on humans remembering it.
- **Reserve an extension field** (e.g. `params`/`Attributes`) for event-specific data, so the core schema stays small.
- Top-level named fields are for what's mandatory or near-always-present with identical semantics across services. Everything else lives in `Attributes`.

## The event field is a contract

`event`/`Body` is the one field humans read, and the one analytics groups by. If the same event is worded differently across services or over time, every dashboard and alert becomes fragile.

- One event class → one stable event string. Treat it like an enum, not a sentence.
- Keep a shared **event dictionary** (names + examples) so new services reuse wording instead of inventing it.
- Put variable data in fields, not in the event text. `Loaded audio file` + `path=...`, not `"Loaded audio file from {path}"`.

## Message grammar

Constrain how event text is formed. Three grammatical modes, in order of preference:

| Mode | Form | When |
|---|---|---|
| **ACTION** (preferred) | Verb (Past Simple) → Object → [details] | A completed event. `"Created task"`, `"Loaded audio file"`. |
| **PROGRESS** | Verb (Present Participle) → Object → [details] | An in-progress state. `"Loading audio file"`. Use sparingly — prefer logging the completed action after it finishes. |
| **STATUS** (least preferred) | State / Fact | A fact, final state, or high-level error condition. |

Errors:

- An error as an **event** uses ACTION and starts with **`failed to`**: `"Failed to load audio file"`.
- An error as a **state** may use STATUS, but only for high-level error conditions.
- State the cause with **`due to`** when known: `"Failed to load audio file due to codec error"`.
- Put machine-readable cause in a field (`error.type`, `error.message`), not only in the text.

Decision shortcut: if a message is logged *before* a short action completes, move it to *after* completion and write it as ACTION.

## Severity levels

Map levels to the OpenTelemetry severity ranges. Smaller number = less severe.

| SeverityNumber | Name | Meaning |
|---|---|---|
| 1–4 | TRACE | Fine-grained debugging; off by default. |
| 5–8 | DEBUG | Debugging event. |
| 9–12 | INFO | Informational — something happened. |
| 13–16 | WARN | Warning — not an error, but more important than info. |
| 17–20 | ERROR | Error — something went wrong. |
| 21–24 | FATAL | Fatal — app/system crash. |

- `ERROR` (numeric ≥ 17) marks an erroneous situation. Don't log normal flow at ERROR; don't swallow errors at INFO.
- When mapping a source format with several levels in one range, assign numbers by relative importance (e.g. `Error`→17, `Critical`→18).
- When a format has a single level matching a range, use the range's smallest number (e.g. `Informational`→9).
- Formats with no severity concept MAY omit severity; backends then typically treat missing severity as INFO.

## Context via binding

Bind scope/context once, then log against the bound logger so every line carries the correlation id without repeating it:

```
logger = root.bind(request_id=req_id, service="billing")
logger.info("Created task", total=counter)
```

- Always bind the **trace/request correlation id** at request entry.
- Carry `TraceId`/`SpanId` so logs join traces end-to-end.
- Bind `service.name`, `host.name`, etc. as resource attributes once at process start.

## Make logs debuggable

The schema tells you *how* to write a record. This section is about *what* to put in it so that weeks later you can reproduce a bug from the logs alone — not stare at `failed to process` with no idea what was processed or why.

- **Log intent + input, not only the result.** `Failed to process order` with `order_id`, `input=...` — not bare `Failed to process`. A month later, one line can't be reproduced.
- **Operation boundaries, not only the outcome.** For non-trivial work, log start/end + duration so the timeline is visible instead of a black hole before the error. Long operations: log progress on a step boundary.
- **Exceptions whole — type, message, stack.** Carry `error.type`, `error.message`, and the stack trace in a field. Never `str(e)`, and never swallow an exception at INFO.
- **Decision points.** At branches that change behavior (chosen path, fallback, retry decision) log *why* that branch was taken. Otherwise "why did it fall back?" is unanswerable forever.
- **Where, not only what.** Component + operation in `Resource`/`InstrumentationScope`/`Attributes`, so you know the location, not just the event.
- **Correlation across boundaries.** `TraceId`/`SpanId` + `request_id`/`causation_id`, so "error here" links back to "root cause in another service". The schema fields exist for exactly this — use them for debugging, not just for dashboards.
- **What's already done (idempotency).** In distributed flows, log "step N done", so an investigation can see which step died and whether a partial effect landed.
- **Detail by context, not by habit.** DEBUG: inputs and intermediate steps for local triage. INFO/WARN: outcomes and anomalies for production. Don't push everything into INFO; don't hide triage detail at INFO.

## Rendering

- **Machine format** (JSON or equivalent) for production/ingest — what monitoring reads.
- **Dev renderer** (colorized table/console) for local work — what engineers read.
- Same fields, two renderers. Don't change the schema to make one prettier.

## Anti-patterns

| Anti-pattern | Fix |
|---|---|
| Free text / string interpolation in the message | Structured named fields |
| Same event, different wording | Stable `event` + shared dictionary |
| Grepping/regex-parsing logs in dashboards | Filter by fields |
| Everything at INFO (or ERROR) | Severity by meaning |
| No correlation id | Bind `request_id`/`TraceId` at entry |
| Variable data baked into the event text | Move to `Attributes` |
| Secrets / PII in logs | Never log credentials, tokens, personal data |
| Duplicate info repeated across lines | One event, one record |
| One timestamp used for both event and ingest | Distinguish `Timestamp` vs `ObservedTimestamp` |

## Checklist before calling logs "production-ready"

- [ ] Records are structured (named fields), not free text.
- [ ] Fixed schema: known fields, fixed order, mandatory correlation id.
- [ ] `event`/`Body` is stable per event class and backed by a dictionary.
- [ ] Messages follow the grammar (ACTION preferred; errors start with `failed to`, cause with `due to`).
- [ ] Severity reflects meaning (ERROR only for errors; missing severity treated as INFO).
- [ ] Every record carries trace/request correlation id; logs join traces.
- [ ] Logs reproduce the path to an error: intent + input, operation boundaries, full exception (type+message+stack), decision points, trace correlation.
- [ ] `Timestamp` vs `ObservedTimestamp` distinguished where relevant.
- [ ] Machine format (JSON) for ingest + a dev renderer for humans.
- [ ] No secrets or PII.
- [ ] Backend filters by fields, not by regex over text.
