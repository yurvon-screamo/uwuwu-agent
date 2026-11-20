//! Реестр MCP-инструментов: дескрипторы + диспетчер. Тонкие обёртки
//! юзкейсов (views) — никакой бизнес-логики в presentation.

use base64::Engine as _;
use serde_json::{json, Value};
use std::sync::Arc;
use uwuwu_usecase::{
    BlobPlace, BlobUseCase, DocsView, PersonView, TaskView, TaskWriteUseCase, TaskWriter,
};

pub struct ToolHub {
    pub tasks: Arc<TaskView>,
    pub docs: Arc<DocsView>,
    pub person: Arc<PersonView>,
    pub writer: Arc<TaskWriteUseCase>,
    /// Батч-снос строк доски (#1158): guard-вердикты + write-ahead undo.
    pub clean: Arc<uwuwu_usecase::TaskCleanUseCase>,
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
            "Строки доски проекта: number, статус, приоритет, тайтл",
            object(json!({ "project": string("имя проекта; дефолт uwuwu") }))
        ),
        tool(
            "task_get",
            "Карточка задачи целиком: fm-поля, тело issue, последние комменты",
            object(json!({ "number": integer("номер задачи (row-id)"),
                         "project": string("проект; дефолт uwuwu") }))
        ),
        tool(
            "docs_ls",
            "Список .md-доков вики под префиксом (слаги без docs/ и .md); при file/dir-коллизии строка = «X (неоднозначно: файл … + папка …)» — слаг для docs_read до « (неоднозначно»",
            object(json!({ "path": string("префикс: проект/папка; пусто — вся вики") }))
        ),
        tool(
            "docs_read",
            "Markdown дока по слагу (без docs/ и .md)",
            object(json!({ "slug": string("слаг: Projects/uwuwu/README, Experience/...") }))
        ),
        tool(
            "person_show",
            "Персона владельца (язык/стиль/граны) — читают агенты в начале работы",
            object(json!({}))
        ),
        tool(
            "goal_list",
            "Реестр долгосрочных целей проекта: open milestones hub-репо (тайтл с префиксом [проект]) — читай перед работой над задачей; работа вразрез с открытой целью — флаг владельцу",
            object(json!({ "project": string("имя проекта; дефолт uwuwu") }))
        ),
        tool(
            "projects",
            "Проекты конвейера (доски в конфиге сервера)",
            object(json!({}))
        ),
        tool(
            "task_create",
            "Создать задачу на доске проекта (Analys; тайтл уникален; direct — сразу In Progress)",
            object(json!({
                "title": string("тайтл (уникален на доске)"),
                "project": string("проект; дефолт uwuwu"),
                "body": string("markdown-тело (Постановка…)"),
                "type": string(&type_enum_list()),
                "priority": string("high|medium|low; дефолт medium"),
                "automerge": json!({"type":"boolean","description":"автомерж: мерж PR при входе в Review, строка остаётся в Review; дефолт true (у direct — false)"}),
                "direct": json!({"type":"boolean","description":"direct-задача (#1357): строка сразу In Progress, агент сам финализирует в Review мимо фабричных стадий. Только по прямому поручению владельца — дословная цитата поручения в теле; дефолт false"})
            }))
        ),
        tool(
            "task_edit",
            "Правка ячеек задачи: статус/ветка/PR/приоритет/тайтл/тип",
            object(json!({
                "number": integer("номер задачи"),
                "project": string("проект; дефолт uwuwu"),
                "status": string("статус канона: Draft/Analys/Review Analys/Todo/Design/Ревью плана/In Progress/Ревью результата/Тестирование/Ревью тестирования/Review/Needs attention/Wait"),f368e51b (refactor: rename status 'Вейт' -> 'Wait' (keep 'Вейт' alias))
                "branch": string("имя ветки"),
                "pr": string("URL PR"),
                "priority": string("high|medium|low"),
                "title": string("новый тайтл"),
                "automerge": json!({"type":"boolean","description":"автомерж on/off (до-пометка старых строк)"}),
                "type": string(&(type_enum_list() + "; пусто — не трогать"))
            }))
        ),
        tool(
            "task_claim",
            "Заявить свою сессию в слот dev (фабрика не спавнит дубля)",
            object(json!({
                "number": integer("номер задачи"),
                "project": string("проект; дефолт uwuwu"),
                "session": string("свой session id (ses_…)")
            }))
        ),
        tool(
            "task_comment",
            "Коммент в задачу (вопрос/доклад владельцу; будит агентов)",
            object(json!({
                "number": integer("номер задачи"),
                "text": string("текст коммента"),
                "reply_to": string("цитата-якорь для ответа в тред (опционально)")
            }))
        ),
        tool(
            "task_close",
            "Закрыть задачу (Done; reason not_planned для отказов)",
            object(json!({
                "number": integer("номер задачи"),
                "project": string("проект; дефолт uwuwu"),
                "not_planned": json!({"type":"boolean","description":"true — отказ/дубль, false — выполнено"})
            }))
        ),
        tool(
            "task_clean",
            "Батч-снос строк доски с guard'ом (#1158): dry-run с вердиктами \
             (Live/Dead/DraftSkeleton/NoRow/Divergent) — дефолт, ноль мутаций; \
             confirm=true исполняет. Не-Draft строки (Live/Dead) требуют ack \
             владельца — без него весь батч отклоняется; write-ahead журнал \
             даёт undo (task_undo)",
            object(json!({
                "ids": json!({
                    "description": "номера строк: csv «1,2,3» или массив integers",
                    "oneOf": [
                        { "type": "string" },
                        { "type": "array", "items": { "type": "integer" } }
                    ]
                }),
                "project": string("проект; дефолт uwuwu"),
                "confirm": json!({"type":"boolean","description":"false/нет — dry-run (вердикты), true — снос"}),
                "ack": json!({
                    "description": "номера не-Draft строк (Live/Dead), подтверждённые \
                                    владельцем: csv или массив integers",
                    "oneOf": [
                        { "type": "string" },
                        { "type": "array", "items": { "type": "integer" } }
                    ]
                })
            }))
        ),
        tool(
            "task_undo",
            "Откат сноса строк по undo-журналу (идемпотентно): recreate item + \
             возврат статуса; без journal — свежий журнал проекта",
            object(json!({
                "project": string("проект; дефолт uwuwu"),
                "journal": string("id журнала из task_clean; опционально — свежий")
            }))
        ),
        tool(
            "docs_append",
            "Дописать строку в конец дока вики (журналы кронов, классы опыта; commit ff-only)",
            object(json!({
                "slug": string("слаг дока (без docs/ и .md)"),
                "line": string("строка целиком (markdown)"),
                "message": string("сообщение коммита; дефолт append")
            }))
        ),
        tool(
            "docs_write",
            "Перезаписать док вики целиком (commit ff-only; для новых статей опыта)",
            object(json!({
                "slug": string("слаг дока (без docs/ и .md)"),
                "content": string("markdown целиком"),
                "message": string("сообщение коммита; дефолт write")
            }))
        ),
        tool(
            "cron_list",
            "Инвентарь кронов (мастера /cron/<имя>/README.md: расписание + журнал-хвост)",
            object(json!({
                "project": string("фильтр по проекту (fm project мастера); без него — все кроны")
            }))
        ),
        tool(
            "search",
            "Гибридный поиск по вики: полнотекст + семантика (если индекс жив)",
            object(json!({
                "query": string("поисковый запрос (токены AND + вектор)"),
                "top": json!({"type":"integer","description":"сколько попаданий на секцию; дефолт 5"})
            }))
        ),
        tool(
            "session_export",
            "Экспорт сессии опенкода (контент для дримера/аудита)",
            object(json!({
                "session": string("id сессии (ses_…)")
            }))
        ),
        tool(
            "task_body",
            "Полная замена тела задачи целиком (fm в тело не пишется — поля \
             нативные; большое тело правь секционно через task_body_section)",
            object(json!({
                "number": integer("номер задачи"),
                "project": string("проект; дефолт uwuwu"),
                "body": string("markdown-тело целиком (без fm)")
            }))
        ),
        tool(
            "task_body_section",
            "Секционная правка тела задачи: дельта секции вместо полного body \
             (большие тела — только этот путь); свежесть — base_hash",
            object(json!({
                "number": integer("номер задачи"),
                "project": string("проект; дефолт uwuwu"),
                "section": string("заголовок секции; ATX-префикс пинит уровень \
                                  («### X» — h3), bare/«## X» — дефолт h2"),
                "content": string("новый контент секции без заголовка; пустая \
                                  строка — очистка секции"),
                "base_hash": string("body_hash тела на момент чтения (fm-блок \
                                    task_get или прошлая квитанция); обязателен")
            }))
        ),
        tool(
            "blob_put",
            "Положить файл в приватный blob-бакет (Tigris) — вложение к \
             задаче/доку: ключ + presigned-ссылка (TTL 7 суток; истёкшая \
             переподписывается blob_get по ключу)",
            object(json!({
                "filename": string("имя файла с расширением — content-type \
                                   берётся из него"),
                "data_base64": string("содержимое файла base64; лимит 20 MiB"),
                "number": integer("номер задачи: ключ blobs/<project>/<number>/…; \
                                  без него — сегмент misc"),
                "project": string("проект; дефолт uwuwu")
            }))
        ),
        tool(
            "blob_get",
            "Свежая presigned-ссылка на blob по ключу — переподпись истёкшей; \
             существование объекта не проверяется. Ключи: канон из blob_put \
             (blobs/…) или legacy assets/… — вложения, перенесённые из репо \
             hub в бакет (#1365)",
            object(json!({ "key": string("ключ из blob_put (blobs/…) или \
                             legacy-ключ перенесённого вложения (assets/<путь>)") }))
        ),
    ])
}

impl ToolHub {
    /// Диспетчер MCP-вызовов: разбор конверта + таблица «имя → хендлер».
    /// Логика каждого тула живёт в одноимённом хендлере ниже.
    pub async fn call(&self, msg: &Value) -> Result<Value, String> {
        let name = msg["params"]["name"].as_str().unwrap_or_default();
        let args = &msg["params"]["arguments"];
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
        let mut out = String::new();
        for t in rows {
            out.push_str(&format!(
                "{:>6}  {:<14} {:<7} {:<15} {}\n",
                t.number(),
                uwuwu_usecase::status_name(t.status()),
                format!("{:?}", t.priority()).to_lowercase(),
                t.kind().map(uwuwu_usecase::task_type_name).unwrap_or(""),
                t.title()
            ));
        }
        ok_text(out)
    }

    async fn task_get(&self, project: &str, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        let view = self.tasks.card(project, number).await.map_err(view_err)?;
        let mut out = format!(
            "#{} {} — {}\n\n--- fm ---\nbranch: {}\npr: {}\nautomerge: {}\ndirect: {}\ntype: {}\nbody_hash: {}\n\n",
            view.task.number(),
            view.task.title(),
            uwuwu_usecase::status_name(view.task.status()),
            view.task.branch().map(|b| b.as_str()).unwrap_or("—"),
            view.task.pr().map(|u| u.as_str()).unwrap_or("—"),
            view.task.automerge(),
            view.task.direct(),
            // канонное имя (словарь MCP-поверхности); None — «—» как
            // branch/pr (native-null хвост, дефолт Task; #1271)
            view.task
                .kind()
                .map(uwuwu_usecase::task_type_name)
                .unwrap_or("—"),
            uwuwu_usecase::body_hash(&view.body),
        );
        out.push_str(&view.body);
        if !view.comments.is_empty() {
            out.push_str("\n\n--- комменты (последние) ---\n");
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
        let slug = arg_str(args, "slug");
        let message = message_or(args, format!("append: {slug}"));
        let outcome = self
            .wiki
            .append_doc(&slug, &arg_str(args, "line"), &message)
            .await
            .map_err(|e| e.to_string())?;
        // честность ответа (#1353): append к существующему vs создание —
        // разрушительный класс бага видно по ответу, а не только коммит-статом
        let fact = if outcome.created {
            format!("создан «{slug}»: файла не было, записана строка")
        } else {
            format!(
                "дописано в «{slug}» — append к существующему, строк в доке: {}",
                outcome.lines
            )
        };
        ok_text(fact)
    }

    async fn docs_write(&self, args: &Value) -> Result<Value, String> {
        require_args(args, &["slug", "content"])?;
        let slug = arg_str(args, "slug");
        let message = message_or(args, format!("write: {slug}"));
        self.wiki
            .write_doc(&slug, &arg_str(args, "content"), &message)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(format!("док «{slug}» записан"))
    }

    async fn cron_list(&self, args: &Value) -> Result<Value, String> {
        // Инвентарь /cron (#1214): project — опциональный фильтр по fm,
        // без него — все кроны
        let filter = arg_str(args, "project");
        let filter = (!filter.is_empty()).then_some(filter.as_str());
        let masters = self
            .crons
            .masters(filter)
            .await
            .map_err(|e| e.to_string())?;
        let out = masters
            .iter()
            .map(|m| {
                format!(
                    "{} [{}] — `{}`\n",
                    m.title,
                    m.project,
                    uwuwu_usecase::schedule_label(&m.schedule)
                )
            })
            .collect::<String>();
        ok_text(if out.is_empty() {
            match filter {
                Some(p) => format!("кроны проекта {p} не найдены"),
                None => "кроны не найдены".to_string(),
            }
        } else {
            out
        })
    }

    async fn search(&self, args: &Value) -> Result<Value, String> {
        let query = arg_str(args, "query");
        if query.is_empty() {
            return Err("query обязателен".into());
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
            return Err("session обязан быть ses_…".into());
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
                    "automerge: жду boolean, пришло {v} — on-мандат не интерпретируется молча"
                ));
            }
            _ => !direct,
        };
        let spec = uwuwu_usecase::NewTask {
            title: arg_str(args, "title"),
            body_md: arg_str(args, "body"),
            kind,
            priority: arg_str(args, "priority"),
            automerge,
            direct,
        };
        let n = self
            .writer
            .create(project, spec)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(if direct {
            format!("создана direct-задача #{n} (In Progress) на доске проекта {project}")
        } else {
            format!("создана задача #{n} (Analys) на доске проекта {project}")
        })
    }

    async fn task_edit(&self, project: &str, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        let kind = parse_type_arg(&arg_str(args, "type"))?;
        let patch = uwuwu_usecase::TaskPatch {
            status: str_arg(args, "status").map(|s| uwuwu_usecase::parse_status(&s)),
            branch: str_arg(args, "branch"),
            pr: str_arg(args, "pr"),
            priority: str_arg(args, "priority"),
            title: str_arg(args, "title"),
            automerge: args.get("automerge").and_then(|v| v.as_bool()),
            kind,
        };
        if patch.is_empty() {
            return Err("пустой патч: укажи хотя бы одно поле".into());
        }
        self.writer
            .edit(project, number, patch)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(format!("задача #{number} обновлена"))
    }

    async fn task_claim(&self, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        let session = arg_str(args, "session");
        // валидация — в домене (SessionRef): мусорный id не заявка
        if uwuwu_domain::SessionRef::parse(&session).is_none() {
            return Err("session обязан быть ses_… (валидный id опенкода)".into());
        }
        self.writer
            .claim(number, &session, None)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(format!("задача #{number}: дев-слот заявлен за {session}"))
    }

    async fn task_comment(&self, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        let text = arg_str(args, "text");
        let reply = str_arg(args, "reply_to");
        self.writer
            .comment(number, &text, reply.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        ok_text(format!("коммент отправлен в #{number}"))
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
        ok_text(format!("задача #{number} закрыта"))
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

    async fn task_body(&self, project: &str, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        let body = arg_str(args, "body");
        if body.trim().is_empty() {
            return Err("пустое тело".into());
        }
        let receipt = self
            .writer
            .set_body(project, number, &body)
            .await
            .map_err(|e| e.to_string())?;
        let mut msg = format!(
            "тело #{number} заменено; {} байт; body_hash: {}",
            receipt.body_size, receipt.body_hash
        );
        if !receipt.verified {
            msg.push_str(delivery_unverified_marker());
        }
        ok_text(msg)
    }

    async fn task_body_section(&self, project: &str, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        let section = arg_str(args, "section");
        let content = arg_str(args, "content");
        let base_hash = arg_str(args, "base_hash");
        if section.trim().is_empty() {
            return Err("section обязателен (заголовок секции)".into());
        }
        if base_hash.trim().is_empty() {
            return Err("base_hash обязателен (body_hash из fm task_get)".into());
        }
        let report = self
            .writer
            .set_body_section(project, number, &section, &content, &base_hash)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(render_section_report(&section, &report))
    }

    async fn blob_put(&self, project: &str, args: &Value) -> Result<Value, String> {
        require_args(args, &["filename", "data_base64"])?;
        let filename = arg_str(args, "filename");
        let data_base64 = arg_str(args, "data_base64");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_base64.trim())
            .map_err(|e| format!("data_base64 не декодируется: {e}"))?;
        let number = optional_number(args)?;
        let target = BlobPlace {
            project: project.to_string(),
            task: number,
            filename,
        };
        let receipt = self
            .blobs
            .place(&target, &bytes)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(format!(
            "ключ: {}\nссылка (TTL 7 суток): {}\n{}; {} байт",
            receipt.key, receipt.url, receipt.content_type, receipt.size
        ))
    }

    async fn blob_get(&self, args: &Value) -> Result<Value, String> {
        let raw = arg_str(args, "key");
        if raw.trim().is_empty() {
            return Err("key обязателен".into());
        }
        let key = uwuwu_domain::BlobKey::parse_read(raw.trim()).ok_or(
            "ключ должен иметь форму blobs/<project>/<task|misc>/<sha8>-<имя> \
             или assets/<путь перенесённого вложения>",
        )?;
        let url = self.blobs.fresh_url(&key).map_err(|e| e.to_string())?;
        ok_text(format!(
            "ссылка (TTL 7 суток): {url}\n(существование объекта не проверяется — \
             подпись локальна; замену ссылки в носителе делай текстовыми тулами)"
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
        .ok_or_else(|| format!("неизвестный тип «{raw}»: {}", type_enum_list()))
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
    Err(format!("{} обязательны", keys.join(" и ")))
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
        .ok_or("number обязателен (число)".to_string())
}

/// Опциональный number: отсутствует/null → None, мусор — громкий отказ.
fn optional_number(args: &Value) -> Result<Option<u64>, String> {
    match args.get("number") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            .map(Some)
            .ok_or("number обязан быть числом".to_string()),
    }
}

/// Список номеров из аргумента: массив integers или csv-строка «1,2,3».
/// Пустой/отсутствующий — Err (ids обязателен); ack — unwrap_or_default
/// на стороне вызова. null и мусорный токен — громкий отказ (не тихий
/// пропуск: батч не сужается молча, ревью #1158 Low-1).
fn arg_numbers(args: &Value, key: &str) -> Result<Vec<u64>, String> {
    let raw: Vec<Value> = match args.get(key) {
        None | Some(Value::Null) => return Err(format!("{key} обязателен (номера строк)")),
        Some(Value::Array(items)) => items.clone(),
        Some(Value::String(csv)) => csv
            .split(',')
            .map(|tok| Value::String(tok.trim().to_string()))
            .collect(),
        Some(other) => {
            return Err(format!(
                "{key}: массив integers или csv-строка «1,2,3», получено: {other}"
            ))
        }
    };
    raw.iter()
        .map(|v| {
            v.as_u64()
                .or_else(|| v.as_str().map(str::trim).and_then(|s| s.parse().ok()))
                .ok_or(format!("{key}: мусорный токен «{v}» — жду номер строки"))
        })
        .collect()
}

/// Квитанция секционной правки для агента: исход + новое тело (хэш/размер).
/// Компактно — полное тело не возвращается (сознательное сужение #1152:
/// возврат больших тел убивал бы экономию секционного пути).
fn render_section_report(section: &str, report: &uwuwu_usecase::SectionEditReport) -> String {
    use uwuwu_usecase::SectionAction;
    let mut verdict = match (report.action, report.cleared) {
        (SectionAction::Replaced, true) => format!("секция «{section}» очищена"),
        (SectionAction::Replaced, false) => format!("секция «{section}» заменена"),
        (SectionAction::Appended, _) => format!("секция «{section}» добавлена в конец"),
    };
    if report.duplicates_removed > 0 {
        verdict.push_str(&format!(" (дублей снят ×{})", report.duplicates_removed));
    }
    format!(
        "{verdict}; тело {} байт; body_hash: {}{}",
        report.body_size,
        report.body_hash,
        if report.verified {
            String::new()
        } else {
            delivery_unverified_marker().to_string()
        }
    )
}

/// Маркер деградированной квитанции (#1331): контрольное чтение носителя
/// не состоялось — hash/размер от отправленного, ретрай мутации запрещён.
/// Единая точка текста для task_body и секционного отчёта.
fn delivery_unverified_marker() -> &'static str {
    "; ⚠ доставка не подтверждена контрольным чтением (носитель не прочитан) — \
     ретрай мутации запрещён, сверь task_get"
}

fn ok_text(text: String) -> Result<Value, String> {
    Ok(json!([{ "type": "text", "text": text }]))
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
    let mut out = String::from("dry-run чистки (мутаций нет):\n");
    for r in reports {
        match r.verdict {
            CleanVerdict::NoRow => out.push_str(&format!(
                "  #{:>6}  NoRow          — строки нет\n",
                r.number
            )),
            CleanVerdict::Divergent => out.push_str(&format!(
                "  #{:>6}  Divergent      — {}; батч не исполняется\n",
                r.number,
                r.note.as_deref().unwrap_or("проба не состоялась")
            )),
            CleanVerdict::Live => out.push_str(&format!(
                "  #{:>6}  Live           «{}» — защищена (не-Draft: нужен ack владельца)\n",
                r.number, r.title
            )),
            CleanVerdict::Dead => out.push_str(&format!(
                "  #{:>6}  Dead           «{}» — терминальная (снос с ack владельца)\n",
                r.number, r.title
            )),
            CleanVerdict::DraftSkeleton => out.push_str(&format!(
                "  #{:>6}  DraftSkeleton  «{}» — пустышка, снос с confirm\n",
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
            "\nне-Draft строки ({}): подтверждение владельца — номерами в ack (confirm=true исполняет снос)\n",
            live_or_dead.join(", ")
        ));
    } else if reports
        .iter()
        .any(|r| r.verdict == CleanVerdict::DraftSkeleton)
    {
        out.push_str(
            "\nDraft-пустышки: confirm=true снесёт их (журнал undo пишется автоматически)\n",
        );
    }
    out
}

/// Отчёт исполнения сноса.
fn render_clean_exec(project: &str, report: &uwuwu_usecase::CleanExecReport) -> String {
    let mut out = String::new();
    if report.deleted.is_empty() {
        out.push_str("сносить нечего (журнал не пишется):\n");
    } else {
        out.push_str(&format!(
            "снесено {} строк; журнал undo: {} (откат — task_undo {{project: \"{project}\", journal: \"{}\"}})\n",
            report.deleted.len(),
            report.journal_id,
            report.journal_id
        ));
    }
    for r in &report.deleted {
        out.push_str(&format!(
            "  #{:>6}  {}  «{}» — снесена (item {})\n",
            r.number,
            r.verdict.label(),
            r.title,
            r.item_id
        ));
    }
    for r in &report.skipped {
        out.push_str(&format!(
            "  #{:>6}  {}  — пропущена (строки нет)\n",
            r.number,
            r.verdict.label()
        ));
    }
    out
}

/// Отчёт undo: карта «строка → исход», идемпотентные skip'и видны.
fn render_undo(report: &uwuwu_usecase::UndoReport) -> String {
    use uwuwu_usecase::UndoOutcome;
    let mut out = format!("undo журнала {}:\n", report.journal_id);
    for (number, title, outcome) in &report.rows {
        match outcome {
            UndoOutcome::Restored(item_id) => out.push_str(&format!(
                "  #{number:>6}  «{title}» — восстановлена (item {item_id})\n"
            )),
            UndoOutcome::AlreadyOnBoard => out.push_str(&format!(
                "  #{number:>6}  «{title}» — уже на доске (skip)\n"
            )),
            UndoOutcome::Failed(reason) => out.push_str(&format!(
                "  #{number:>6}  «{title}» — НЕ удалось: {reason}\n"
            )),
        }
    }
    out
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
            "task_body",
            "task_body_section",
            "blob_put",
            "blob_get",
            "docs_ls",
            "docs_read",
            "docs_write",
            "docs_append",
            "person_show",
            "goal_list",
            "projects",
            "search",
            "cron_list",
            "session_export",
        ] {
            assert!(names.contains(&expected), "нет инструмента {expected}");
        }
        assert_eq!(names.len(), 23, "контракт поверхности: {names:?}");
    }

    #[test]
    fn task_create_type_schema_matches_validation_canon() {
        // Инвариант enum-паритета дескрипторов (#1298; #1271 добавил
        // task_edit.type): перечень типов в описании поля type (сплит по
        // '|', аннотация после "; " срезается) — полный матч с каноном
        // валидации: каждый токен парсится и ни один тип канона не потерян.
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
