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
    /// Роутер «проект → executor»: session_export веером по исполнителям.
    pub sessions: Arc<uwuwu_usecase::SessionRouter>,
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
            "Создать задачу на доске проекта (Analys; тайтл уникален)",
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
                "status": string("статус канона: Draft/Analys/Review Analys/Todo/Design/Ревью плана/In Progress/Ревью результата/Тестирование/Review/Needs attention/Вейт"),
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
    pub async fn call(&self, msg: &Value) -> Result<Value, String> {
        let name = msg["params"]["name"].as_str().unwrap_or_default();
        let args = &msg["params"]["arguments"];
        let arg = |k: &str| -> String {
            args.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let project = {
            let p = arg("project");
            if p.is_empty() {
                DEFAULT_PROJECT.to_string()
            } else {
                p
            }
        };
        match name {
            "projects" => ok_text(self.tasks.projects().join("\n")),
            "task_list" => {
                let rows = self.tasks.list(&project).await.map_err(view_err)?;
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
            "task_get" => {
                let number: u64 = args
                    .get("number")
                    .and_then(|v| {
                        v.as_u64()
                            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                    })
                    .ok_or("number обязателен (число)")?;
                let view = self.tasks.card(&project, number).await.map_err(view_err)?;
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
            "docs_ls" => {
                let list = self.docs.list(&arg("path")).await.map_err(view_err)?;
                ok_text(list.join("\n"))
            }
            "docs_read" => {
                let md = self.docs.read(&arg("slug")).await.map_err(view_err)?;
                ok_text(md)
            }
            "person_show" => {
                let md = self.person.show().await.map_err(view_err)?;
                ok_text(md)
            }
            "goal_list" => {
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
            "docs_append" => {
                let slug = arg("slug");
                let line = arg("line");
                if slug.is_empty() || line.is_empty() {
                    return Err("slug и line обязательны".into());
                }
                let msg = {
                    let m = arg("message");
                    if m.is_empty() {
                        format!("append: {slug}")
                    } else {
                        m
                    }
                };
                self.wiki
                    .append_doc(&slug, &line, &msg)
                    .await
                    .map_err(|e| e.to_string())?;
                ok_text(format!("дописано в «{slug}»"))
            }
            "docs_write" => {
                let slug = arg("slug");
                let content = arg("content");
                if slug.is_empty() || content.is_empty() {
                    return Err("slug и content обязательны".into());
                }
                let msg = {
                    let m = arg("message");
                    if m.is_empty() {
                        format!("write: {slug}")
                    } else {
                        m
                    }
                };
                self.wiki
                    .write_doc(&slug, &content, &msg)
                    .await
                    .map_err(|e| e.to_string())?;
                ok_text(format!("док «{slug}» записан"))
            }
            "cron_list" => {
                // тонкая обёртка: инвентарь живёт в юзкейсе (masters())
                let masters = self
                    .crons
                    .masters(&project)
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
            "search" => {
                let query = arg("query");
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
            "session_export" => {
                let session = arg("session");
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
            "task_create" => {
                let raw_kind = arg("type");
                if !raw_kind.is_empty() && uwuwu_usecase::parse_task_type(&raw_kind).is_none() {
                    return Err(format!(
                        "неизвестный тип «{raw_kind}»: bug|spike|docs|feature|refactoring|legacy|qa|infrastructure|other"
                    ));
                }
                let spec = uwuwu_usecase::NewTask {
                    title: arg("title"),
                    body_md: arg("body"),
                    kind: uwuwu_usecase::parse_task_type(&raw_kind),
                    priority: arg("priority"),
                    autodone: args
                        .get("autodone")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true),
                };
                let n = self
                    .writer
                    .create(&project, spec)
                    .await
                    .map_err(|e| e.to_string())?;
                ok_text(format!(
                    "создана задача #{n} (Analys) на доске проекта {project}"
                ))
            }
            "task_edit" => {
                let number = need_number(args)?;
                let patch = uwuwu_usecase::TaskPatch {
                    status: str_arg(args, "status").map(|s| uwuwu_usecase::parse_status(&s)),
                    branch: str_arg(args, "branch"),
                    pr: str_arg(args, "pr"),
                    priority: str_arg(args, "priority"),
                    title: str_arg(args, "title"),
                };
                if patch.status.is_none()
                    && patch.branch.is_none()
                    && patch.pr.is_none()
                    && patch.priority.is_none()
                    && patch.title.is_none()
                {
                    return Err("пустой патч: укажи хотя бы одно поле".into());
                }
                self.writer
                    .edit(&project, number, patch)
                    .await
                    .map_err(|e| e.to_string())?;
                ok_text(format!("задача #{number} обновлена"))
            }
            "task_claim" => {
                let number = need_number(args)?;
                let session = arg("session");
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
            "task_comment" => {
                let number = need_number(args)?;
                let text = arg("text");
                let reply = str_arg(args, "reply_to");
                self.writer
                    .comment(number, &text, reply.as_deref())
                    .await
                    .map_err(|e| e.to_string())?;
                ok_text(format!("коммент отправлен в #{number}"))
            }
            "task_close" => {
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
            "task_body" => {
                let number = need_number(args)?;
                let body = arg("body");
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
            "task_body_section" => {
                let number = need_number(args)?;
                let section = arg("section");
                let content = arg("content");
                let base_hash = arg("base_hash");
                if section.trim().is_empty() {
                    return Err("section обязателен (заголовок секции)".into());
                }
                if base_hash.trim().is_empty() {
                    return Err("base_hash обязателен (body_hash из fm task_get)".into());
                }
                let report = self
                    .writer
                    .set_body_section(&project, number, &section, &content, &base_hash)
                    .await
                    .map_err(|e| e.to_string())?;
                ok_text(render_section_report(&section, &report))
            }
            other => Err(format!("unknown tool: {other}")),
        }
    }
}

fn view_err(e: uwuwu_usecase::ViewError) -> String {
    e.to_string()
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
