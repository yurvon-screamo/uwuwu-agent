//! Реестр MCP-инструментов: дескрипторы + диспетчер. Тонкие обёртки
//! юзкейсов (views) — никакой бизнес-логики в presentation.

use serde_json::{json, Value};
use std::sync::Arc;
use uwuwu_usecase::{DocsView, PersonView, TaskView, TaskWriteUseCase, TaskWriter};

pub struct ToolHub {
    pub tasks: Arc<TaskView>,
    pub docs: Arc<DocsView>,
    pub person: Arc<PersonView>,
    pub writer: Arc<TaskWriteUseCase>,
    pub wiki: Arc<dyn uwuwu_usecase::WikiRepo>,
    pub sessions: Arc<dyn uwuwu_usecase::AgentSessionGateway>,
    pub search: Arc<uwuwu_usecase::SearchUseCase>,
    pub crons: Arc<uwuwu_usecase::CronUseCase>,
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
            "Список .md-доков вики под префиксом (слаги без docs/ и .md)",
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
            "Реестр долгосрочных целей проекта — читай перед работой над задачей",
            object(json!({ "project": string("имя проекта; дефолт uwuwu") }))
        ),
        tool(
            "projects",
            "Проекты конвейера (доски в конфиге сервера)",
            object(json!({}))
        ),
        tool(
            "task_create",
            "Создать задачу на доске проекта (Draft; тайтл уникален)",
            object(json!({
                "title": string("тайтл (уникален на доске)"),
                "project": string("проект; дефолт uwuwu"),
                "body": string("markdown-тело (Постановка…)"),
                "type": string("bug|spike|analys|docs|other|feature|refactoring|legacy|idea|qa|infrastructure"),
                "priority": string("high|medium|low; дефолт medium"),
                "autodone": json!({"type":"boolean","description":"автодон: мерж PR на Review; дефолт true"})
            }))
        ),
        tool(
            "task_edit",
            "Правка ячеек задачи: статус/ветка/PR/приоритет/тайтл",
            object(json!({
                "number": integer("номер задачи"),
                "project": string("проект; дефолт uwuwu"),
                "status": string("статус канона: Draft/Todo/Design/Ревью плана/In Progress/Ревью результата/Тестирование/Review/Needs attention/Вейт"),
                "branch": string("имя ветки"),
                "pr": string("URL PR"),
                "priority": string("high|medium|low"),
                "title": string("новый тайтл")
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
            "Инвентарь кронов проекта (мастера в вики-папке Кроны: расписание + журнал-хвост)",
            object(json!({
                "project": string("проект; дефолт uwuwu")
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
            "cron_list" => self.cron_list(&project).await,
            "search" => self.search(args).await,
            "session_export" => self.session_export(args).await,
            "task_create" => self.task_create(&project, args).await,
            "task_edit" => self.task_edit(&project, args).await,
            "task_claim" => self.task_claim(args).await,
            "task_comment" => self.task_comment(args).await,
            "task_close" => self.task_close(args).await,
            "task_body" => self.task_body(args).await,
            "task_body_section" => self.task_body_section(&project, args).await,
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
                "{:>6}  {:<14} {:<7} {}\n",
                t.number(),
                uwuwu_usecase::status_name(t.status()),
                format!("{:?}", t.priority()).to_lowercase(),
                t.title()
            ));
        }
        ok_text(out)
    }

    async fn task_get(&self, project: &str, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        let view = self.tasks.card(project, number).await.map_err(view_err)?;
        let mut out = format!(
            "#{} {} — {}\n\n--- fm ---\nbranch: {}\npr: {}\nautodone: {}\nbody_hash: {}\n\n",
            view.task.number(),
            view.task.title(),
            uwuwu_usecase::status_name(view.task.status()),
            view.task.branch().map(|b| b.as_str()).unwrap_or("—"),
            view.task.pr().map(|u| u.as_str()).unwrap_or("—"),
            view.task.autodone(),
            uwuwu_usecase::body_hash(&view.body),
        );
        out.push_str(&view.body);
        if !view.comments.is_empty() {
            out.push_str("\n\n--- комменты (последние) ---\n");
            for c in &view.comments {
                out.push_str(&format!("---\n{}\n", c.body));
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
        // Реестр целей проекта: док Projects/<proj>/Goals (тело правит
        // владелец в UI; агент — читает перед работой над задачей)
        let slug = format!("Projects/{project}/Goals");
        match self.docs.read(&slug).await {
            Ok(md) => ok_text(md),
            Err(_) => ok_text(format!(
                "реестр целей пуст: док {slug} не заведён \
                 (цели ведёт владелец в UI)"
            )),
        }
    }

    async fn docs_append(&self, args: &Value) -> Result<Value, String> {
        require_args(args, &["slug", "line"])?;
        let slug = arg_str(args, "slug");
        let message = message_or(args, format!("append: {slug}"));
        self.wiki
            .append_doc(&slug, &arg_str(args, "line"), &message)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(format!("дописано в «{slug}»"))
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

    async fn cron_list(&self, project: &str) -> Result<Value, String> {
        // тонкая обёртка: инвентарь живёт в юзкейсе (masters())
        let masters = self
            .crons
            .masters(project)
            .await
            .map_err(|e| e.to_string())?;
        let out = masters
            .iter()
            .map(|m| {
                format!(
                    "{} — `{}`\n",
                    m.title,
                    uwuwu_usecase::schedule_label(&m.schedule)
                )
            })
            .collect::<String>();
        ok_text(if out.is_empty() {
            format!("кроны проекта {project} не найдены")
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
            .export_session(&session)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(export)
    }

    async fn task_create(&self, project: &str, args: &Value) -> Result<Value, String> {
        let raw_kind = arg_str(args, "type");
        if !raw_kind.is_empty() && uwuwu_usecase::parse_task_type(&raw_kind).is_none() {
            return Err(format!(
                "неизвестный тип «{raw_kind}»: bug|spike|analys|docs|feature|refactoring|legacy|idea|qa|infrastructure|other"
            ));
        }
        let spec = uwuwu_usecase::NewTask {
            title: arg_str(args, "title"),
            body_md: arg_str(args, "body"),
            kind: uwuwu_usecase::parse_task_type(&raw_kind),
            priority: arg_str(args, "priority"),
            autodone: args
                .get("autodone")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        };
        let n = self
            .writer
            .create(project, spec)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(format!(
            "создана задача #{n} (Draft) на доске проекта {project}"
        ))
    }

    async fn task_edit(&self, project: &str, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        let patch = uwuwu_usecase::TaskPatch {
            status: str_arg(args, "status").map(|s| uwuwu_usecase::parse_status(&s)),
            branch: str_arg(args, "branch"),
            pr: str_arg(args, "pr"),
            priority: str_arg(args, "priority"),
            title: str_arg(args, "title"),
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

    async fn task_body(&self, args: &Value) -> Result<Value, String> {
        let number = need_number(args)?;
        let body = arg_str(args, "body");
        if body.trim().is_empty() {
            return Err("пустое тело".into());
        }
        let receipt = self
            .writer
            .set_body(number, &body)
            .await
            .map_err(|e| e.to_string())?;
        ok_text(format!(
            "тело #{number} заменено; {} байт; body_hash: {}",
            receipt.body_size, receipt.body_hash
        ))
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
}

fn view_err(e: uwuwu_usecase::ViewError) -> String {
    e.to_string()
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
        "{verdict}; тело {} байт; body_hash: {}",
        report.body_size, report.body_hash
    )
}

fn ok_text(text: String) -> Result<Value, String> {
    Ok(json!([{ "type": "text", "text": text }]))
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
            "task_body",
            "task_body_section",
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
        assert_eq!(names.len(), 19, "контракт поверхности: {names:?}");
    }
}
