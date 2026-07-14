import { tool } from "@opencode-ai/plugin";
import { execFileSync } from "child_process";
import { writeFileSync, mkdirSync } from "fs";
import { join } from "path";

const WIKI_ROOT = process.env.WIKI_ROOT ?? "D:\\uwuwu\\wiki";
const REQUESTS_DIR = join(WIKI_ROOT, ".requests");
const WIKI_CLI = "uwuwu-cli";

const ENCODING = "utf-8" as const;

function runWikiCli(args: readonly string[], timeoutMs: number, maxBufferMb: number): string {
    return execFileSync(WIKI_CLI, args, {
        encoding: ENCODING,
        timeout: timeoutMs,
        maxBuffer: maxBufferMb * 1024 * 1024,
        shell: false,
    });
}

function validateProject(project: string): string {
    if (!/^[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(project)) {
        throw new Error(
            `Invalid project name (allowed: alphanumeric, dot, dash, underscore; first char must be alphanumeric): ${project}`,
        );
    }
    return project;
}

export const wiki_search = tool({
    description:
        "Search uwuwu_wiki experience/ for howto/tech articles. Returns a compact list (title + description, score, filepath) for top 5 matches above threshold 0.3 — NOT full article bodies. Call wiki_get with the returned filepath for full text.\n\nAccess docs (credentials, topology) have moved to `projects/<P>/access/` — use `access_search` instead.",
    args: {
        query: tool.schema
            .string()
            .describe("Search query — be descriptive for better results"),
    },
    async execute(args) {
        try {
            return runWikiCli(
                ["wiki", "search", "experience", args.query, "--top", "5"],
                120000,
                50,
            );
        } catch (e) {
            return `Search failed: ${e}`;
        }
    },
});

export const wiki_grep = tool({
    description:
        "Grep through uwuwu_wiki experience/ articles using regex. Returns matching lines with file paths and line numbers. Use to find specific config values, API names, error messages.\n\nFor access docs use `access_grep`.",
    args: {
        pattern: tool.schema
            .string()
            .describe("Regex pattern to search for"),
    },
    async execute(args) {
        try {
            return runWikiCli(["wiki", "grep", "experience", args.pattern], 30000, 10);
        } catch (e) {
            return `Grep failed: ${e}`;
        }
    },
});

export const wiki_get = tool({
    description:
        "Get full content of a wiki experience/ article by path. Returns the article body without frontmatter. Use after wiki_grep to read the full document.",
    args: {
        path: tool.schema
            .string()
            .describe("Document path (e.g. 'experience/rust/axum.md')"),
    },
    async execute(args) {
        try {
            return runWikiCli(["wiki", "get", args.path], 10000, 10);
        } catch (e) {
            return `Get failed: ${e}`;
        }
    },
});

export const wiki_request = tool({
    description:
        "Create a change request for uwuwu_wiki (create/update/delete article). Saved to .requests/ for manual review — does NOT modify wiki directly.",
    args: {
        action: tool.schema
            .string()
            .describe("Action: create, update, or delete"),
        content: tool.schema
            .string()
            .describe("Article content (create/update) or target filepath (delete)"),
        reason: tool.schema
            .string()
            .describe("Why this change is needed"),
    },
    async execute(args) {
        mkdirSync(REQUESTS_DIR, { recursive: true });

        const now = new Date();
        const pad = (n: number) => String(n).padStart(2, "0");
        const ts = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}T${pad(now.getHours())}${pad(now.getMinutes())}`;
        const slug = args.reason
            .toLowerCase()
            .replace(/[^a-z0-9-]/g, "-")
            .replace(/-+/g, "-")
            .slice(0, 60);
        const filename = `${ts}_${args.action}_${slug}.md`;
        const filepath = join(REQUESTS_DIR, filename);

        const body = `---\ntype: ${args.action}\nreason: ${args.reason}\ncreated: ${now.toISOString()}\n---\n\n${args.content}\n`;

        writeFileSync(filepath, body, ENCODING);
        return `Request saved: ${filepath}`;
    },
});

export const projects = tool({
    description:
        "Список всех проектов в uwuwu_wiki с полным README каждого. Используй чтобы понять в каких проектах ты работаешь и что они из себя представляют.",
    args: {},
    async execute() {
        try {
            return runWikiCli(["projects"], 10000, 10);
        } catch (e) {
            return `Projects failed: ${e}`;
        }
    },
});

export const task_search = tool({
    description:
        "Семантический поиск по задачам проекта в uwuwu_wiki/projects/<project>/tasks/. Возвращает компактный список (title + description, score, filepath) для топ-5 совпадений.\n\nОбязательный arg: `project`. Опциональные фильтры (через AND): `status` (`open | in_progress | blocked | closed`), `from`/`to` (`YYYY-MM-DD`, по `created`, inclusive).",
    args: {
        project: tool.schema
            .string()
            .describe("Имя проекта (подпапка в projects/). Обязательный."),
        query: tool.schema
            .string()
            .describe("Поисковый запрос — описывай детально для лучшего результата"),
        status: tool.schema
            .string()
            .optional()
            .describe("Точный матч статуса: open | in_progress | blocked | closed"),
        from: tool.schema
            .string()
            .optional()
            .describe("Нижняя граница по дате создания (inclusive): YYYY-MM-DD"),
        to: tool.schema
            .string()
            .optional()
            .describe("Верхняя граница по дате создания (inclusive): YYYY-MM-DD"),
    },
    async execute(args) {
        try {
            const cliArgs: string[] = [
                "task", "search",
                validateProject(args.project),
                args.query,
                "--top", "5",
            ];
            if (args.status) cliArgs.push("--status", args.status);
            if (args.from) cliArgs.push("--from", args.from);
            if (args.to) cliArgs.push("--to", args.to);
            return runWikiCli(cliArgs, 120000, 50);
        } catch (e) {
            return `Task search failed: ${e}`;
        }
    },
});

export const task_list = tool({
    description:
        "Простой список задач проекта по статусу (без embeddings-ранжирования, sorted by created). Обязательные args: `project`, `status`. Опциональные: `from`/`to`.",
    args: {
        project: tool.schema
            .string()
            .describe("Имя проекта. Обязательный."),
        status: tool.schema
            .string()
            .describe("Статус (точный матч): open | in_progress | blocked | closed. Обязательный."),
        from: tool.schema
            .string()
            .optional()
            .describe("Нижняя граница по дате создания (inclusive): YYYY-MM-DD"),
        to: tool.schema
            .string()
            .optional()
            .describe("Верхняя граница по дате создания (inclusive): YYYY-MM-DD"),
    },
    async execute(args) {
        try {
            const cliArgs: string[] = [
                "task", "list",
                validateProject(args.project),
                "--status", args.status,
            ];
            if (args.from) cliArgs.push("--from", args.from);
            if (args.to) cliArgs.push("--to", args.to);
            return runWikiCli(cliArgs, 10000, 10);
        } catch (e) {
            return `Task list failed: ${e}`;
        }
    },
});

export const task_grep = tool({
    description:
        "Regex-поиск по задачам проекта. Возвращает совпадающие строки с file paths и line numbers.",
    args: {
        project: tool.schema
            .string()
            .describe("Имя проекта. Обязательный."),
        pattern: tool.schema
            .string()
            .describe("Regex-паттерн для поиска"),
    },
    async execute(args) {
        try {
            return runWikiCli(
                ["task", "grep", validateProject(args.project), args.pattern],
                30000,
                10,
            );
        } catch (e) {
            return `Task grep failed: ${e}`;
        }
    },
});

export const task_get = tool({
    description:
        "Прочитать задачу целиком (полное содержимое: frontmatter + body + worklogs). Args: `project`, `slug` (filename без .md).",
    args: {
        project: tool.schema
            .string()
            .describe("Имя проекта. Обязательный."),
        slug: tool.schema
            .string()
            .describe("Slug задачи (filename без .md)"),
    },
    async execute(args) {
        try {
            return runWikiCli(
                ["task", "get", validateProject(args.project), args.slug],
                10000,
                10,
            );
        } catch (e) {
            return `Task get failed: ${e}`;
        }
    },
});

export const task_worklog = tool({
    description:
        "Дописать worklog-запись в конец задачи. Используй ПРИ ЛЮБОМ изменении контекста по задаче: что сделал, что осталось, блокеры, следующий шаг. Args: `project`, `slug`, `text`.",
    args: {
        project: tool.schema
            .string()
            .describe("Имя проекта. Обязательный."),
        slug: tool.schema
            .string()
            .describe("Slug задачи (filename без .md)"),
        text: tool.schema
            .string()
            .describe("Текст worklog'а"),
    },
    async execute(args) {
        try {
            return runWikiCli(
                ["task", "worklog", validateProject(args.project), args.slug, args.text],
                10000,
                10,
            );
        } catch (e) {
            return `Task worklog failed: ${e}`;
        }
    },
});

export const task_create = tool({
    description:
        "Создать новую задачу. Используй когда задача рождена в чате с пользователем (не пришла извне). Args: `project`, `task_key` (filename без .md), `content` (body), опциональные `title`/`description`/`tags`. Если `title` и `description` оба не переданы — запускается авто-enrich (non-fatal).",
    args: {
        project: tool.schema
            .string()
            .describe("Имя проекта. Обязательный."),
        task_key: tool.schema
            .string()
            .describe("Имя файла без .md"),
        content: tool.schema
            .string()
            .describe("Тело задачи (markdown). Непустой."),
        title: tool.schema
            .string()
            .optional()
            .describe("Title (EN, frontmatter)"),
        description: tool.schema
            .string()
            .optional()
            .describe("Description (EN, frontmatter)"),
        tags: tool.schema
            .array(tool.schema.string())
            .optional()
            .describe("Теги (inline-массив)"),
    },
    async execute(args) {
        try {
            const cliArgs: string[] = [
                "task", "create",
                validateProject(args.project),
                args.task_key,
                "--content", args.content,
            ];
            if (args.title) cliArgs.push("--title", args.title);
            if (args.description) cliArgs.push("--description", args.description);
            if (args.tags) {
                for (const t of args.tags) cliArgs.push("--tag", t);
            }
            return runWikiCli(cliArgs, 180000, 10);
        } catch (e) {
            return `Task create failed: ${e}`;
        }
    },
});

export const task_set_status = tool({
    description:
        "Сменить статус задачи. Args: `project`, `slug`, `status` (`open | in_progress | blocked | closed`). Обновляет `updated` в frontmatter.",
    args: {
        project: tool.schema
            .string()
            .describe("Имя проекта. Обязательный."),
        slug: tool.schema
            .string()
            .describe("Slug задачи (filename без .md)"),
        status: tool.schema
            .string()
            .describe("Новый статус: open | in_progress | blocked | closed"),
    },
    async execute(args) {
        try {
            return runWikiCli(
                ["task", "set-status", validateProject(args.project), args.slug, args.status],
                10000,
                10,
            );
        } catch (e) {
            return `Task set-status failed: ${e}`;
        }
    },
});

export const access_search = tool({
    description:
        "Семантический поиск по access-документам проекта (credentials, topology, stands). Возвращает компактный список для топ-5 совпадений. Args: `project` (обязательный), `query`.",
    args: {
        project: tool.schema
            .string()
            .describe("Имя проекта. Обязательный."),
        query: tool.schema
            .string()
            .describe("Поисковый запрос"),
    },
    async execute(args) {
        try {
            return runWikiCli(
                ["access", "search", validateProject(args.project), args.query, "--top", "5"],
                120000,
                50,
            );
        } catch (e) {
            return `Access search failed: ${e}`;
        }
    },
});

export const access_grep = tool({
    description:
        "Regex-поиск по access-документам проекта. Args: `project` (обязательный), `pattern`.",
    args: {
        project: tool.schema
            .string()
            .describe("Имя проекта. Обязательный."),
        pattern: tool.schema
            .string()
            .describe("Regex-паттерн"),
    },
    async execute(args) {
        try {
            return runWikiCli(
                ["access", "grep", validateProject(args.project), args.pattern],
                30000,
                10,
            );
        } catch (e) {
            return `Access grep failed: ${e}`;
        }
    },
});

export const access_get = tool({
    description:
        "Прочитать access-документ проекта (credentials, topology). Args: `project`, `slug` (filename без .md).",
    args: {
        project: tool.schema
            .string()
            .describe("Имя проекта. Обязательный."),
        slug: tool.schema
            .string()
            .describe("Slug документа (filename без .md)"),
    },
    async execute(args) {
        try {
            return runWikiCli(
                ["access", "get", validateProject(args.project), args.slug],
                10000,
                10,
            );
        } catch (e) {
            return `Access get failed: ${e}`;
        }
    },
});
