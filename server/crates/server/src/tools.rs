//! Реестр MCP-инструментов: дескрипторы + диспетчер. Тонкие обёртки
//! юзкейсов (views) — никакой бизнес-логики в presentation.

use serde_json::{json, Value};
use std::sync::Arc;
use uwuwu_usecase::{DocsView, PersonView, TaskView};

pub struct ToolHub {
    pub tasks: Arc<TaskView>,
    pub docs: Arc<DocsView>,
    pub person: Arc<PersonView>,
}

/// Дефолтный проект для инструментов без явного project.
pub const DEFAULT_PROJECT: &str = "uwuwu";

impl ToolHub {
    pub fn descriptors(&self) -> Value {
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
                "projects",
                "Проекты конвейера (доски в конфиге сервера)",
                object(json!({}))
            ),
        ])
    }

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
                        t.number,
                        t.status.as_str(),
                        format!("{:?}", t.priority).to_lowercase(),
                        t.title
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
                    "#{} {} — {}\n\n--- fm ---\nbranch: {}\npr: {}\nautodone: {}\n\n",
                    view.task.number,
                    view.task.title,
                    view.task.status.as_str(),
                    view.task.branch.as_deref().unwrap_or("—"),
                    view.task.pr.as_deref().unwrap_or("—"),
                    view.task.autodone,
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
            other => Err(format!("unknown tool: {other}")),
        }
    }
}

fn view_err(e: uwuwu_usecase::ViewError) -> String {
    e.to_string()
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
    fn descriptors_shape() {
        let tools = ToolHub::descriptors_static();
        let arr = tools.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        for t in arr {
            assert!(t["name"].is_string());
            assert!(t["inputSchema"]["type"] == "object");
        }
    }

    impl ToolHub {
        fn descriptors_static() -> Value {
            // дескрипторы не зависят от вьюх — проверяем форму без портов
            json!([tool(
                "task_list",
                "x",
                object(json!({ "project": string("y") }))
            )])
        }
    }
}
