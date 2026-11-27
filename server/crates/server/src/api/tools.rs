//! Реестр MCP-инструментов: дескрипторы + диспетчер. Тонкие обёртки
//! юзкейсов (views) — никакой бизнес-логики в presentation.

use base64::Engine as _;
use serde_json::{json, Value};
use std::sync::Arc;
use uwuwu_usecase::docs_delete::{
    guard as delete_guard, DeleteReceipt, DeleteRequest, DocsDeleteUseCase,
};
use uwuwu_usecase::{
    cron_journal, fm_guard, jst_stamp, sanitize_slug, slug_guard, BlobPlace, BlobUseCase, Clock,
    DocsView, PersonView, PostCheck, SectionWrite, TaskView, TaskWriteUseCase, TaskWriter,
    UsecaseError,
};

pub struct ToolHub {
    pub tasks: Arc<TaskView>,
    pub docs: Arc<DocsView>,
    pub person: Arc<PersonView>,
    pub writer: Arc<TaskWriteUseCase>,
    /// Батч-снос строк доски (#1158): guard-вердикты + write-ahead undo.
    pub clean: Arc<uwuwu_usecase::TaskCleanUseCase>,
    /// Rename статуса канбана (#1447): pre-image журнал → мутация → verify.
    pub status: Arc<uwuwu_usecase::BoardStatusRenameUseCase>,
    pub wiki: Arc<dyn uwuwu_usecase::WikiRepo>,
    /// Роутер «проект → executor»: session_export веером по исполнителям.
    pub sessions: Arc<uwuwu_usecase::SessionRouter>,
    pub search: Arc<uwuwu_usecase::SearchUseCase>,
    pub crons: Arc<uwuwu_usecase::CronUseCase>,
    /// Blob-канал (#1292): вложения к задачам/докам через приватный S3.
    pub blobs: Arc<BlobUseCase>,
    /// Реестр целей проекта (#1216): milestones hub, префикс [проект] в тайтле.
    pub goals: Arc<uwuwu_usecase::GoalsView>,
}

/// Дефолтный проект для инструментов без явного project.
pub const DEFAULT_PROJECT: &str = "uwuwu";

/// Мутационные тулы: имена — семантический факт «тул мутирует носитель»
/// (в дескрипторах не кодируется). Новый мутационный тул — добавь имя
/// сюда; ключи гард возьмёт из дескриптора сам. Тул из списка без
/// дескриптора гардится громко (всё чужое) — дескриптор обязателен.
const MUTATION_TOOLS: &[&str] = &[
    "task_create",
    "task_edit",
    "task_claim",
    "task_comment",
    "task_close",
    "task_clean",
    "task_undo",
    "board_status_rename",
    "task_body",
    "task_body_section",
    "blob_put",
    "docs_write",
    "docs_append",
    "docs_delete",
];

/// Ключи диспетчера: легальны на любом мутационном туле. Семантика
/// честная (ревью #1572 Low): диспетчер читает project на каждом вызове
/// (дефолт uwuwu, канон «проект — канал вызова»); на тулах без
/// маршрутизации доски ключ принимается и не влияет на поведение —
/// канон предписывает агентам передавать project всюду. В текст отказа
/// «the tool knows» не входят — рекламировать надо только ключи схемы.
const DISPATCHER_KEYS: &[&str] = &["project"];

/// Читающие тулы: явное дополнение MUTATION_TOOLS до каталога
/// (ревью #1572, Common). Пин catalog_partition_... держит разбиение
/// «мутационные ∪ читающие = каталог»: новый тул без записи в один из
/// списков валит тест — тихий пропуск гарда исключён.
/// Тест-носитель: в рантайме гард не нужен.
#[cfg(test)]
const READ_TOOLS: &[&str] = &[
    "task_list",
    "task_get",
    "docs_ls",
    "docs_read",
    "person_show",
    "goal_list",
    "projects",
    "cron_list",
    "search",
    "session_export",
    "blob_get",
];

/// Legacy-ключи: читаются хендлерами, в схему не рекламируются —
/// осознанное решение #1232 («legacy-ключ не рекламится (но читается)»,
/// пин descriptors_expose_automerge). Единственное исключение из
/// дескриптор-вывода, перечислено явно: (тул, ключ).
const LEGACY_KEYS: &[(&str, &str)] = &[("task_create", "autodone")];

/// Разрешённые ключи мутационного тула: legal — что принимает хендлер
/// (inputSchema дескриптора + LEGACY_KEYS), advertised — что отказ
/// рекламирует в «the tool knows» (только схема: legacy-ключи и
/// диспетчерский project легальны молча — пин решения #1232 «legacy-ключ
/// не рекламится (но читается)», ревью #1572 дельта-раунд 3). Some(пусто)
/// — тул из списка без дескриптора: гардится громко; None — не
/// мутационный (читающий: принадлежность проверена пином
/// catalog_partition_mutation_or_read_is_total — каждый тул каталога
/// заявлен ровно в одном списке).
fn mutation_keys(name: &str) -> Option<(Vec<String>, Vec<String>)> {
    if !MUTATION_TOOLS.contains(&name) {
        return None;
    }
    let list = tool_descriptors();
    let props = list
        .as_array()?
        .iter()
        .find(|t| t["name"].as_str() == Some(name))
        .map(|t| &t["inputSchema"]["properties"]);
    let Some(props) = props else {
        return Some((Vec::new(), Vec::new()));
    };
    let schema: Vec<String> = props
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let mut legal = schema.clone();
    for (tool, key) in LEGACY_KEYS {
        if *tool == name {
            legal.push((*key).to_string());
        }
    }
    Some((legal, schema))
}

/// Гард чужих ключей мутационной поверхности (#1572, сценарий 2): вызов
/// с ключами вне схемы — громкий отказ до мутации; носитель не трогается,
/// контент не передан. Отказ вместо тихого strip (ADR-065: «стрип
/// маскировал бы ошибку навсегда»); история деградации — вторым
/// предложением (ревью #1572 Low: прошедшее время читалось как «прошло»).
fn refuse_unknown_args(name: &str, args: &Value) -> Result<(), String> {
    let Some((legal, advertised)) = mutation_keys(name) else {
        return Ok(());
    };
    let mut unknown: Vec<&str> = Vec::new();
    if let Some(map) = args.as_object() {
        for key in map.keys() {
            if DISPATCHER_KEYS.contains(&key.as_str()) || legal.iter().any(|a| a == key) {
                continue;
            }
            unknown.push(key.as_str());
        }
    }
    if unknown.is_empty() {
        return Ok(());
    }
    Err(format!(
        "unknown argument(s) {} for {name} — the write is REFUSED, the carrier is untouched \
         (no content was passed); the tool knows: {} — re-send with the schema keys (unknown \
         keys used to be silently ignored and degraded the call to a partial write, \
         bug #1572)",
        unknown.join(", "),
        advertised.join(", ")
    ))
}

impl ToolHub {
    pub fn descriptors(&self) -> Value {
        tool_descriptors()
    }
}

/// Реестр инструментов MCP (сверка канона: «чего нет в списке — того нет»).
pub fn tool_descriptors() -> Value {
    json!([
        tool(
            "task_list",
            "JSON array of rows: number, status, priority, type, \
             title; empty cells are null; filter/aggregate in script — \
             the full list may exceed the model-facing output window",
            object(json!({ "project": string("project name; default uwuwu") }))
        ),
        tool(
            "task_get",
            "Full task card: fm fields (incl. body_bytes), issue body, \
             latest comments (shown/total declared)",
            object(json!({ "number": integer("task number (row-id)"),
                         "project": string("project; default uwuwu") }))
        ),
        tool(
            "docs_ls",
            "List wiki .md docs under prefix (slugs without docs/ and .md); on file/dir collision the row reads «X (ambiguous: file … + folder …)» — slug for docs_read up to « (ambiguous»",
            object(json!({ "path": string("prefix: project/folder; empty — whole wiki") }))
        ),
        tool(
            "docs_read",
            "Doc markdown by slug (without docs/ and .md)",
            object(json!({ "slug": string("slug: Projects/uwuwu/README, Experience/...") }))
        ),
        tool(
            "person_show",
            "Owner persona (language/style/grains) — agents read it at the start of work",
            object(json!({}))
        ),
        tool(
            "goal_list",
            "Project long-term goal registry: open hub-repo milestones (the title prefixed with [project]) — read before working on a task; work against an open goal is flagged to the owner",
            object(json!({ "project": string("project name; default uwuwu") }))
        ),
        tool(
            "projects",
            "Pipeline projects (boards in the server config)",
            object(json!({}))
        ),
        tool(
            "task_create",
            "Create a task on the project board (Analys; title unique; direct — straight to In Progress)",
            object(json!({
                "title": string("title (unique on the board)"),
                "project": string("project; default uwuwu"),
                "body": string("markdown body (problem statement); a copied task_get header and comments tail are stripped"),
                "type": string(&type_enum_list()),
                "priority": string("high|medium|low; default medium"),
                "complexity": string("regular|high|low; default regular; shifts the model tier by 0/+1/-1"),
                "automerge": json!({"type":"boolean","description":"automerge: merge the PR on entering Review, the row stays in Review; default true (direct — false)"}),
                "direct": json!({"type":"boolean","description":"direct task (#1357): row goes straight to In Progress, the agent finalizes to Review itself bypassing factory stages. Only on the owner's direct order — verbatim quote of the order in the body; default false"}),
                "fact_track": json!({"type":"boolean","description":"fact_track task (#1631): shortened pipe without Design and Testing — only In Progress → Result review → Review. Create-only, immutable, mutually exclusive with direct; default false"})
            }))
        ),
        tool(
            "task_edit",
            "Edit task cells: status/branch/PR/priority/title/type/complexity",
            object(json!({
                "number": integer("task number"),
                "project": string("project; default uwuwu"),
                "status": string("canon status: Draft/Analys/Review Analys/Todo/Design/Plan review/In Progress/Result review/Testing/Testing review/Review/Needs attention/Wait"),
                "branch": string("branch name"),
                "pr": string("PR URL"),
                "priority": string("high|medium|low"),
                "complexity": string("regular|high|low; empty — leave untouched"),
                "title": string("new title"),
                "type": string(&(type_enum_list() + "; empty — leave untouched")),
                "automerge": json!({"type":"boolean","description":"automerge on/off (back-tagging old rows)"}),
                "owner_answer": string("verbatim owner reply; required when clearing Needs attention to the source stage (fix the reply in the body/thread first). Ignored with a note when the row is not in Needs attention")
            }))
        ),
        tool(
            "task_claim",
            "Claim your session into the dev slot (the factory will not spawn a duplicate)",
            object(json!({
                "number": integer("task number"),
                "session": string("your session id (ses_…)")
            }))
        ),
        tool(
            "task_comment",
            "Comment on a task (question/report to the owner; wakes agents)",
            object(json!({
                "number": integer("task number"),
                "text": string("comment text"),
                "reply_to": string("quote anchor for thread reply (optional)")
            }))
        ),
        tool(
            "task_close",
            "Close the task (Done; reason not_planned for rejections)",
            object(json!({
                "number": integer("task number"),
                "not_planned": json!({"type":"boolean","description":"true — rejected/duplicate, false — done"})
            }))
        ),
        tool(
            "task_clean",
            "Batch board-row cleanup with a guard (#1158): dry-run with verdicts \
             (Live/Dead/DraftSkeleton/NoRow/Divergent) — default, zero mutations; \
             confirm=true executes. Non-Draft rows (Live/Dead) require the owner's ack \
             — without it the whole batch is rejected; a write-ahead journal \
             provides undo (task_undo)",
            object(json!({
                "ids": json!({
                    "description": "row numbers: csv «1,2,3» or an array of integers",
                    "oneOf": [
                        { "type": "string" },
                        { "type": "array", "items": { "type": "integer" } }
                    ]
                }),
                "project": string("project; default uwuwu"),
                "confirm": json!({"type":"boolean","description":"false/absent — dry-run (verdicts), true — delete"}),
                "ack": json!({
                    "description": "non-Draft row numbers (Live/Dead) confirmed \
                                    by the owner: csv or array of integers",
                    "oneOf": [
                        { "type": "string" },
                        { "type": "array", "items": { "type": "integer" } }
                    ]
                })
            }))
        ),
        tool(
            "task_undo",
            "Undo row deletion from the undo journal (idempotent): recreate item + \
             restore status; without journal — the project's latest one",
            object(json!({
                "project": string("project; default uwuwu"),
                "journal": string("journal id from task_clean; optional — latest")
            }))
        ),
        tool(
            "board_status_rename",
            "Rename a board Status option by option id (#1447): fresh pre-image \
             (option set + row distribution) is written to the write-ahead journal, \
             then the carrier receives the verbatim pre-image set with one name \
             changed, then fresh post-verify; any divergence is a loud refusal with \
             the journal path for manual rollback. Non-destructive by id: row values \
             follow the renamed option. Replacing the whole option set is NOT exposed",
            object(json!({
                "from": string("current option name"),
                "to": string("new option name (canonical board statuses parse; \
                              non-canonical names get a warning, not a refusal)"),
                "project": string("проект; дефолт uwuwu")
            }))
        ),
        tool(
            "docs_append",
            "Append a line to the end of an EXISTING wiki doc (experience articles, doc appendices; ff-only commit). Never creates a file: a missing doc is refused append-nonexistent with suffix candidates and the hint to create via docs_write",
            object(json!({
                "slug": string("doc slug (without docs/ and .md); sanitized like docs_read"),
                "line": string("the whole line (markdown)"),
                "message": string("commit message; default append")
            }))
        ),
        tool(
            "docs_write",
            "Write doc by slug (full replace); fm header survives: carried over with fresh lastmod, canonical one created when absent, fm in the payload kept verbatim. Slug is sanitized like docs_read. New docs are refused (nothing written) when outside the wiki root (root-prefix: Experience, Projects, bot, cron, person), under a live file that is not a folder (parent-collision), or colliding with a live doc by path suffix (suffix-collision) — each refusal names the branch and candidates. When the tree read fails, collision checks are skipped; the receipt names the degradation and the server logs a warning",
            object(json!({
                "slug": string("doc slug (without docs/ and .md); sanitized like docs_read"),
                "content": string("full markdown"),
                "message": string("commit message; default write")
            }))
        ),
        tool(
            "docs_delete",
            "Delete a wiki doc by full slug. A verbatim backup is stored in the private blob bucket BEFORE the ff-only delete; the receipt names the branch (deleted, absent, refused), the backup key and the post-check. Refuses slugs outside the wiki roots; a longer suffix twin is reported as information, not a refusal. When the tree read fails, the delete is refused before mutation: the error names the degradation; the snapshot is checked against the commit tree",
            object(json!({
                "slug": string("doc slug (without docs/ and .md); sanitized like docs_read"),
                "message": string("commit message; default delete")
            }))
        ),
        tool(
            "cron_list",
            "Cron inventory (masters /cron/<name>/README.md: schedule + machine accounting — last fire, fail counter)",
            object(json!({
                "project": string("filter by project (master fm project); without it — all crons")
            }))
        ),
        tool(
            "search",
            "Hybrid wiki search: full-text + semantic (if the index is alive)",
            object(json!({
                "query": string("search query (AND tokens + vector)"),
                "top": json!({"type":"integer","description":"hits per section; default 5"})
            }))
        ),
        tool(
            "session_export",
            "Export an opencode session (content for the dreamer/audit)",
            object(json!({
                "session": string("session id (ses_…)")
            }))
        ),
        tool(
            "task_body",
            "Full replacement of the task body (fm is not written into the body — fields are \
             native; a copied task_get header and comments tail are stripped on write; edit \
             large bodies sectionally via task_body_section). Write, not read: without the \
             body parameter — loud refusal; explicit empty string clears the body",
            object(json!({
                "number": integer("task number"),
                "project": string("project; default uwuwu"),
                "body": string("full markdown body (no fm); a copied task_get header and \
                              comments tail are stripped on write; ABSENT parameter is a loud \
                              refusal (read via task_get); empty or whitespace-only string — \
                              clear deliberately; fm/task_get-header-only input or a comments \
                              tail without body is refused (no effective content)")
            }))
        ),
        tool(
            "task_body_section",
            "Sectional edit of the task body: a section delta instead of the full body \
             (large bodies — this path only); freshness — base_hash; radical shrink (a third \
             of the carrier or a big section cut) refuses — deliberate shrink repeats with \
             confirm: true",
            object(json!({
                "number": integer("task number"),
                "project": string("project; default uwuwu"),
                "section": string("section heading; ATX prefix pins the level \
                                  («### X» — h3), bare/«## X» — default h2"),
                "content": string("new section content without the heading; a copied \
                                  task_get comments tail is stripped after the body is \
                                  assembled; ABSENT parameter is a loud refusal (write, not \
                                  read); empty string — clear the section deliberately"),
                "base_hash": string("body_hash of the body at read time (fm block of \
                                    task_get or a prior receipt); required"),
                "confirm": json!({"type":"boolean","description":"true — deliberately \
                                  re-run a write refused by the shrink guard"})
            }))
        ),
        tool(
            "blob_put",
            "Attach a file to a task/doc in the private bucket (Tigris). Give \
             data_base64 for a small file. Omit data_base64 for a presigned PUT \
             URL: upload the bytes with curl from the shell, then use the key \
             and the GET URL from the same receipt",
            object(json!({
                "filename": string("file name with extension — content-type \
                                   is derived from it"),
                "data_base64": string("file content base64 (limit 20 MiB); omit \
                                      to reserve a key and get a presigned PUT \
                                      URL with a 5-minute TTL"),
                "number": integer("task number: key blobs/<project>/<number>/…; \
                                  without it — misc segment"),
                "project": string("project; default uwuwu")
            }))
        ),
        tool(
            "blob_get",
            "Fresh presigned URL for a blob by key — re-signs an expired one; \
             object existence is not checked. Keys: the blob_put canon \
             (blobs/…) or a legacy assets/… key — attachments migrated from the \
             hub repo into the bucket (#1365)",
            object(json!({ "key": string("key from blob_put (blobs/…) or \
                             a legacy key of a migrated attachment (assets/<path>)") }))
        ),
    ])
}

impl ToolHub {
    /// Диспетчер MCP-вызовов: разбор конверта + таблица «имя → хендлер».
    /// Логика каждого тула живёт в одноимённом хендлере ниже.
    pub async fn call(&self, msg: &Value) -> Result<Value, String> {
        let name = msg["params"]["name"].as_str().unwrap_or_default();
        let args = &msg["params"]["arguments"];
        // чужие ключи мутаций — отказ до разбора и до носителя (#1572)
        refuse_unknown_args(name, args)?;
        let project = {
            let p = arg_str(args, "project");
            if p.is_empty() {
                DEFAULT_PROJECT.to_string()
            } else {
                p
            }
        };
        match name {
            "projects" => self.projects().await,
            "task_list" => self.task_list(&project).await,
            "task_get" => self.task_get(&project, args).await,
            "docs_ls" => self.docs_ls(args).await,
            "docs_read" => self.docs_read(args).await,
            "person_show" => self.person_show().await,
            "goal_list" => self.goal_list(&project).await,
            "docs_append" => self.docs_append(args).await,
            "docs_write" => self.docs_write(args).await,
            "docs_delete" => self.docs_delete(&project, args).await,
            "cron_list" => self.cron_list(args).await,
            "search" => self.search(args).await,
            "session_export" => self.session_export(args).await,
            "task_create" => self.task_create(&project, args).await,
            "task_edit" => self.task_edit(&project, args).await,
            "task_claim" => self.task_claim(args).await,
            "task_comment" => self.task_comment(args).await,
            "task_close" => self.task_close(args).await,
            "task_clean" => self.task_clean(&project, args).await,
            "task_undo" => self.task_undo(&project, args).await,
            "board_status_rename" => self.board_status_rename(&project, args).await,
            "task_body" => self.task_body(&project, args).await,
            "task_body_section" => self.task_body_section(&project, args).await,
            "blob_put" => self.blob_put(&project, args).await,
            "blob_get" => self.blob_get(args).await,
            other => Err(format!("unknown tool: {other}")),
        }
    }

    async fn projects(&self) -> Result<Value, String> {
        ok_text(self.tasks.projects().join("\n"))
    }

    async fn task_list(&self, project: &str) -> Result<Value, String> {
        let rows = self.tasks.list(project).await.map_err(view_err)?;
        let rows: Vec<Value> = rows
            .iter()
            .map(|t| {
                json!({
                    "number": t.number(),
                    "status": status_cell(t.status()),
                    "priority": priority_cell(t.priority()),
                    "type": t.kind().map(uwuwu_usecase::task_type_name),
                    "title": t.title(),
                })
            })
            .collect();
        // Компактный JSON: машина-потребитель Code Mode получает объект
        // (транспорт авто-парсит текст с «[»); pretty сразу съедает окно
        // показа финального результата (#1604, Common-1)
        let out =
            serde_json::to_string(&rows).map_err(|e| format!("task_list: rows serialise: {e}"))?;
        ok_text(out)
    }

    async fn task_get(&self, project: &str, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        let card = self.tasks.card(project, number).await.map_err(view_err)?;
        let view = card.issue;
        let mut out = format!(
            "#{} {} — {}\n\n--- fm ---\nbranch: {}\npr: {}\nautomerge: {}\ndirect: {}\nfact_track: {}\ncomplexity: {}\ntype: {}\nbody_hash: {}\nbody_bytes: {}\n\n",
            view.task.number(),
            view.task.title(),
            uwuwu_usecase::status_name(view.task.status()),
            view.task.branch().map(|b| b.as_str()).unwrap_or("—"),
            view.task.pr().map(|u| u.as_str()).unwrap_or("—"),
            view.task.automerge(),
            view.task.direct(),
            view.task.fact_track(),
            uwuwu_usecase::complexity_name(view.task.complexity()),
            // канонное имя (словарь MCP-поверхности); None — «—» как
            // branch/pr (native-null хвост, дефолт Task; #1271)
            view.task
                .kind()
                .map(uwuwu_usecase::task_type_name)
                .unwrap_or("—"),
            uwuwu_usecase::body_hash(&view.body),
            // состав вывода заявлен (#1604): тело отдельно от хвоста
            // комментов; потребитель сверяет TextEncoder'ом — байты UTF-8,
            // те же единицы, что в квитанциях write-path
            view.body.len(),
        );
        out.push_str(&view.body);
        if !view.comments.is_empty() {
            out.push_str(&format!(
                "\n\n--- comments (latest {} of {}) ---\n",
                view.comments.len(),
                card.comments_fetched
            ));
            for c in &view.comments {
                out.push_str(&format!(
                    "---\n{}\n{}\n",
                    iso_utc_stamp(c.created_ms),
                    c.body
                ));
            }
        }
        ok_text(out)
    }

    async fn docs_ls(&self, args: &Value) -> Result<Value, String> {
        let list = self
            .docs
            .list(&arg_str(args, "path"))
            .await
            .map_err(view_err)?;
        ok_text(list.join("\n"))
    }

    async fn docs_read(&self, args: &Value) -> Result<Value, String> {
        let md = self
            .docs
            .read(&arg_str(args, "slug"))
            .await
            .map_err(view_err)?;
        ok_text(md)
    }

    async fn person_show(&self) -> Result<Value, String> {
        let md = self.person.show().await.map_err(view_err)?;
        ok_text(md)
    }

    async fn goal_list(&self, project: &str) -> Result<Value, String> {
        // Реестр целей проекта: open milestones hub-репо (тайтл с
        // префиксом [проект]; пишет владелец в UI, агент читает перед
        // работой над задачей) — #1216
        let md = self.goals.list(project).await.map_err(view_err)?;
        ok_text(md)
    }

    async fn docs_append(&self, args: &Value) -> Result<Value, String> {
        require_args(args, &["slug", "line"])?;
        let slug = sanitize_slug(&arg_str(args, "slug"));
        if slug.is_empty() {
            return Err(empty_slug_error());
        }
        // #1576: create-on-missing (#1353) не должен воскрешать снесённый
        // журнал крона — отказ до записи, без мутации носителя. Нормализация
        // слага обязана предшествовать проверке (#1613, пин тестера).
        cron_journal::refuse(&slug)?;
        let message = message_or(args, format!("append: {slug}"));
        // append не создаёт (#1613, развилка Б): несуществующий таргет —
        // доменный отказ писателя; кандидаты суффикса — best-effort из
        // дерева (сбой дерева → отказ без кандидатов, факт от дерева
        // не зависит: писатель читает авторитетно)
        match self
            .wiki
            .append_doc(&slug, &arg_str(args, "line"), &message)
            .await
        {
            Ok(lines) => ok_text(format!("appended to «{slug}» — lines in the doc: {lines}")),
            Err(UsecaseError::AppendMissing(_)) => {
                let candidates = suffix_candidates_best_effort(self.wiki.as_ref(), &slug).await;
                Err(append_missing_error(&slug, &candidates))
            }
            Err(e) => Err(e.to_string()),
        }
    }

    async fn docs_write(&self, args: &Value) -> Result<Value, String> {
        require_args(args, &["slug", "content"])?;
        let slug = sanitize_slug(&arg_str(args, "slug"));
        if slug.is_empty() {
            return Err(empty_slug_error());
        }
        // #1576: тот же запрет для полной перезаписи — writable-цель
        // снесённого журнала не восстанавливается ни одним write-путём.
        // Порядок: sanitize_slug → пустой слаг → refuse → писатель.
        cron_journal::refuse(&slug)?;
        let message = message_or(args, format!("write: {slug}"));
        let incoming = arg_str(args, "content");
        // fm-гвард (#1444): шапка вики-дока переживает replace. Текущий
        // док нужен веткам переноса/создания; сбой чтения громко рвёт
        // запись — кроме входа с собственным fm, той ветке док не нужен
        // (read Err при входе с fm оставляет существование неизвестным).
        let (existing, read_failed) = match self.wiki.read_doc(&slug).await {
            Ok(doc) => (doc, None),
            Err(e) => {
                if fm_guard::split(&incoming).is_none() {
                    return Err(format!(
                        "fm-guard needs the current doc, read «{slug}» failed: {e}"
                    ));
                }
                log_slug_guard_degraded("docs_write", &slug, "doc read failed", &e);
                (None, Some("doc read failed"))
            }
        };
        // слаг-гард (#1613): достоверно новый док — предикат по живому
        // дереву; существующий — быстрый путь без пробы дерева (симметрия
        // #1403). Read Err при входе с fm — существование неизвестно,
        // полный гард невозможен, но root-проверка (ни от чтения, ни от
        // дерева) остаётся — тихого пути семьи не остаётся. Сбой дерева —
        // деградация: root жив (константа), parent/suffix пропущены.
        let mut degraded = read_failed;
        if degraded.is_some() {
            if let Some(candidates) = slug_guard::check_root(&slug) {
                return Err(root_prefix_error(&slug, &candidates));
            }
        } else if existing.is_none() {
            let live = match self.wiki.list_docs("").await {
                Ok(live) => live,
                Err(e) => {
                    log_slug_guard_degraded("docs_write", &slug, "tree unavailable", &e);
                    degraded = Some("tree unavailable");
                    Vec::new()
                }
            };
            match slug_guard::check(&slug, &live) {
                slug_guard::SlugVerdict::RootPrefix(candidates) => {
                    return Err(root_prefix_error(&slug, &candidates));
                }
                slug_guard::SlugVerdict::ParentCollision(parent) => {
                    return Err(parent_collision_error(&parent));
                }
                slug_guard::SlugVerdict::SuffixCollision(candidates) => {
                    return Err(suffix_collision_error(&slug, &candidates));
                }
                slug_guard::SlugVerdict::Clear => {}
            }
        }
        let now = jst_stamp(crate::clock::SystemClock.now_ms());
        let (content, verdict) = fm_guard::guard(existing.as_deref(), &incoming, &slug, &now);
        self.wiki
            .write_doc(&slug, &content, &message)
            .await
            .map_err(|e| e.to_string())?;
        let mut fact = match verdict {
            fm_guard::FmGuard::Verbatim => {
                format!("doc «{slug}» written — fm header from payload kept verbatim")
            }
            fm_guard::FmGuard::Carried => {
                format!("doc «{slug}» written — fm header carried over, lastmod refreshed")
            }
            fm_guard::FmGuard::Created => {
                format!("doc «{slug}» written — canonical fm header created (title/date/lastmod)")
            }
        };
        fact.push_str(&degraded_clause(degraded));
        ok_text(fact)
    }

    /// #1658: удаление дока — слаг-гард → живое чтение → бэкап → снос.
    async fn docs_delete(&self, project: &str, args: &Value) -> Result<Value, String> {
        require_args(args, &["slug"])?;
        let slug = sanitize_slug(&arg_str(args, "slug"));
        if slug.is_empty() {
            return Err(empty_slug_error());
        }
        let (live, degraded) = match self.wiki.list_docs("").await {
            Ok(live) => (live, None),
            Err(e) => {
                log_slug_guard_degraded("docs_delete", &slug, "tree unavailable", &e);
                (Vec::new(), Some("tree unavailable"))
            }
        };
        let verdict = delete_guard(&slug, &live);
        if let Some(refusal) = verdict.refusal {
            return Err(match refusal {
                uwuwu_usecase::docs_delete::DeleteRefusal::RootPrefix(candidates) => {
                    root_prefix_error(&slug, &candidates)
                }
                uwuwu_usecase::docs_delete::DeleteRefusal::ParentCollision(parent) => {
                    parent_collision_error(&parent)
                }
            });
        }
        let usecase = DocsDeleteUseCase::new(self.wiki.clone(), self.blobs.clone());
        let request = DeleteRequest {
            slug: slug.clone(),
            project: project.to_string(),
            message: message_or(args, format!("delete: {slug}")),
            twins: verdict.twins.clone(),
        };
        match usecase.delete(request).await {
            Ok(receipt) => ok_text(delete_receipt_text(&receipt, degraded)),
            Err(UsecaseError::DocAbsent(_)) => Err(delete_absent_error(&slug, &verdict.twins)),
            Err(e) => {
                let mut text = delete_refusal_error(&e);
                if let Some(reason) = degraded {
                    text.push_str(&format!(
                        "\nslug-guard degraded: collision checks skipped ({reason})"
                    ));
                }
                Err(text)
            }
        }
    }

    async fn cron_list(&self, args: &Value) -> Result<Value, String> {
        // Инвентарь /cron (#1214): project — опциональный фильтр по fm,
        // без него — все кроны; учёт ранов — машинный, из state (#1480)
        let filter = arg_str(args, "project");
        let filter = (!filter.is_empty()).then_some(filter.as_str());
        let masters = self
            .crons
            .masters(filter)
            .await
            .map_err(|e| e.to_string())?;
        let mut out = String::new();
        for m in &masters {
            let row = self.crons.state.cron_row(uwuwu_usecase::cron_name(&m.slug));
            let fire = row
                .last_fire_ms
                .map(uwuwu_usecase::jst_stamp)
                .unwrap_or_else(|| "never".into());
            out.push_str(&format!(
                "{} [{}] — `{}` · last fire {} · fails {}\n",
                m.title,
                m.project,
                uwuwu_usecase::schedule_label(&m.schedule),
                fire,
                row.fails
            ));
        }
        ok_text(if out.is_empty() {
            match filter {
                Some(p) => format!("no crons for project {p}"),
                None => "no crons found".to_string(),
            }
        } else {
            out
        })
    }

    async fn search(&self, args: &Value) -> Result<Value, String> {
        let query = arg_str(args, "query");
        if query.is_empty() {
            return Err("query is required".into());
        }
        let top = args.get("top").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
        let hits = self
            .search
            .search(&query, top)
            .await
            .map_err(|e| e.to_string())?;
        let out = hits
            .iter()
            .map(|h| format!("[{}] {} {}", h.section, h.slug, h.score))
            .collect::<Vec<_>>()
            .join("\n");
        ok_text(out)
    }

    async fn session_export(&self, args: &Value) -> Result<Value, String> {
        let session = arg_str(args, "session");
        if !session.starts_with("ses_") {
            return Err("session must be ses_…".into());
        }
        let export = self
            .sessions
            .export_session_any(&session)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(export)
    }

    async fn task_create(&self, project: &str, args: &Value) -> Result<Value, String> {
        let kind = parse_type_arg(&arg_str(args, "type"))?;
        let direct = args
            .get("direct")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // fact_track-класс (#1631): укороченный пайп; create-only,
        // immutable; с direct взаимоисключающ — отказ ниже, в юзкейсе
        let fact_track = args
            .get("fact_track")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // direct-строке дефолт off (план #1357, развилка 5): мерж только по
        // явному мандату владельца (automerge: true в create-вызове), не по
        // умолчанию конвейера. Не-boolean значение (null не считается —
        // клиенты шлют его как «не задано») — громкий отказ: молчаливая
        // интерпретация мусора как on-мандата противоречит инварианту
        // «on — только явным мандатом» (ревью #1357 круг 1→2, Low)
        let automerge_arg = args.get("automerge").or_else(|| args.get("autodone"));
        let automerge = match automerge_arg {
            Some(v) if v.is_boolean() => v.as_bool().unwrap_or(true),
            Some(v) if !v.is_null() => {
                return Err(format!(
                    "automerge: expecting boolean, got {v} — an on-mandate is never interpreted silently"
                ));
            }
            _ => !direct,
        };
        let spec = uwuwu_usecase::NewTask {
            title: arg_str(args, "title"),
            body_md: arg_str(args, "body"),
            kind,
            priority: arg_str(args, "priority"),
            complexity: parse_complexity_arg(args)?.unwrap_or_default(),
            automerge,
            direct,
            fact_track,
        };
        let n = self
            .writer
            .create(project, spec)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(if direct {
            format!("direct task #{n} created (In Progress) on the {project} board")
        } else if fact_track {
            format!("fact_track task #{n} created (Analys) on the {project} board")
        } else {
            format!("task #{n} created (Analys) on the {project} board")
        })
    }

    async fn task_edit(&self, project: &str, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        let kind = parse_type_arg(&arg_str(args, "type"))?;
        let owner_answer = args
            .get("owner_answer")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let status = str_arg(args, "status");
        if owner_answer.is_some() && status.is_none() {
            return Err("owner_answer requires a status transition out of Needs attention".into());
        }
        let patch = uwuwu_usecase::TaskPatch {
            status: status.map(|s| uwuwu_usecase::parse_status(&s)),
            kind,
            branch: str_arg(args, "branch"),
            pr: str_arg(args, "pr"),
            priority: str_arg(args, "priority"),
            complexity: parse_complexity_arg(args)?,
            title: str_arg(args, "title"),
            automerge: args.get("automerge").and_then(|v| v.as_bool()),
            owner_answer,
        };
        if patch.is_empty() {
            return Err("empty patch: set at least one field".into());
        }
        // Честная квитанция статусной записи (#1437): confirmed — носитель
        // подтвердил свежим чтением; unverified — GitHub принял, применения
        // свежие чтения не показали (лаг листа / rate limit): success не
        // заявляется, агенту предписано контрольное чтение НОСИТЕЛЯ.
        let receipt = self
            .writer
            .edit(project, number, patch)
            .await
            .map_err(|e| e.to_string())?;
        let mut text = match receipt.status {
            Some(uwuwu_usecase::StatusDelivery::Confirmed { actual }) => {
                format!("task #{number} updated, carrier confirms: \"{actual}\"")
            }
            Some(uwuwu_usecase::StatusDelivery::Unverified { reason }) => format!(
                "task #{number}: status accepted by GitHub, carrier read did NOT confirm \
                 ({reason}) — verify the CARRIER (gh, board) yourself, do not retry blindly"
            ),
            None => format!("task #{number} updated"),
        };
        if receipt.owner_answer_ignored {
            text.push_str("; owner_answer ignored: the row was not in Needs attention");
        }
        ok_text(text)
    }

    async fn task_claim(&self, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        let session = arg_str(args, "session");
        // валидация — в домене (SessionRef): мусорный id не заявка
        if uwuwu_domain::SessionRef::parse(&session).is_none() {
            return Err("session must be ses_… (a valid opencode id)".into());
        }
        self.writer
            .claim(number, &session, None)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(format!("task #{number}: dev slot claimed by {session}"))
    }

    async fn task_comment(&self, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        let text = arg_str(args, "text");
        let reply = str_arg(args, "reply_to");
        self.writer
            .comment(number, &text, reply.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        ok_text(format!("comment posted to #{number}"))
    }

    async fn task_close(&self, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        let np = args
            .get("not_planned")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        self.writer
            .close(number, np)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(format!("task #{number} closed"))
    }

    async fn task_clean(&self, project: &str, args: &Value) -> Result<Value, String> {
        let numbers = arg_numbers(args, "ids")?;
        let ack = match args.get("ack") {
            None | Some(Value::Null) => Vec::new(),
            _ => arg_numbers(args, "ack")?,
        };
        let confirm = args
            .get("confirm")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !confirm {
            let reports = self
                .clean
                .plan(project, &numbers)
                .await
                .map_err(|e| e.to_string())?;
            ok_text(render_clean_plan(&reports))
        } else {
            let report = self
                .clean
                .execute(project, &numbers, &ack)
                .await
                .map_err(|e| e.to_string())?;
            ok_text(render_clean_exec(project, &report))
        }
    }

    async fn task_undo(&self, project: &str, args: &Value) -> Result<Value, String> {
        let journal = str_arg(args, "journal");
        let report = self
            .clean
            .undo(project, journal.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        ok_text(render_undo(&report))
    }

    /// Rename статуса канбана (#1447): строки тула — английские (#1383),
    /// ошибки юзкейса проходят как есть.
    async fn board_status_rename(&self, project: &str, args: &Value) -> Result<Value, String> {
        let from = str_arg(args, "from").ok_or("from is required (current option name)")?;
        let to = str_arg(args, "to").ok_or("to is required (new option name)")?;
        let report = self
            .status
            .rename(project, &from, &to)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(render_status_rename(&report))
    }

    async fn task_body(&self, project: &str, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        // «параметр отсутствует» ≠ «явно пустая строка» (#1572): Option-разбор
        // на границе; отказ с длиной носителя и рецептом строит юзкейс
        let body = args.get("body").and_then(|v| v.as_str());
        let receipt = self
            .writer
            .set_body(project, number, body)
            .await
            .map_err(|e| e.to_string())?;
        // явная очистка названа актом (ревью #1572 Low): секционный путь
        // тот же акт зовёт «cleared»
        let mut msg = if receipt.body_size == 0 {
            format!(
                "body of #{number} cleared; 0 bytes; body_hash: {}",
                receipt.body_hash
            )
        } else {
            format!(
                "body of #{number} replaced; {} bytes; body_hash: {}",
                receipt.body_size, receipt.body_hash
            )
        };
        if !receipt.verified {
            msg.push_str(delivery_unverified_marker());
        }
        ok_text(msg)
    }

    async fn task_body_section(&self, project: &str, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        let section = arg_str(args, "section");
        let content = args.get("content").and_then(|v| v.as_str());
        let base_hash = arg_str(args, "base_hash");
        // confirm (#1409): осознанное проведение радикального сжатия мимо
        // гарда; без него гард отвечает отказом с длинами было/стало
        let confirm = args
            .get("confirm")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if section.trim().is_empty() {
            return Err("section is required (the section heading)".into());
        }
        if base_hash.trim().is_empty() {
            return Err("base_hash is required (body_hash from the fm of task_get)".into());
        }
        let report = self
            .writer
            .set_body_section(
                project,
                number,
                SectionWrite {
                    section: &section,
                    content,
                    base_hash: &base_hash,
                    confirm,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        ok_text(render_section_report(&section, &report))
    }

    async fn blob_put(&self, project: &str, args: &Value) -> Result<Value, String> {
        require_args(args, &["filename"])?;
        let number = optional_number(args)?;
        let target = BlobPlace {
            project: project.to_string(),
            task: number,
            filename: arg_str(args, "filename"),
        };
        let data_base64 = match args.get("data_base64") {
            None | Some(Value::Null) => return self.blob_put_reserve(&target),
            Some(Value::String(raw)) if !raw.trim().is_empty() => raw.trim(),
            Some(Value::String(_)) => {
                return Err("data_base64 is empty; omit it for the presigned PUT mode".into())
            }
            Some(_) => return Err("data_base64 must be a base64 string".into()),
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_base64)
            .map_err(|e| format!("data_base64 does not decode: {e}"))?;
        let receipt = self
            .blobs
            .place(&target, &bytes)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(format!(
            "key: {}\nURL (TTL 7 days): {}\n{}; {} bytes",
            receipt.key, receipt.url, receipt.content_type, receipt.size
        ))
    }

    fn blob_put_reserve(&self, target: &BlobPlace) -> Result<Value, String> {
        let receipt = self.blobs.reserve(target).map_err(|e| e.to_string())?;
        ok_text(format!(
            "key: {}\nPUT URL (TTL {} s; send Content-Type: {}): {}\n\
             URL (TTL 7 days): {}\n\
             upload the bytes with curl from the shell; verify the GET URL before \
             writing the link, and reuse the key from this receipt",
            receipt.key, receipt.put_ttl_secs, receipt.content_type, receipt.put_url, receipt.url
        ))
    }

    async fn blob_get(&self, args: &Value) -> Result<Value, String> {
        let raw = arg_str(args, "key");
        if raw.trim().is_empty() {
            return Err("key is required".into());
        }
        let key = uwuwu_domain::BlobKey::parse_read(raw.trim()).ok_or(
            "key must look like blobs/<project>/<task|misc>/<id>-<name> \
             or assets/<path of a migrated attachment>",
        )?;
        let url = self.blobs.fresh_url(&key).map_err(|e| e.to_string())?;
        ok_text(format!(
            "URL (TTL 7 days): {url}\n(object existence is not checked — \
             the signature is local; replace the URL in the carrier with text tools)"
        ))
    }
}

fn view_err(e: uwuwu_usecase::ViewError) -> String {
    e.to_string()
}

/// Перечень типов канона для дескрипторов и текстов отказов: один
/// источник с валидацией (пин enum-паритета — зеркало #1298).
fn type_enum_list() -> String {
    uwuwu_usecase::TASK_TYPES
        .iter()
        .map(|t| uwuwu_usecase::task_type_name(*t))
        .collect::<Vec<_>>()
        .join("|")
}

/// Разбор аргумента `type` (create/edit, #1271): пусто — None, мусор —
/// громкий отказ с канонным перечнем до мутаций доски.
fn parse_type_arg(raw: &str) -> Result<Option<uwuwu_domain::TaskType>, String> {
    if raw.is_empty() {
        return Ok(None);
    }
    uwuwu_usecase::parse_task_type(raw)
        .map(Some)
        .ok_or_else(|| format!("unknown type «{raw}»: {}", type_enum_list()))
}

/// Разбор аргумента `complexity` (create/edit, #1689): отсутствие/null —
/// None, не-строка и неканоническая строка — громкий отказ до мутаций доски.
fn parse_complexity_arg(args: &Value) -> Result<Option<String>, String> {
    let raw = match args.get("complexity") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(raw)) => raw,
        Some(other) => {
            return Err(format!(
                "complexity: expecting string (regular|high|low), got {other} — no silent default"
            ))
        }
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    uwuwu_usecase::parse_complexity_strict(raw)
        .map(|c| Some(uwuwu_usecase::complexity_name(c).to_string()))
        .map_err(|error| format!("complexity: {error}"))
}

/// Значение строкового аргумента: отсутствующий или не-строковый — пустая строка.
fn arg_str(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Громкий отказ, если хоть один из перечисленных строковых аргументов пуст.
fn require_args(args: &Value, keys: &[&str]) -> Result<(), String> {
    let all_filled = keys.iter().all(|k| !arg_str(args, k).is_empty());
    if all_filled {
        return Ok(());
    }
    let verb = if keys.len() == 1 { "is" } else { "are" };
    Err(format!("{} {} required", keys.join(" and "), verb))
}

/// message из аргументов или дефолт (например, «append: слаг»).
fn message_or(args: &Value, fallback: String) -> String {
    let given = arg_str(args, "message");
    if given.is_empty() {
        fallback
    } else {
        given
    }
}

/// Единое событие деградации слаг-гарда (#1697): имя стабильно, причина —
/// поле. Ошибка связывается, а не глушится.
fn log_slug_guard_degraded(tool: &str, slug: &str, reason: &str, error: &impl std::fmt::Display) {
    tracing::warn!(
        event = "slug_guard.degraded",
        tool,
        slug,
        reason,
        error = %error,
        "collision checks skipped"
    );
}

/// Приписка квитанции: причина деградации — та, что действительно сработала.
fn degraded_clause(degraded: Option<&'static str>) -> String {
    match degraded {
        Some(reason) => format!(" — slug-guard degraded: collision checks skipped ({reason})"),
        None => String::new(),
    }
}

/// Слаг-гард (#1613): слаг, вырожденный после санитайза, — отказ входа.
fn empty_slug_error() -> String {
    "slug is required (nothing left after sanitization)".into()
}

/// Кандидаты суффикса для append-отказа: дерево best-effort — сбой
/// деградирует отказ к форме без кандидатов, не к записи.
async fn suffix_candidates_best_effort(
    wiki: &dyn uwuwu_usecase::WikiRepo,
    slug: &str,
) -> Vec<String> {
    match wiki.list_docs("").await {
        Ok(live) => slug_guard::suffix_candidates(slug, &live),
        Err(e) => {
            log_slug_guard_degraded("docs_append", slug, "tree unavailable", &e);
            Vec::new()
        }
    }
}

/// Отказ append-nonexistent (#1613, развилка Б): имя ветки + строка-
/// лекарство (детерминированный мост: создание — только docs_write)
/// + кандидаты суффикса, если нашлись.
fn append_missing_error(slug: &str, candidates: &[String]) -> String {
    let mut out = format!(
        "refused: append-nonexistent — doc «{slug}» does not exist; create it via docs_write"
    );
    if !candidates.is_empty() {
        out.push_str(&format!("; suffix candidates: {}", candidates.join(", ")));
    }
    out
}

/// Отказ root-prefix (#1613): имя ветки + факт ветки (для одно-сегментного
/// таргета — файл в корне вики, корень держит только папки; для
/// много-сегментного — вне канонического набора) + кандидаты суффикса.
fn root_prefix_error(slug: &str, candidates: &[String]) -> String {
    let roots = slug_guard::ROOT_FOLDERS.join(", ");
    let fact = if slug.contains('/') {
        format!("refused: root-prefix — «{slug}» is outside the wiki root ({roots})")
    } else {
        format!(
            "refused: root-prefix — «{slug}» would be a file in the wiki root; the root holds folders only ({roots})"
        )
    };
    if candidates.is_empty() {
        fact
    } else {
        format!("{fact}; suffix candidates: {}", candidates.join(", "))
    }
}

/// Отказ parent-collision (#1613): живой файл без детей на пути таргета.
fn parent_collision_error(parent: &str) -> String {
    format!(
        "refused: parent-collision — «{parent}» is a live doc (file, not a folder); pick another slug"
    )
}

/// Отказ suffix-collision (#1613): живой док с совпавшим путём-хвостом.
fn suffix_collision_error(slug: &str, candidates: &[String]) -> String {
    format!(
        "refused: suffix-collision — «{slug}» collides with live docs: {}; write with the full slug or a distinct name",
        candidates.join(", ")
    )
}

/// Квитанция удаления (#1658): ветка, бэкап, пост-проверка, тёзки.
fn delete_receipt_text(receipt: &DeleteReceipt, degraded: Option<&'static str>) -> String {
    let mut out = format!(
        "deleted: «{}» (path {}, blob sha {}, commit {})",
        receipt.slug, receipt.path, receipt.old_blob_sha, receipt.commit_sha
    );
    out.push_str(&format!(
        "\nbackup: {} (presigned GET: {}, TTL 7 days)",
        receipt.backup_key, receipt.backup_url
    ));
    let check = match &receipt.post_check {
        PostCheck::ConfirmedAbsent => format!("absent in tree at {}", receipt.commit_sha),
        PostCheck::Disagreement(reason) => {
            format!("⚠ disagreement — {reason}; verify before retry")
        }
        PostCheck::Unverified(reason) => format!("⚠ unverified — {reason}; verify before retry"),
    };
    out.push_str(&format!("\npost-check: {check}"));
    if !receipt.suffix_twins.is_empty() {
        out.push_str(&format!(
            "\nsuffix twins: {}",
            receipt.suffix_twins.join(", ")
        ));
    }
    if let Some(reason) = degraded {
        out.push_str(&format!(
            "\nslug-guard degraded: collision checks skipped ({reason})"
        ));
    }
    out
}

/// Отсутствующий док: честная ветка `absent`, без мутации и бэкапа.
fn delete_absent_error(slug: &str, candidates: &[String]) -> String {
    let mut out = format!("absent: doc «{slug}» does not exist (no backup, no mutation)");
    if !candidates.is_empty() {
        out.push_str(&format!("; suffix candidates: {}", candidates.join(", ")));
    }
    out
}

/// Отказы удаления: имя ветки и причина, без повторной мутации.
fn delete_refusal_error(error: &UsecaseError) -> String {
    match error {
        UsecaseError::DocReadFailed(reason) => format!("refused: read-failed — {reason}"),
        UsecaseError::BackupFailed(reason) => format!("refused: backup-failed — {reason}"),
        UsecaseError::RaceRef(reason) => format!(
            "refused: race-ref — {reason}; the delete was NOT retried, verify before repeating; any backup is for the read version only"
        ),
        UsecaseError::DocAbsentAtRef(reason) => format!("absent-at-ref: {reason}"),
        UsecaseError::PrecheckUnavailable(reason) => format!(
            "refused: precheck-unavailable — {reason}; no delete was committed; the backup, if any, is for the read version only"
        ),
        UsecaseError::DocAbsent(reason) => {
            format!("absent: doc «{reason}» does not exist (no backup, no mutation)")
        }
        other => other.to_string(),
    }
}

fn str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn need_number(args: &Value) -> Result<u64, String> {
    args.get("number")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .ok_or("number is required (an integer)".to_string())
}

/// Опциональный number: отсутствует/null → None, мусор — громкий отказ.
fn optional_number(args: &Value) -> Result<Option<u64>, String> {
    match args.get("number") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            .map(Some)
            .ok_or("number must be an integer".to_string()),
    }
}

/// Список номеров из аргумента: массив integers или csv-строка «1,2,3».
/// Пустой/отсутствующий — Err (ids обязателен); ack — unwrap_or_default
/// на стороне вызова. null и мусорный токен — громкий отказ (не тихий
/// пропуск: батч не сужается молча, ревью #1158 Low-1).
fn arg_numbers(args: &Value, key: &str) -> Result<Vec<u64>, String> {
    let raw: Vec<Value> = match args.get(key) {
        None | Some(Value::Null) => return Err(format!("{key} is required (row numbers)")),
        Some(Value::Array(items)) => items.clone(),
        Some(Value::String(csv)) => csv
            .split(',')
            .map(|tok| Value::String(tok.trim().to_string()))
            .collect(),
        Some(other) => {
            return Err(format!(
                "{key}: an array of integers or a csv string «1,2,3», got: {other}"
            ))
        }
    };
    raw.iter()
        .map(|v| {
            v.as_u64()
                .or_else(|| v.as_str().map(str::trim).and_then(|s| s.parse().ok()))
                .ok_or(format!(
                    "{key}: garbage token «{v}» — expecting a row number"
                ))
        })
        .collect()
}

/// Квитанция секционной правки для агента: исход + длины было/стало по
/// телу и по секции (#1409: сверка без второго GET) + новое тело
/// (хэш/размер). Компактно — полное тело не возвращается (сознательное
/// сужение #1152: возврат больших тел убивал бы экономию секционного пути).
fn render_section_report(section: &str, report: &uwuwu_usecase::SectionEditReport) -> String {
    use uwuwu_usecase::SectionAction;
    let mut verdict = match (report.action, report.cleared) {
        (SectionAction::Replaced, true) => format!("section «{section}» cleared"),
        (SectionAction::Replaced, false) => format!("section «{section}» replaced"),
        (SectionAction::Appended, _) => format!("section «{section}» appended at the end"),
    };
    if report.duplicates_removed > 0 {
        verdict.push_str(&format!(
            " (×{} duplicates removed)",
            report.duplicates_removed
        ));
    }
    let mut out = format!(
        "{verdict}; body {} bytes (was {}); section {} bytes (was {}); body_hash: {}{}",
        report.body_size,
        report.body_was,
        report.section_now,
        report.section_was,
        report.body_hash,
        if report.verified {
            String::new()
        } else {
            delivery_unverified_marker().to_string()
        }
    );
    if report.confirmed {
        out.push_str("; shrink passed with confirm: true");
    }
    out
}

/// Маркер деградированной квитанции (#1331): контрольное чтение носителя
/// не состоялось — hash/размер от отправленного, ретрай мутации запрещён.
/// Единая точка текста для task_body и секционного отчёта.
fn delivery_unverified_marker() -> &'static str {
    "; ⚠ delivery not confirmed by a control read (carrier not read) — \
     retrying the mutation is forbidden, verify via task_get"
}

fn ok_text(text: String) -> Result<Value, String> {
    Ok(json!([{ "type": "text", "text": text }]))
}

/// null = «нет значения» в JSON-rows task_list (#1604): Empty-варианты
/// статуса и приоритета — null, а не пустая строка/мусорное имя
/// (канон native-null хвоста #1271; поля присутствуют всегда).
/// Имена — из словаря MCP-поверхности (naming), не из Debug-формы:
/// переименование варианта энума не меняет wire-значение.
fn status_cell(status: uwuwu_domain::Status) -> Value {
    match status {
        uwuwu_domain::Status::Empty => Value::Null,
        known => json!(uwuwu_usecase::status_name(known)),
    }
}

fn priority_cell(priority: uwuwu_domain::Priority) -> Value {
    match priority {
        uwuwu_domain::Priority::Empty => Value::Null,
        known => json!(uwuwu_usecase::priority_name(known)),
    }
}

/// Метка коммента в выводе task_get (#1432): created_ms → ISO 8601 UTC
/// (формат-контракт для тайминг-разборов агентов). ms вне часов chrono
/// (в т.ч. > i64::MAX — до приведения типов) — «—» по конвенции пустых ячеек.
fn iso_utc_stamp(ms: u64) -> String {
    if ms > i64::MAX as u64 {
        return "—".into();
    }
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| "—".into())
}

/// Dry-run чистки: вердикты и подсказки, ноль мутаций.
fn render_clean_plan(reports: &[uwuwu_usecase::CleanRowReport]) -> String {
    use uwuwu_usecase::CleanVerdict;
    let mut out = String::from("cleanup dry-run (no mutations):\n");
    for r in reports {
        match r.verdict {
            CleanVerdict::NoRow => out.push_str(&format!(
                "  #{:>6}  NoRow          — no such row\n",
                r.number
            )),
            CleanVerdict::Divergent => out.push_str(&format!(
                "  #{:>6}  Divergent      — {}; batch not executed\n",
                r.number,
                r.note.as_deref().unwrap_or("probe did not complete")
            )),
            CleanVerdict::Live => out.push_str(&format!(
                "  #{:>6}  Live           «{}» — protected (non-Draft: owner's ack required)\n",
                r.number, r.title
            )),
            CleanVerdict::Dead => out.push_str(&format!(
                "  #{:>6}  Dead           «{}» — terminal (delete with owner's ack)\n",
                r.number, r.title
            )),
            CleanVerdict::DraftSkeleton => out.push_str(&format!(
                "  #{:>6}  DraftSkeleton  «{}» — empty stub, delete with confirm\n",
                r.number, r.title
            )),
        }
    }
    let live_or_dead: Vec<String> = reports
        .iter()
        .filter(|r| matches!(r.verdict, CleanVerdict::Live | CleanVerdict::Dead))
        .map(|r| r.number.to_string())
        .collect();
    if !live_or_dead.is_empty() {
        out.push_str(&format!(
            "\nnon-Draft rows ({}): owner's confirmation — numbers into ack (confirm=true executes the deletion)\n",
            live_or_dead.join(", ")
        ));
    } else if reports
        .iter()
        .any(|r| r.verdict == CleanVerdict::DraftSkeleton)
    {
        out.push_str(
            "\nDraft stubs: confirm=true will delete them (the undo journal is written automatically)\n",
        );
    }
    out
}

/// Отчёт исполнения сноса.
fn render_clean_exec(project: &str, report: &uwuwu_usecase::CleanExecReport) -> String {
    let mut out = String::new();
    if report.deleted.is_empty() {
        out.push_str("nothing to delete (no journal written):\n");
    } else {
        out.push_str(&format!(
            "deleted {} rows; undo journal: {} (rollback — task_undo {{project: \"{project}\", journal: \"{}\"}})\n",
            report.deleted.len(),
            report.journal_id,
            report.journal_id
        ));
    }
    for r in &report.deleted {
        out.push_str(&format!(
            "  #{:>6}  {}  «{}» — deleted (item {})\n",
            r.number,
            r.verdict.label(),
            r.title,
            r.item_id
        ));
    }
    for r in &report.skipped {
        out.push_str(&format!(
            "  #{:>6}  {}  — skipped (no such row)\n",
            r.number,
            r.verdict.label()
        ));
    }
    out
}

/// Отчёт undo: карта «строка → исход», идемпотентные skip'и видны.
fn render_undo(report: &uwuwu_usecase::UndoReport) -> String {
    use uwuwu_usecase::UndoOutcome;
    let mut out = format!("undo of journal {}:\n", report.journal_id);
    for (number, title, outcome) in &report.rows {
        match outcome {
            UndoOutcome::Restored(item_id) => out.push_str(&format!(
                "  #{number:>6}  «{title}» — restored (item {item_id})\n"
            )),
            UndoOutcome::AlreadyOnBoard => out.push_str(&format!(
                "  #{number:>6}  «{title}» — already on the board (skip)\n"
            )),
            UndoOutcome::Failed(reason) => {
                out.push_str(&format!("  #{number:>6}  «{title}» — FAILED: {reason}\n"))
            }
        }
    }
    out
}

/// Отчёт rename статуса (#1447): английский (#1383) — строки тула ходят
/// в каждую сессию агентам.
fn render_status_rename(report: &uwuwu_usecase::RenameReport) -> String {
    let mut out = format!(
        "Renamed \"{}\" → \"{}\" on board \"{}\" (option id kept: {})\n\
         Options: {}; rows seen: {}\n\
         Histogram before: {}\n\
         Histogram after: {}\n\
         Pre-image journal: {}",
        report.from,
        report.to,
        report.board,
        report.option_id,
        report.options_total,
        report.rows_total,
        fmt_histogram(&report.histogram_before),
        fmt_histogram(&report.histogram_after),
        report.journal_path,
    );
    for warning in &report.warnings {
        out.push_str(&format!("\nWARNING: {warning}"));
    }
    out
}

/// «Draft=3, In Progress=12» — детерминированный порядок BTreeMap.
fn fmt_histogram(histogram: &std::collections::BTreeMap<String, usize>) -> String {
    histogram
        .iter()
        .map(|(name, count)| format!("{name}={count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": input_schema })
}

fn object(props: Value) -> Value {
    json!({ "type": "object", "properties": props, "required": [] })
}

fn string(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}

fn integer(desc: &str) -> Value {
    json!({ "type": "integer", "description": desc })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_and_priority_cells_map_empty_to_null() {
        assert_eq!(status_cell(uwuwu_domain::Status::Empty), Value::Null);
        assert_eq!(
            status_cell(uwuwu_domain::Status::NeedsAttention),
            json!("Needs attention")
        );
        assert_eq!(priority_cell(uwuwu_domain::Priority::Empty), Value::Null);
        assert_eq!(priority_cell(uwuwu_domain::Priority::High), json!("high"));
        assert_eq!(
            priority_cell(uwuwu_domain::Priority::Medium),
            json!("medium")
        );
    }

    #[test]
    fn iso_utc_stamp_renders_iso8601_utc() {
        // Типичный created_ms: ровно YYYY-MM-DDTHH:MM:SSZ (контракт
        // тайминг-разборов дримера, #1432).
        assert_eq!(iso_utc_stamp(1_795_171_645_570), "2026-11-20T10:47:25Z");
        // Нижняя граница эпохи — валидная метка, не «—».
        assert_eq!(iso_utc_stamp(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn iso_utc_stamp_degrades_to_dash_outside_chrono_hours() {
        // Гард стоит ДО приведения типов: значения > i64::MAX не
        // заворачиваются в отрицательные (валидно-выглядящая 1969-метка),
        // а честно дают пустую ячейку. Чувствительное значение — верхний
        // край u64: без гарда `u64::MAX as i64 = -1` → 1969-12-31T23:59:59Z.
        assert_eq!(iso_utc_stamp(u64::MAX), "—");
        assert_eq!(iso_utc_stamp(i64::MAX as u64 + 1), "—");
        // i64::MAX мс — вне часов chrono (from_timestamp_millis → None).
        assert_eq!(iso_utc_stamp(i64::MAX as u64), "—");
    }

    #[test]
    fn descriptors_are_the_real_surface() {
        let list = tool_descriptors();
        let names: Vec<&str> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for expected in [
            "task_list",
            "task_get",
            "task_claim",
            "task_create",
            "task_edit",
            "task_comment",
            "task_close",
            "task_clean",
            "task_undo",
            "board_status_rename",
            "task_body",
            "task_body_section",
            "blob_put",
            "blob_get",
            "docs_ls",
            "docs_read",
            "docs_write",
            "docs_append",
            "docs_delete",
            "person_show",
            "goal_list",
            "projects",
            "search",
            "cron_list",
            "session_export",
        ] {
            assert!(names.contains(&expected), "нет инструмента {expected}");
        }
        assert_eq!(names.len(), 25, "контракт поверхности: {names:?}");
    }

    /// Пин консистентности Common-2 (ревью #1572): allow-list гарда
    /// выводится из дескрипторов — единый носитель контракта. Тест ловит
    /// дрейф ключей («ключ в схеме, гард отвергает» и «чужой ключ гард
    /// пропускает») и требует от каждого мутационного тула дескриптора
    /// (тул из списка без дескриптора гардится громко, не молча); дрейф
    /// членства «мутирующие ∕ читающие» держит соседний пин
    /// catalog_partition_mutation_or_read_is_total.
    #[test]
    fn mutation_guard_keys_match_descriptors() {
        let list = tool_descriptors();
        for name in MUTATION_TOOLS {
            let tool = list
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"].as_str() == Some(name))
                .unwrap_or_else(|| panic!("мутационный тул {name} вне каталога дескрипторов"));
            let keys: Vec<&str> = tool["inputSchema"]["properties"]
                .as_object()
                .expect("inputSchema.properties")
                .keys()
                .map(String::as_str)
                .chain(
                    LEGACY_KEYS
                        .iter()
                        .filter(|(t, _)| *t == *name)
                        .map(|(_, k)| *k),
                )
                .collect();
            assert!(!keys.is_empty(), "{name}: дескриптор без ключей");
            // схема-ключи + диспетчерский project легальны
            let mut args = serde_json::Map::new();
            for k in &keys {
                args.insert((*k).to_string(), json!("x"));
            }
            args.insert("project".to_string(), json!("uwuwu"));
            assert!(
                refuse_unknown_args(name, &Value::Object(args.clone())).is_ok(),
                "{name}: схема-ключи + project легальны"
            );
            // мусорный ключ — громкий отказ с перечнем известных
            args.insert("__find__".to_string(), json!("x"));
            let err = refuse_unknown_args(name, &Value::Object(args))
                .expect_err("чужой ключ отвергается");
            assert!(err.contains("__find__"), "{name}: чужой ключ назван: {err}");
            // внесхемные легальные ключи не рекламируются в «the tool
            // knows» (ревью #1572 дельта-раунд 3): project — канал
            // диспетчера, legacy autodone — решение #1232
            for legacy in LEGACY_KEYS
                .iter()
                .filter(|(t, _)| *t == *name)
                .map(|(_, k)| *k)
            {
                assert!(
                    !err.contains(legacy),
                    "{name}: legacy-ключ {legacy} легален молча, в отказе его нет: {err}"
                );
            }
        }
        // диспетчерский project не рекламируется в «the tool knows» —
        // только ключи схемы (мёртвых ключей в отказе нет)
        let err = refuse_unknown_args("task_comment", &json!({ "find": "x" })).expect_err("отказ");
        assert!(
            !err.contains("project"),
            "project — канал диспетчера, не ключ схемы: {err}"
        );
        // project легален на любом мутационном туле, даже не объявленном
        assert!(refuse_unknown_args(
            "task_comment",
            &json!({ "number": 1, "text": "т", "project": "uwuwu" })
        )
        .is_ok());
        // fact_track create-only (#1631): ключ вне схемы task_edit —
        // чужой ключ отвергается гардом #1572 до мутации
        let err = refuse_unknown_args("task_edit", &json!({ "number": 1, "fact_track": true }))
            .expect_err("fact_track в task_edit — отказ");
        assert!(err.contains("fact_track"), "чужой ключ назван: {err}");
    }

    /// Пин разбиения поверхности (ревью #1572, Common): каждый тул
    /// каталога — либо мутационный (MUTATION_TOOLS), либо
    /// явно заявленный читающий (READ_TOOLS); третий статус красный.
    /// Направление дрейфа «новый мутационный тул с дескриптором, но без
    /// записи в списке» ловится: имя без принадлежности валит тест, а
    /// мёртвые имена в списках — тоже.
    #[test]
    fn catalog_partition_mutation_or_read_is_total() {
        let list = tool_descriptors();
        let names: Vec<&str> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 25, "контракт поверхности: {names:?}");
        for name in &names {
            let is_mutation = MUTATION_TOOLS.contains(name);
            let is_read = READ_TOOLS.contains(name);
            assert!(
                is_mutation ^ is_read,
                "тул {name} обязан быть заявлен ровно в одном списке (мутационные ∪ читающие)"
            );
        }
        for name in MUTATION_TOOLS.iter().chain(READ_TOOLS.iter()) {
            assert!(
                names.contains(name),
                "имя {name} из списков вне каталога дескрипторов"
            );
        }
    }

    #[test]
    fn task_create_type_schema_matches_validation_canon() {
        // Инвариант enum-паритета дескрипторов (#1298; #1271 добавил
        // task_edit.type): перечень типов в описании поля type (сплит по
        // '|', аннотация после "; " срезается) — полный матч с каноном
        // валидации: каждый токен парсится и ни один тип канона не потерян.
        // (tester #1383: пин восстановлен из master — ребейз унёс до-#1271
        // версию с одним task_create, и потеря task_edit.type в схеме
        // перестала ловиться.)
        let list = tool_descriptors();
        let schema_types = ["task_create", "task_edit"]
            .iter()
            .map(|tool_name| {
                let description = list
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|t| t["name"] == *tool_name)
                    .expect("тул в дескрипторах")["inputSchema"]["properties"]["type"]
                    ["description"]
                    .as_str()
                    .expect("description поля type");
                let enums = description.split_once("; ").map_or(description, |(e, _)| e);
                enums.split('|').map(str::trim).collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let mut canon_types: Vec<&str> = uwuwu_usecase::TASK_TYPES
            .iter()
            .map(|t| uwuwu_usecase::task_type_name(*t))
            .collect();
        canon_types.sort_unstable();
        for tokens in &schema_types {
            for token in tokens {
                assert!(
                    uwuwu_usecase::parse_task_type(token).is_some(),
                    "схема обещает тип, отвергаемый валидацией: «{token}»"
                );
            }
            let mut sorted = tokens.clone();
            sorted.sort_unstable();
            assert_eq!(
                sorted, canon_types,
                "перечень type дескриптора != канон валидации"
            );
        }
    }
}
