# Factory task body templates

A task body — the issue body of the «Задачи <Proj>» kanban (uwuwu-hub).
The factory does not parse the body: the agent reads it itself (`uwuwu_task_get`), the spawn prompt carries
only the task's data. Below — the canon of sections, templates, and format rules: there is no body
generator and no automatic verification gate — this document is the reference of section content and order,
the discipline rests on the agent and review.
The analyst (#1135) reworks a request body per the skeleton of the matching type —
this document is its elaboration standard (frame proportionality: a light task — a light skeleton).
Body sections themselves stay Russian: task bodies are user-facing content the owner reads and writes
(the same carrier-noun class as the board columns).

## Format rules (mandatory)

- **Markdown must survive the round-trip of the issue body**: headings `##`, lists, bold.
  No HTML and no wiki syntax `[[ ]]` — the issue body is raw GitHub markdown.
  Diagrams — mermaid in the issue body (GitHub renders it natively) or a picture asset;
  the `visual-diagrams` skill. Images/files — as attachments via `blob_put` (#1292): a key +
  a presigned link in the text (TTL 7 days; an expired one — re-sign via `blob_get`, replace the URL in
  the carrier by hand).
- **No paired tildes outside code spans** in markdown fields of bodies and comments: on the hub a pair of tildes is
  GitHub strikethrough syntax — an accidental pair in a paragraph (`~30K`, `~250K`) silently changes
  how the text looks. Approximations — `≈` or «около», examples
  with a tilde — only in code spans.
- **No H1 in the body content**: the doc name already renders as a heading, `# {title}` is imported by the core
  as a real heading block — the export gets a duplicate H1.
- **Comments are not a body section**: `uwuwu_task_comment` writes real issue comments,
  they are not written into the body; a full body replacement does not touch them — that is the decision history
  (review verdicts, owner replies).
- **Agent comments are posted by the server under its GH token** — the same account as the owner's user
  comments in the UI: indistinguishable by author.

## Section canon

- **feature** (and every type of the feature pipeline: docs, other, refactoring, legacy, qa,
  infrastructure): `## Итог` (the first section; appears at final stages — written
  by the developer in the final report, updated on their rounds; not part of the create skeleton,
  the template below) → `## Постановка` (context → what to do) → `## План` (at the finish the agent
  grows it into `## Решение` — how it is built and why) → `## Юзкейсы` (opt.) →
  `## Сиквенсы` (opt.) → `## Проверка` (required) → `## Отчёт инженера` (the final phase
  of the task's life) → `## Смежное` (opt.). Optional sections are not part of the create skeleton —
  we do not breed empty stubs.
- **Отчёт инженера** — the legal final H2 section of a feature body; the inside — H3 subsections per
  §«Шаблон отчёта» of `agents/developer.md` (the only source of the report's inside — this
  document does not duplicate it).
- **spike**: `## Итог` (the template below; does not duplicate roles with a «Вывод»: Итог —
  a header digest, Вывод — the details with evidence) → Контекст → Варианты → Вывод →
  Границы. The agent's spike report — into the task
  body (the skeleton above), the finish — the Needs attention status; follow-up tasks for found
  problems are opened by the board owner.
- **bug**: `## Итог` (the template below) → Сценарий (BDD) → Красный тест → Причина → Фикс →
  Профилактика. TDD discipline:
  the red test first; it did not reproduce — stop and ask the owner.
- **docs** (doc work: syncs, hygiene, documentation) and **other** (everything else) — the full
  pipeline as a feature, the feature skeleton (Постановка → План/Решение → Проверка → Отчёт
  инженера). No separate canon sections — new types must share the
  feature/spike skeletons. The dev type is retired (20261110180332).
  A docs-only finish (edits only in uwuwu docs) — is legal: no PR is opened,
  the PR cell = the URL of the main wiki artifact (the finish canon — AGENTS.md, the «docs-only финал» bullet).

## Anti-patterns (the body is a clean draft, not a log)

- Conversational inserts («я посмотрел…», «ок, поправлю») — impersonal form.
- Progress logs: turn progress — kanban statuses and comments, not the body.
- Duplicate sections; appending paragraphs at the bottom without re-reading the whole.
- Live counters («строк 45») — the PR #66 convention: a number only with a reproduction
  command, otherwise qualitatively.

## Template: the «Итог» section (the final header, all types)

Universal for feature/spike/bug and their families: the body's first section at final
stages. Written by the developer in the final report, updated on each of their rounds
(review fixes, test fails). A digest for whoever opened the task: it does not duplicate details —
it points to them.

```markdown
## Итог

**Что сделано:** <1–2 предложения — суть>
**Где живёт:** <PR, вики-артефакты, конфиги/миграции>
**Как проверено:** <итог self-check + вердикты ревью/теста кратко>
**Компромиссы/флаги:** <или «нет»>
**Детали:** <указатели: секции тела, ключевые треды>
```

On a clean pass path the verification line stays «в тестировании» — the test verdict lives
in a comment next to it; the body is not a log, «Итог» is updated only by the developer on their rounds.

## Template: a feature task

```markdown
## Постановка

<контекст → что сделать; ссылки на доки и ADR>

## План

<скелет Design-стадии feature: контекст/источники → срезы с зависимостями → контракты
(типы/сигнатуры/API) → критерии приёмки (гиркин-сценарии) → стратегия верификации,
независимая от исполнителя; в финале секция перерастает в «Решение»>

## Проверка

<исполняемое: команды, тесты, живой прогон>
```

## Template: a spike task (research)

For tasks whose result is a conclusion and a recommendation, not a feature.
The Design stage: the spike plan lives in a `## План` section — right after `## Контекст`
(hypothesis → what counts as the answer → how we verify → research boundaries); at the finish
the plan goes away, the body moves to the report skeleton below.

```markdown
## Контекст

<что проверяем и зачем>

## Варианты

<что рассматривали: суть → плюсы/минусы/замеры; сравнение — таблица>

## Вывод

<рекомендация + доказательства: «путь A, потому что …»>

## Границы

<что НЕ исследовано и почему; где вывод не действует>
```

## Template: a bug task (TDD)

The Design stage: the bug plan lives in a `## План` section — right after `## Сценарий (BDD)`
(the reproduction scenario → the red-test plan → the expected mechanism); the red test
is mandatory before code. The finish — the skeleton below.

```markdown
## Сценарий (BDD)

Сценарий: <название>
  Дано <состояние мира>
  Когда <действие>
  Тогда <ожидание>  # факт: <что было на самом деле>

## Красный тест

<имя теста, который воспроизводит; не воспроизвёлся на стенде → детали окружения>

## Причина

<механизм: почему система так себя вела>

## Фикс

<что изменилось; красный тест теперь зелёный>

## Профилактика

<как класс бага закрыт: тест/инвариант/линтер>
```
