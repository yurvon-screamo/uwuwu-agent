//! `uwuwu-cli task` — CRUD канбан-задач на AFFiNE.
//!
//! Модель (см. док «Фабрика разработки на AFFiNE»): задача = строка
//! database в доске проекта (`Задачи <Proj>`) + линкованный док-тело с
//! тегом «Task». Комменты — нативные комменты афины к доку-телу.

use anyhow::{anyhow, bail, Context as _, Result};
use serde_json::{json, Value};

use crate::affine_client::{AffineClient, AffineConfig};

/// Тип задачи — тег на теле. Определяет шаблон тела и пайп агента.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskType {
    Dev,
    Spike,
    Bug,
}

impl TaskType {
    pub fn tag(self) -> &'static str {
        match self {
            TaskType::Dev => "Dev",
            TaskType::Spike => "Spike",
            TaskType::Bug => "Bug",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "dev" | "разработка" => Some(TaskType::Dev),
            "spike" | "спайк" => Some(TaskType::Spike),
            "bug" | "баг" => Some(TaskType::Bug),
            _ => None,
        }
    }

    /// Стартовый костяк тела: принципиальное описание, не протокол.
    /// Секции-подсказки; диаграммы — только по запросу/неясности текста.
    pub fn body_template(self, title: &str, user_text: &str) -> String {
        let ctx = if user_text.trim().is_empty() {
            "<текст постановки>".to_string()
        } else {
            user_text.trim().to_string()
        };
        match self {
            TaskType::Dev => format!(
                "# {title}\n\n## Постановка\n{ctx}\n\n## Решение\n<принципиально, как для нового инженера: как теперь устроено и почему так — концепция, без файлов>\n\n## Юзкейсы\n<сценарии: кто что делает>\n\n## Сиквенсы\n<взаимодействия текстом, по строке на шаг: А → Б: что>\n\n## Проверка\n<исполняемое: команды, тесты, живой прогон>\n"
            ),
            TaskType::Spike => format!(
                "# {title}\n\n## Контекст\n{ctx}\n\n## Варианты\n<что рассматривали: суть → плюсы/минусы/замеры; сравнение — таблица>\n\n## Вывод\n<рекомендация + доказательства: «путь A, потому что …»>\n\n## Границы\n<что НЕ исследовано и почему; где вывод не действует>\n"
            ),
            TaskType::Bug => format!(
                "# {title}\n\n## Сценарий (BDD)\n```gherkin\nСценарий: <название>\n  Дано <состояние мира>\n  Когда <действие>\n  Тогда <ожидание>  # факт: <что было на самом деле>\n```\n\n{ctx}\n\n## Красный тест\n<имя теста, который воспроизводит; не воспроизвёлся на стенде → детали окружения>\n\n## Причина\n<механизм: почему система так себя вела>\n\n## Фикс\n<что изменилось; красный тест теперь зелёный>\n\n## Профилактика\n<как класс бага закрыт: тест/инвариант/линтер>\n"
            ),
        }
    }
}

/// Тег тел крон (доска Crons).
pub const CRON_TAG: &str = "Cron";

/// Доска проекта: док + database-блок внутри него.
pub struct Board {
    pub doc_id: String,
    pub db_id: String,
}

/// Найти доску проекта (`Задачи <project>`).
pub async fn find_board(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    project: &str,
) -> Result<Board> {
    let title = format!("Задачи {project}");
    match find_board_by_title(client, cfg, &title).await {
        Ok(board) => Ok(board),
        // индекс тайтлов отстаёт от свежих/переименованных доков —
        // фоллбек через организ-дерево: папка проекта → дети по тайтлу
        Err(search_err) => match find_board_via_folder(client, cfg, project, &title).await {
            Some(board) => Ok(board),
            None => Err(search_err),
        },
    }
}

/// Резолв доски без поискового индекса: папка `<project>` → её доки → точный тайтл.
async fn find_board_via_folder(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    project: &str,
    title: &str,
) -> Option<Board> {
    let nodes = client
        .call_tool("list_organize_nodes", json!({"workspaceId": cfg.workspace}))
        .await
        .ok()?;
    let empty: Vec<serde_json::Value> = Vec::new();
    let all = nodes
        .get("nodes")
        .and_then(|n| n.as_array())
        .cloned()
        .unwrap_or(empty);
    let folder_ids: Vec<String> = all
        .iter()
        .filter(|n| {
            n.get("type").and_then(|t| t.as_str()) == Some("folder")
                && n.get("data").and_then(|d| d.as_str()) == Some(project)
        })
        .filter_map(|n| n.get("id").and_then(|i| i.as_str()).map(String::from))
        .collect();
    for fid in folder_ids {
        let doc_ids: Vec<String> = all
            .iter()
            .filter(|n| {
                n.get("type").and_then(|t| t.as_str()) == Some("doc")
                    && n.get("parentId").and_then(|p| p.as_str()) == Some(&fid)
            })
            .filter_map(|n| n.get("data").and_then(|d| d.as_str()).map(String::from))
            .collect();
        for doc_id in doc_ids {
            if let Ok(d) = client
                .call_tool(
                    "get_doc",
                    json!({"workspaceId": cfg.workspace, "docId": doc_id}),
                )
                .await
            {
                if d.get("title").and_then(|t| t.as_str()) == Some(title) {
                    return board_from_doc(client, cfg, &doc_id).await.ok();
                }
            }
        }
    }
    None
}

/// Найти доску по точному тайтлу (например, «Crons»).
/// Крон-доски: «Crons» + «Кроны <proj>» по папкам Projects (динамически —
/// новые проекты подхватываются без правки cli). Заголовки, не Board'ы.
pub async fn cron_board_titles(
    client: &mut AffineClient,
    cfg: &AffineConfig,
) -> Result<Vec<String>> {
    let mut out = vec!["Crons".to_string()];
    let Ok(nodes) = client
        .call_tool("list_organize_nodes", json!({"workspaceId": cfg.workspace}))
        .await
    else {
        return Ok(out);
    };
    let empty: Vec<serde_json::Value> = Vec::new();
    let all = nodes
        .get("nodes")
        .and_then(|n| n.as_array())
        .cloned()
        .unwrap_or(empty);
    let Some(pid) = all
        .iter()
        .find(|n| {
            n.get("type").and_then(|t| t.as_str()) == Some("folder")
                && n.get("data").and_then(|d| d.as_str()) == Some("Projects")
        })
        .and_then(|n| n.get("id").and_then(|i| i.as_str()))
        .map(String::from)
    else {
        return Ok(out);
    };
    let mut projects: Vec<String> = all
        .iter()
        .filter(|n| {
            n.get("type").and_then(|t| t.as_str()) == Some("folder")
                && n.get("parentId").and_then(|p| p.as_str()) == Some(pid.as_str())
        })
        .filter_map(|n| n.get("data").and_then(|d| d.as_str()).map(String::from))
        .collect();
    projects.sort();
    for p in projects {
        out.push(format!("Кроны {p}"));
    }
    Ok(out)
}

/// Результат поиска доски по тайтлу: живая / в трэше / нет вообще.
/// Нужен, чтобы cron list честно показывал ВСЕ крон-доски, а не только
/// непустые — «молчаливые» пропуски читались как «видна одна пустая доска».
pub enum BoardFind {
    Found(Board),
    /// док с тайтлом есть, но все матччи в трэше
    Trashed,
    /// живого дока с таким тайтлом нет
    Missing,
}

pub async fn board_lookup(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    title: &str,
) -> Result<BoardFind> {
    let resp = client
        .call_tool(
            "find_doc_by_title",
            json!({"workspaceId": cfg.workspace, "title": title, "caseInsensitive": true, "limit": 5}),
        )
        .await
        .context("find_doc_by_title")?;
    let matches = resp
        .get("matches")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    let alive = matches
        .iter()
        .find(|m| m.get("inTrash").and_then(|t| t.as_bool()) != Some(true));
    let Some(doc) = alive.and_then(|m| m.get("id")).and_then(|v| v.as_str()) else {
        return Ok(if matches.is_empty() {
            BoardFind::Missing
        } else {
            BoardFind::Trashed
        });
    };
    match board_from_doc(client, cfg, doc).await {
        Ok(board) => Ok(BoardFind::Found(board)),
        // живой док есть, но канбана в нём нет — для крон-досок это «нет доски»
        Err(_) => Ok(BoardFind::Missing),
    }
}

/// Фоллбек: индексатор поиска слепнет к части доков (ACL-снапшот на момент
/// индексации — доки без правок с миграции не находятся боту). Резолвим доску
/// через organize-дерево: папка проекта → doc-линки → get_doc → тайтл.
/// Лимит 50 кандидатов: доски задач/кронов стоят в начале папок по index.
pub async fn find_board_by_title_tree(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    title: &str,
) -> Result<Board> {
    let nodes = client
        .call_tool("list_organize_nodes", json!({"workspaceId": cfg.workspace}))
        .await
        .context("list_organize_nodes")?;
    let all = nodes
        .get("nodes")
        .and_then(|n| n.as_array())
        .cloned()
        .unwrap_or_default();
    // «Задачи origa» / «Кроны origa» → папка origa в Projects
    let project = title.split_once(' ').map(|(_, p)| p.to_string());
    let is_folder =
        |n: &serde_json::Value| n.get("type").and_then(|t| t.as_str()) == Some("folder");
    let pid = all
        .iter()
        .find(|n| is_folder(n) && n.get("data").and_then(|d| d.as_str()) == Some("Projects"))
        .and_then(|n| n.get("id"))
        .and_then(|i| i.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow!("папка Projects не найдена в дереве"))?;
    let folder: Option<String> = project.as_ref().and_then(|p| {
        all.iter()
            .find(|n| {
                is_folder(n)
                    && n.get("parentId").and_then(|x| x.as_str()) == Some(pid.as_str())
                    && n.get("data").and_then(|d| d.as_str()) == Some(p.as_str())
            })
            .and_then(|n| n.get("id"))
            .and_then(|i| i.as_str())
            .map(String::from)
    });
    let folder_ids: Vec<String> = match &folder {
        Some(f) => vec![f.clone()],
        None => all
            .iter()
            .filter(|n| {
                is_folder(n) && n.get("parentId").and_then(|x| x.as_str()) == Some(pid.as_str())
            })
            .filter_map(|n| n.get("id").and_then(|i| i.as_str()).map(String::from))
            .collect(),
    };
    let mut checked = 0usize;
    for n in &all {
        if n.get("type").and_then(|t| t.as_str()) != Some("doc") {
            continue;
        }
        let Some(parent) = n.get("parentId").and_then(|x| x.as_str()) else {
            continue;
        };
        if !folder_ids.iter().any(|f| f == parent) {
            continue;
        }
        let Some(doc_id) = n.get("data").and_then(|d| d.as_str()) else {
            continue;
        };
        if checked >= 50 {
            break;
        }
        checked += 1;
        let Ok(doc) = client
            .call_tool(
                "get_doc",
                json!({"workspaceId": cfg.workspace, "docId": doc_id}),
            )
            .await
        else {
            continue;
        };
        let t = doc
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if t == title {
            return board_from_doc(client, cfg, doc_id).await;
        }
    }
    Err(anyhow!(
        "доска «{title}» не найдена в дереве (просмотрено {checked} доков)"
    ))
}

pub async fn find_board_by_title(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    title: &str,
) -> Result<Board> {
    match board_lookup(client, cfg, title).await? {
        BoardFind::Found(board) => Ok(board),
        BoardFind::Missing => {
            // поиск слепнет — резолвим деревом (канон-путь для досок проектов)
            find_board_by_title_tree(client, cfg, title).await
        }
        BoardFind::Trashed => Err(anyhow!(
            "доска «{title}» в трэше — восстанови из корзины или пересоздай"
        )),
    }
}

/// Доска из дока: найти database-блок (affine:database / affine:data_view).
async fn board_from_doc(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    doc_id: &str,
) -> Result<Board> {
    let doc = client
        .call_tool(
            "read_doc",
            json!({"workspaceId": cfg.workspace, "docId": doc_id}),
        )
        .await
        .context("read_doc board")?;
    let blocks = doc
        .get("blocks")
        .and_then(|b| b.as_array())
        .ok_or_else(|| anyhow!("пустой read_doc"))?;
    let db_id = blocks
        .iter()
        .find(|b| {
            let f = b.get("flavour").and_then(|v| v.as_str()).unwrap_or("");
            f == "affine:database" || f == "affine:data_view"
        })
        .and_then(|b| b.get("id").and_then(|v| v.as_str()))
        .ok_or_else(|| anyhow!("нет database-блока"))?
        .to_string();
    Ok(Board {
        doc_id: doc_id.to_string(),
        db_id,
    })
}

/// Одна строка доски.
#[derive(Clone, Debug, Default)]
pub struct TaskRow {
    pub row_id: String,
    pub title: String,
    pub body_doc: String,
    pub status: String,
    pub pr: String,
    pub branch: String,
    /// доска Crons: расписание и последний запуск
    pub schedule: String,
    pub last_run: String,
}

impl TaskRow {
    pub fn cron_schedule_of(&self) -> &str {
        &self.schedule
    }

    pub fn last_run_display(&self) -> &str {
        &self.last_run
    }
}

impl RowPatch {
    /// Статус для доски Crons (active/paused/fired).
    pub fn cron_status(status: &str) -> Self {
        RowPatch {
            status: Some(status.to_string()),
            ..Default::default()
        }
    }
}

/// Прочитать все строки доски. Заголовки линкованных строк резолвятся
/// из контент-кэша (sync уже принёс тела задач) и только при промахе —
/// get_doc-ом: без кэша list делал бы N round-trip-ов до Сингапура.
pub async fn read_rows(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    board: &Board,
    root: &std::path::Path,
) -> Result<Vec<TaskRow>> {
    let cache_titles: std::collections::BTreeMap<String, String> =
        crate::content_cache::ContentCache::load(root)
            .map(|c| {
                c.iter()
                    .map(|(_, d)| (d.doc_id.clone(), d.title.clone()))
                    .collect()
            })
            .unwrap_or_default();
    let resp = client
        .call_tool(
            "read_database_cells",
            json!({"workspaceId": cfg.workspace, "docId": board.doc_id, "databaseBlockId": board.db_id}),
        )
        .await
        .context("read_database_cells")?;
    let rows = resp
        .get("rows")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let cells = r.get("cells").cloned().unwrap_or(Value::Null);
        let cell = |k: &str| -> String {
            cells
                .get(k)
                .and_then(|c| c.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let linked = first_str(&r, &["linkedDocId"]).unwrap_or_default();
        let mut title = r
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or(" ")
            .trim()
            .to_string();
        // linked rows carry their title on the linked page, not the
        // cell: the "title" cell is a single-space LinkedPage reference,
        // blank after trim — resolve from the cache, network on miss
        if title.is_empty() && !linked.is_empty() {
            title = cache_titles.get(&linked).cloned().unwrap_or_default();
            if title.is_empty() {
                if let Ok(doc) = client
                    .call_tool(
                        "get_doc",
                        json!({"workspaceId": cfg.workspace, "docId": linked}),
                    )
                    .await
                {
                    title = doc
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or_default()
                        .to_string();
                }
            }
        }
        out.push(TaskRow {
            row_id: first_str(&r, &["rowBlockId", "rowId", "id"]).unwrap_or_default(),
            title,
            body_doc: linked,
            status: cell("Status"),
            pr: cell("PR"),
            branch: cell("Branch"),
            schedule: cell("Schedule"),
            last_run: cell("Last Run"),
        });
    }
    Ok(out)
}

/// Найти строку по id (row id или doc-тела) или уникальному префиксу.
/// Ведущие дефисы row-id игнорируются с обеих сторон (кли-аргументы).
pub fn resolve_row<'a>(rows: &'a [TaskRow], id: &str) -> Result<&'a TaskRow> {
    let needle = id.trim_start_matches('-');
    let by_id: Vec<&TaskRow> = rows
        .iter()
        .filter(|r| {
            r.row_id.trim_start_matches('-').starts_with(needle) || r.body_doc.starts_with(needle)
        })
        .collect();
    if by_id.len() == 1 {
        return Ok(by_id[0]);
    }
    if by_id.is_empty() {
        let lower = needle.to_lowercase();
        let by_title: Vec<&TaskRow> = rows
            .iter()
            .filter(|r| r.title.to_lowercase().contains(&lower))
            .collect();
        return match by_title.len() {
            1 => Ok(by_title[0]),
            0 => bail!("задача '{id}' не найдена"),
            n => bail!("'{id}' неоднозначно ({n} по заголовку) — уточни или используй id"),
        };
    }
    bail!(
        "'{id}' неоднозначно ({} по id) — уточни префикс",
        by_id.len()
    )
}

/// Создать задачу: док-тело (тег «Task») + строка Todo, слинкованная с телом.
pub async fn create_task(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    board: &Board,
    title: &str,
    body_markdown: &str,
    task_type: TaskType,
) -> Result<TaskRow> {
    // 1. док-тело — ребёнок доски; стартовый костяк по типу задачи
    let md = task_type.body_template(title, body_markdown);
    let resp = client
        .call_tool(
            "create_doc_from_markdown",
            json!({"workspaceId": cfg.workspace, "title": title, "markdown": md, "parentDocId": board.doc_id}),
        )
        .await
        .context("create_doc_from_markdown")?;
    let body_doc = first_str(&resp, &["docId", "id"])
        .ok_or_else(|| anyhow!("тело создано, но docId не возвращён: {resp:.120}"))?;

    // 2. тег типа (идемпотентно)
    let tag = task_type.tag();
    let tags = client
        .call_tool("list_tags", json!({"workspaceId": cfg.workspace}))
        .await;
    let has_tag = tags
        .ok()
        .and_then(|t| {
            t.get("tags").and_then(|a| a.as_array()).map(|arr| {
                arr.iter().any(|x| {
                    x.get("name").and_then(|n| n.as_str()) == Some(tag)
                        || x.get("tag").and_then(|n| n.as_str()) == Some(tag)
                })
            })
        })
        .unwrap_or(false);
    if !has_tag {
        let _ = client
            .call_tool(
                "create_tag",
                json!({"workspaceId": cfg.workspace, "tag": tag}),
            )
            .await;
        // cross-machine race: a concurrent create may land the same name.
        // delete_tag works by name only, so duplicates get disarmed by a
        // name-reset (delete + single re-create) — the #7 requests lesson
        let relist = client
            .call_tool("list_tags", json!({"workspaceId": cfg.workspace}))
            .await;
        if let Ok(relist) = relist {
            if relist.get("ok").and_then(|o| o.as_bool()) == Some(false) {
                eprintln!("affine: re-list тегов не удался — гонка могла остаться");
            }
            let dup = relist
                .get("tags")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter(|x| {
                            x.get("name").and_then(|n| n.as_str()) == Some(tag)
                                || x.get("tag").and_then(|n| n.as_str()) == Some(tag)
                        })
                        .count()
                })
                .unwrap_or(0);
            if dup > 1 {
                let _ = client
                    .call_tool(
                        "delete_tag",
                        json!({"workspaceId": cfg.workspace, "tag": tag}),
                    )
                    .await;
                let _ = client
                    .call_tool(
                        "create_tag",
                        json!({"workspaceId": cfg.workspace, "tag": tag}),
                    )
                    .await;
                eprintln!("affine: гонка тега {tag} разряжена (name-reset)");
            }
        }
    }
    let _ = client
        .call_tool(
            "add_tag_to_doc",
            json!({"workspaceId": cfg.workspace, "docId": body_doc, "tag": tag}),
        )
        .await;

    // 3. строка в Todo, слинкованная с телом
    let resp = client
        .call_tool(
            "add_database_row",
            json!({"workspaceId": cfg.workspace, "docId": board.doc_id,
                   "databaseBlockId": board.db_id,
                   "cells": {"Title": title},
                   "linkedDocId": body_doc}),
        )
        .await
        .context("add_database_row")?;
    let row_id = first_str(&resp, &["rowBlockId", "rowId", "id"]).unwrap_or_default();

    Ok(TaskRow {
        row_id,
        title: title.to_string(),
        body_doc,
        status: String::new(), // пустой статус = черновик юзера; Design = спавн агента
        ..Default::default()
    })
}

/// Обновить ячейки строки (Status/Title/PR/Branch — только непустые).
/// Патч колонок строки; пустой = нечего обновлять.
#[derive(Default)]
pub struct RowPatch {
    pub status: Option<String>,
    pub title: Option<String>,
    pub pr: Option<String>,
    pub branch: Option<String>,
}

impl RowPatch {
    fn is_empty(&self) -> bool {
        self.status.is_none() && self.title.is_none() && self.pr.is_none() && self.branch.is_none()
    }

    fn cells(&self) -> Value {
        let mut cells = serde_json::Map::new();
        if let Some(v) = &self.status {
            cells.insert("Status".into(), json!(v));
        }
        if let Some(v) = &self.title {
            cells.insert("Title".into(), json!(v));
        }
        if let Some(v) = &self.pr {
            cells.insert("PR".into(), json!(v));
        }
        if let Some(v) = &self.branch {
            cells.insert("Branch".into(), json!(v));
        }
        Value::Object(cells)
    }
}

/// Обновить ячейки строки (только непустые поля патча).
pub async fn edit_row(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    board: &Board,
    row: &TaskRow,
    patch: &RowPatch,
) -> Result<()> {
    if patch.is_empty() {
        bail!("нечего обновлять: передай --status/--title/--pr/--branch");
    }
    let cells = patch.cells();
    client
        .call_tool(
            "update_database_row",
            json!({"workspaceId": cfg.workspace, "docId": board.doc_id,
                   "databaseBlockId": board.db_id, "rowBlockId": row.row_id,
                   "cells": cells}),
        )
        .await
        .map(|_| ())
        .context("update_database_row")
}

/// Коммент в док-тело задачи.
pub async fn comment_task(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    row: &TaskRow,
    text: &str,
) -> Result<String> {
    if row.body_doc.is_empty() {
        bail!("у задачи нет док-тела — комментить некуда");
    }
    let resp = client
        .call_tool(
            "create_comment",
            json!({"workspaceId": cfg.workspace, "docId": row.body_doc, "content": text}),
        )
        .await
        .context("create_comment")?;
    Ok(first_str(&resp, &["commentId", "id"]).unwrap_or_default())
}

/// Последние комменты тела задачи (свежие первыми).
pub async fn recent_comments(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    row: &TaskRow,
    limit: usize,
) -> Result<Vec<(String, String)>> {
    if row.body_doc.is_empty() {
        return Ok(Vec::new());
    }
    let resp = client
        .call_tool(
            "list_comments",
            json!({"workspaceId": cfg.workspace, "docId": row.body_doc, "first": limit as u64}),
        )
        .await
        .context("list_comments")?;
    let mut out = Vec::new();
    if let Some(edges) = resp.get("edges").and_then(|e| e.as_array()) {
        for e in edges {
            let node = e.get("node").cloned().unwrap_or(Value::Null);
            let content = node.get("content").cloned().unwrap_or(Value::Null);
            let preview = content
                .get("preview")
                .and_then(|p| p.as_str())
                .unwrap_or_default()
                .to_string();
            // fresh comments may carry an empty preview — walk the
            // snapshot blocks for the actual text deltas
            let preview = if preview.trim().is_empty() {
                snapshot_text(&content)
            } else {
                preview
            };
            let resolved = node
                .get("resolved")
                .and_then(|r| r.as_bool())
                .unwrap_or(false);
            let marker = if resolved { "[✓] " } else { "" };
            out.push((
                format!("{marker}{preview}"),
                first_str(&node, &["id"]).unwrap_or_default(),
            ));
        }
    }
    Ok(out)
}

fn first_str(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(|x| x.as_str()))
        .map(String::from)
}

/// Concatenate all text deltas from a comment's snapshot block tree.
fn snapshot_text(content: &Value) -> String {
    fn walk(node: &Value, out: &mut String) {
        if let Some(delta) = node
            .get("props")
            .and_then(|p| p.get("text"))
            .and_then(|t| t.get("delta"))
            .and_then(|d| d.as_array())
        {
            for d in delta {
                if let Some(ins) = d.get("insert").and_then(|i| i.as_str()) {
                    out.push_str(ins);
                }
            }
        }
        if let Some(children) = node.get("children").and_then(|c| c.as_array()) {
            for ch in children {
                walk(ch, out);
            }
        }
    }
    let mut out = String::new();
    if let Some(blocks) = content.get("snapshot").and_then(|s| s.get("blocks")) {
        walk(blocks, &mut out);
    }
    out.trim().to_string()
}

/// Все комменты тела задачи хронологически: (текст, userId, ms-таймстамп, resolved).
/// userId — автор (бот vs юзер), createDate — из снапшота (надёжный порядок).
pub async fn thread_comments(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    row: &TaskRow,
) -> Result<Vec<(String, String, i64, bool)>> {
    if row.body_doc.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut cursor = String::new();
    for _page in 0..10 {
        let mut args = json!({"workspaceId": cfg.workspace, "docId": row.body_doc, "first": 50});
        if !cursor.is_empty() {
            args["after"] = json!(cursor);
        }
        let resp = client
            .call_tool("list_comments", args)
            .await
            .context("list_comments")?;
        let edges = resp
            .get("edges")
            .and_then(|e| e.as_array())
            .cloned()
            .unwrap_or_default();
        let has_next = resp
            .pointer("/pageInfo/hasNextPage")
            .and_then(|h| h.as_bool())
            .unwrap_or(false);
        cursor = resp
            .pointer("/pageInfo/endCursor")
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string();
        for e in &edges {
            let node = e.get("node").cloned().unwrap_or(Value::Null);
            let content = node.get("content").cloned().unwrap_or(Value::Null);
            let preview = content
                .get("preview")
                .and_then(|p| p.as_str())
                .unwrap_or_default()
                .to_string();
            let preview = if preview.trim().is_empty() {
                snapshot_text(&content)
            } else {
                preview
            };
            let created = content
                .pointer("/snapshot/meta/createDate")
                .and_then(|c| c.as_i64())
                .unwrap_or(0);
            let resolved = node
                .get("resolved")
                .and_then(|r| r.as_bool())
                .unwrap_or(false);
            let user_id = node
                .pointer("/user/id")
                .and_then(|u| u.as_str())
                .unwrap_or_default()
                .to_string();
            out.push((preview, user_id, created, resolved));
        }
        if !has_next || cursor.is_empty() {
            break;
        }
    }
    out.sort_by_key(|(_, _, created, _)| *created);
    Ok(out)
}

// ——— кроны (доска Crons) ———

/// Заглушка тела крона: что делать на каждом срабатывании.
pub fn cron_body_stub(title: &str, schedule: &str, prompt: &str) -> String {
    if prompt.trim().is_empty() {
        format!(
            "# {title}\n\nРасписание: `{schedule}`.\n\n## Что делать на каждом срабатывании\n(опиши работу агента)\n\n## Лог\n_Отчёты приходят комментами._\n"
        )
    } else {
        format!("# {title}\n\nРасписание: `{schedule}`.\n\n## Что делать\n\n{prompt}\n")
    }
}

/// Разобрать человекочитаемое смещение (10m/90s/2h) в минуты.
pub fn parse_offset_minutes(s: &str) -> Result<i64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().saturating_sub(1));
    let n: i64 = num
        .parse()
        .with_context(|| format!("--in '{s}': ожидалось число+единица (10m/90s/2h)"))?;
    match unit {
        "s" => Ok((n + 59) / 60), // секунды округляем вверх до минут (гранулярность демона)
        "m" => Ok(n),
        "h" => Ok(n * 60),
        _ => anyhow::bail!("--in '{s}': единицы s/m/h"),
    }
}

/// Schedule-строка для разового срабатывания через `minutes` от сейчас.
/// Формат «HH:MM» сегодня или «HH:MM DD.MM» при переходе через полночь.
pub fn one_shot_schedule(minutes: i64) -> String {
    use chrono::{Duration, Local};
    let target = Local::now() + Duration::minutes(minutes);
    let now = Local::now();
    if target.date_naive() == now.date_naive() {
        target.format("%H:%M").to_string()
    } else {
        target.format("%H:%M %d.%m").to_string()
    }
}

/// Валидация Schedule-строки (упрощённая — полный парсер в демоне).
pub fn validate_schedule(s: &str) -> Result<()> {
    let t = s.trim();
    if regex::Regex::new(r"^\d+[mhd]$").unwrap().is_match(t) {
        return Ok(());
    }
    if regex::Regex::new(r"^\d{1,2}:\d{2}( \d{1,2}\.\d{1,2})?$")
        .unwrap()
        .is_match(t)
    {
        return Ok(());
    }
    if t.split_whitespace().count() == 5 {
        return Ok(());
    }
    anyhow::bail!(
        "расписание '{s}': ожидался интервал (5m), время (09:30 [01.12]) или cron-expr (5 полей)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(row_id: &str, body: &str, title: &str) -> TaskRow {
        TaskRow {
            row_id: row_id.into(),
            body_doc: body.into(),
            title: title.into(),
            ..Default::default()
        }
    }

    #[test]
    fn resolve_by_prefix() {
        let rows = vec![
            row("abc123", "docX", "первая"),
            row("def456", "docY", "вторая"),
        ];
        assert_eq!(resolve_row(&rows, "abc").unwrap().title, "первая");
        assert_eq!(resolve_row(&rows, "docY").unwrap().title, "вторая");
        assert!(resolve_row(&rows, "zzz").is_err());
    }

    #[test]
    fn resolve_ambiguous() {
        let rows = vec![row("ab1", "", ""), row("ab2", "", "")];
        assert!(resolve_row(&rows, "ab").is_err());
    }
}

// ——— тело задачи: markdown с html_embed-фенсами ———

/// Фенс диаграммы из исходного markdown: ```html_embed [имя]\n…\n```
#[derive(Clone, Debug, PartialEq)]
pub struct HtmlEmbedFence {
    pub name: String,
    pub html: String,
}

/// Вытащить html_embed-фенсы из markdown (для предудаления старых версий).
pub fn parse_html_embed_fences(md: &str) -> Vec<HtmlEmbedFence> {
    let mut out = Vec::new();
    let mut lines = md.lines().peekable();
    while let Some(line) = lines.next() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("```html_embed") {
            let name = rest.trim().to_string();
            let mut html = Vec::new();
            for l in lines.by_ref() {
                if l.trim_start().starts_with("```") {
                    break; // конец фенса
                }
                html.push(l);
            }
            out.push(HtmlEmbedFence {
                name,
                html: html.join("\n").trim().to_string(),
            });
        }
    }
    out
}

/// Маркер-абзац перед embed: читается обратно (html-проп блока не читается).
pub fn diagram_marker(name: &str) -> String {
    format!("⁂ diagram:{name}")
}

/// Дописать markdown в тело: предудаление именованных старых диаграмм,
/// append_markdown, конвертация code[language=html_embed …] → маркер + embed.
/// Возвращает (вставлено диаграмм, удалено старых).
pub async fn append_body_markdown(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    body_doc: &str,
    md: &str,
) -> Result<(usize, usize)> {
    let fences = parse_html_embed_fences(md);
    let mut deleted = 0usize;

    // 1. именованные фенсы: удалить прежние пары маркер+embed
    if !fences.is_empty() {
        let doc = client
            .call_tool(
                "read_doc",
                json!({"workspaceId": cfg.workspace, "docId": body_doc}),
            )
            .await
            .context("read_doc body")?;
        let blocks = doc
            .get("blocks")
            .and_then(|b| b.as_array())
            .cloned()
            .unwrap_or_default();
        let note = blocks
            .iter()
            .find(|b| b.get("flavour").and_then(|f| f.as_str()) == Some("affine:note"));
        let child_ids: Vec<String> = note
            .and_then(|n| n.get("childIds"))
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let by_id: std::collections::HashMap<String, serde_json::Value> = blocks
            .iter()
            .filter_map(|b| {
                b.get("id")
                    .and_then(|i| i.as_str())
                    .map(String::from)
                    .map(|id| (id.clone(), b.clone()))
            })
            .collect();
        for fence in &fences {
            if fence.name.is_empty() {
                continue; // анонимные — только вставка
            }
            let marker = diagram_marker(&fence.name);
            for (i, cid) in child_ids.iter().enumerate() {
                let is_marker = by_id
                    .get(cid)
                    .map(|b| b.get("text").and_then(|t| t.as_str()) == Some(&marker))
                    .unwrap_or(false);
                if !is_marker {
                    continue;
                }
                let _ = client
                    .call_tool(
                        "delete_block",
                        json!({"workspaceId": cfg.workspace, "docId": body_doc, "blockId": cid}),
                    )
                    .await;
                deleted += 1;
                // следом идущий embed (если есть) — тоже
                if let Some(next) = child_ids.get(i + 1) {
                    let is_embed = by_id
                        .get(next)
                        .map(|b| {
                            (b.get("flavour").and_then(|f| f.as_str()))
                                .unwrap_or("")
                                .contains("embed")
                        })
                        .unwrap_or(false);
                    if is_embed {
                        let _ = client
                            .call_tool(
                                "delete_block",
                                json!({"workspaceId": cfg.workspace, "docId": body_doc, "blockId": next}),
                            )
                            .await;
                    }
                }
                // без break: чистим и дубли (легаси-мусор)
            }
        }
    }

    // 2. append markdown как есть (фенсы станут code-блоками)
    let resp = client
        .call_tool(
            "append_markdown",
            json!({"workspaceId": cfg.workspace, "docId": body_doc, "markdown": md}),
        )
        .await
        .context("append_markdown")?;
    if resp.get("ok").and_then(|o| o.as_bool()) == Some(false) {
        bail!(
            "append_markdown: {}",
            resp.get("error").and_then(|e| e.as_str()).unwrap_or("?")
        );
    }

    // 3. фенсы → живые embed-диаграммы
    let inserted = convert_embed_fences(client, cfg, body_doc).await?;
    Ok((inserted, deleted))
}

/// code[language=html_embed …] → маркер ¶ + embed_html, code — снести.
/// Возвращает число вставленных диаграмм.
async fn convert_embed_fences(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    body_doc: &str,
) -> Result<usize> {
    let doc = client
        .call_tool(
            "read_doc",
            json!({"workspaceId": cfg.workspace, "docId": body_doc}),
        )
        .await
        .context("read_doc for fence conversion")?;
    let blocks = doc
        .get("blocks")
        .and_then(|b| b.as_array())
        .cloned()
        .unwrap_or_default();
    let mut inserted = 0usize;
    for b in &blocks {
        let lang = b.get("language").and_then(|l| l.as_str()).unwrap_or("");
        let Some(rest) = lang.strip_prefix("html_embed") else {
            continue;
        };
        let name = rest.trim().to_string();
        let html = b.get("text").and_then(|t| t.as_str()).unwrap_or("");
        let code_id = b.get("id").and_then(|i| i.as_str()).unwrap_or_default();
        if html.trim().is_empty() {
            // пустой фенс = чистое удаление старой (уже удалена в п.1); code — снести
            let _ = client
                .call_tool(
                    "delete_block",
                    json!({"workspaceId": cfg.workspace, "docId": body_doc, "blockId": code_id}),
                )
                .await;
            continue;
        }
        // маркер ¶ после code (stackAfter), затем embed после маркера
        let marker_resp = client
            .call_tool(
                "append_block",
                json!({"workspaceId": cfg.workspace, "docId": body_doc, "type": "paragraph",
                       "text": diagram_marker(&name),
                       "placement": {"afterBlockId": code_id}}),
            )
            .await;
        let marker_id = marker_resp
            .ok()
            .and_then(|r| r.get("blockId").and_then(|v| v.as_str()).map(String::from))
            .unwrap_or_default();
        let stack = if marker_id.is_empty() {
            code_id
        } else {
            &marker_id
        };
        let emb = client
            .call_tool(
                "append_block",
                json!({"workspaceId": cfg.workspace, "docId": body_doc, "type": "embed_html",
                       "html": html,
                       "placement": {"afterBlockId": stack}}),
            )
            .await;
        // вставка не удалась (ok:false / транспорт) — code оставляем как источник
        let embed_ok =
            matches!(&emb, Ok(r) if r.get("ok").and_then(|o| o.as_bool()) != Some(false));
        if !embed_ok {
            continue;
        }
        let _ = client
            .call_tool(
                "delete_block",
                json!({"workspaceId": cfg.workspace, "docId": body_doc, "blockId": code_id}),
            )
            .await;
        inserted += 1;
    }
    Ok(inserted)
}

/// Заменить тело задачи ЦЕЛИКОМ (агент читал task get, правил, шлёт новое).
/// Rich-контент прежнего тела не переживает — диаграммы пересоздаются фенсами
/// html_embed из нового текста. Возвращает число вставленных диаграмм.
pub async fn replace_body(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    body_doc: &str,
    md: &str,
) -> Result<usize> {
    let resp = client
        .call_tool(
            "replace_doc_with_markdown",
            json!({"workspaceId": cfg.workspace, "docId": body_doc, "markdown": md}),
        )
        .await
        .context("replace_doc_with_markdown")?;
    if resp.get("ok").and_then(|o| o.as_bool()) == Some(false) {
        bail!(
            "replace_doc_with_markdown: {}",
            resp.get("error").and_then(|e| e.as_str()).unwrap_or("?")
        );
    }
    convert_embed_fences(client, cfg, body_doc).await
}

#[cfg(test)]
mod embed_tests {
    use super::*;

    #[test]
    fn parse_fences() {
        let md = "текст\n\n```html_embed arch\n<div>один</div>\n```\n\nещё\n\n```html_embed\n<div>аноним</div>\n```\n";
        let f = parse_html_embed_fences(md);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].name, "arch");
        assert_eq!(f[0].html, "<div>один</div>");
        assert_eq!(f[1].name, "");
    }

    #[test]
    fn empty_fence_is_delete() {
        let md = "```html_embed arch\n```";
        let f = parse_html_embed_fences(md);
        assert_eq!(f.len(), 1);
        assert!(f[0].html.is_empty());
    }

    #[test]
    fn other_fences_untouched() {
        let md = "```rust\nfn x() {}\n```";
        assert!(parse_html_embed_fences(md).is_empty());
    }

    #[test]
    fn marker_format() {
        assert_eq!(diagram_marker("arch"), "⁂ diagram:arch");
    }
}
