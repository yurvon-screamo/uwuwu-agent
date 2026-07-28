# AGENTS.md

ALWAYS RESPOND IN RUSSIAN.

- Provide the user with a working solution only, unless the plan explicitly requires otherwise
- NEVER PERFORM UNSAFE GIT OPERATIONS
- NEVER DELETE CODE YOU DON'T UNDERSTAND!
- NEVER HIDE LINTER ISSUES — FIX THEM OR AT LEAST IGNORE THEM, BUT DON'T HIDE THEM!
- ALWAYS PROACTIVELY USE the `qlty` formatting and linting tool after making code changes.
- Any action that may lead to theoretical data loss must be preceded by creating a backup

SKILLS rules:

- ALWAYS PROACTIVELY USE SKILLS.
- ALWAYS use SKILLS if there are relevant ones for the task. This is VERY important.
- ALWAYS invoke ALL relevant SKILLS. Don't limit yourself to just one if you see other relevant skills. This is VERY important.

## uwuwu-cli

uwuwu-cli — это cli утилита для **база знаний-инструкций** (howto, gotchas, конфиги, credentials, топология, задачи, таск-трекинг).

ALWAYS search in the wiki: call `uwuwu-cli wiki search/grep/get` with a descriptive query to find relevant experience articles.

When you learn something new or find outdated info, create a change request via `uwuwu-cli wiki request`.

Wiki - это знаний, не привязанные к конкретным проектам.

`uwuwu-cli projects` — список всех проектов с полным README. Используй чтобы понять в каких проектах ты работаешь.

`uwuwu-cli access search/grep/get` — поиск и чтение access-документов проекта (credentials, topology, stands).

`uwuwu-cli task` - личный таск-трекер. При начале работы найди с какой задачей ты работаешь.

Ты можешь заводить задачи — для задач, рождённых **в чате с пользователем**, которы ранее не было заведены в трекер.

По окончанию работы обязательно дописывай worklog к задаче.

## cli-tools

| Команда       | Назначение                                                                        | Документация                                |
| ------------- | --------------------------------------------------------------------------------- | ------------------------------------------- |
| `atlassian`   | Jira и Confluence через MCP-сервер `mcp-atlassian`. Auth — Personal Access Token. | `wiki/experience/cli/atlassian-cli.md`      |
| `grep-github` | Поиск реального кода по публичным репозиториям GitHub через grep.app.             | `wiki/experience/cli/research-toolchain.md` |

## Серверы и долгие процессы

Серверы, dev-серверы, вотчеры и любые долгие процессы — запускай тулом `bg_start`, а не `bash`, иначе сессия заблокируется.

## Уточнение требований

ПЕРЕД работой проанализируй задачу. Если хоть что-то непонятно — ОСТАНОВИСЬ и уточни:

- Нужен ответ юзера (на любой фазе) → верни маркер `NEEDS_CLARIFICATION` с конкретными вопросами
- Технический тупик (упал инструмент, не могу прочитать файл и т.п.) → верни маркер `BLOCKED`

Не догадывайся — лучше потратить время на уточнение, чем сделать неправильно.
Если задачу можно интерпретировать по-разному — спроси, какая интерпретация верна.

> **Эффект накопления:** ИИ-агенты реализуют размытые требования буквально или изобретательно интерпретируют их — и делают это в 10 раз быстрее людей.
> Неправильный код становится шаблоном для будущих генераций: каждая реализованная «наугад» функция множит технический долг.

## Не борись с ошибками

Если столкнулся с одной и той же ошибкой ДВАЖДЫ — ОСТАНОВИСЬ.

Не пытайся «продавить» решение подбором вариантов — это мотание туда-сюда, которое тратит контекст и время без результата.

**Порядок действий:**

1. Первая ошибка → попробуй исправить, это нормально
2. Та же ошибка снова → НЕМЕДЛЕННО ОСТАНОВИСЬ и верни `BLOCKED`:
    - Полный текст ошибки
    - Что уже пробовал
    - Гипотеза причины
