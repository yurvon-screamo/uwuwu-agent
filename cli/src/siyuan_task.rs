//! SiYuan-бекенд для uwuwu-cli task: те же команды и форматы вывода,
//! что и у афин-реализации (контракт для агентов/AGENTS.md не меняется).
//!
//! Модель (v1):
//! - доска «Задачи <Proj>» = док /<project>/Задачи <project> с database-блоком
//! - строка задачи: Title (primary), Status (select), Branch (text), PR (url)
//! - тело задачи = саб-док дока доски: /<project>/Задачи <project>/<title>
//! - комменты = секция «## Комменты» в теле (строки, реплаи — отступом)

use crate::siyuan::SiYuan;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

/// Валидный хит db_search: имя доски совпадает точно, анкор — по hPath
/// (первый сегмент — имя ноутбука) или как OR-страховка по blockID == doc_id.
fn pick_search_hit(results: &[Value], expect_hpath: &str, title: &str, doc_id: &str) -> Option<(String, String)> {
    results.iter().find_map(|hit| {
        let av_name = hit.get("avName").and_then(|n| n.as_str()).unwrap_or("");
        let av_id = hit.get("avID").and_then(|a| a.as_str()).unwrap_or("");
        let block_id = hit.get("blockID").and_then(|b| b.as_str()).unwrap_or("");
        let hpath = hit.get("hPath").and_then(|h| h.as_str()).unwrap_or("");
        let anchored = hpath == expect_hpath || block_id == doc_id;
        (av_name == title && anchored && !av_id.is_empty())
            .then(|| (av_id.to_string(), block_id.to_string()))
    })
}

pub struct SyBoard {
    pub doc_id: String,
    pub av_id: String,
    pub block_id: String,
    pub hpath: String,
    pub notebook: String,
}

/// Доска проекта: док /<project>/Задачи <project> + database-блок.
pub async fn find_board(sy: &SiYuan, project: &str) -> Result<SyBoard> {
    find_board_titled(sy, project, &format!("Задачи {project}")).await
}

pub async fn find_board_titled(sy: &SiYuan, project: &str, title: &str) -> Result<SyBoard> {
    let nb = sy.ensure_notebook("Projects").await?;
    let hpath = format!("/{p}/{title}", p = project);
    let doc_id = sy
        .doc_id_by_hpath(&nb, &hpath)
        .await?
        .ok_or_else(|| anyhow!("доска «{title}» не найдена ({hpath})"))?;
    // database-блок внутри дока: av-блок ядра (data-av-id=) или legacy i-носитель (avID=)
    let rows = sy
        .sql(&format!(
            "select id, markdown from blocks where root_id='{doc_id}' \
             and (type='av' or (type='i' and markdown like '%avID=%')) limit 5"
        ))
        .await?;
    let mut av_id = String::new();
    let mut block_id = String::new();
    for r in &rows {
        let md = r.get("markdown").and_then(|m| m.as_str()).unwrap_or("");
        if let Some(av) = extract_av_id(md) {
            av_id = av;
            block_id = r.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
            break;
        }
    }
    if av_id.is_empty() {
        // fallback: MCP db_search по имени; первый хит не доверяем —
        // несвежий индекс отдаёт фаззи-хиты чужих досок
        let v = sy.db_search(title).await?;
        let results = v
            .pointer("/data/results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let expect_hpath = format!("Projects{hpath}");
        match pick_search_hit(&results, &expect_hpath, title, &doc_id) {
            Some((av, block)) => {
                av_id = av;
                block_id = block;
            }
            None => bail!(
                "database доски «{title}» не найден: поиск вернул {} хит(ов), валидного нет (ожидался hPath «{expect_hpath}»)",
                results.len()
            ),
        }
    }
    Ok(SyBoard { doc_id, av_id, block_id, hpath, notebook: nb })
}

fn extract_av_id(md: &str) -> Option<String> {
    let markers = [("data-av-id=", "data-av-id=".len()), ("avID=", "avID=".len())];
    let (idx, marker_len) = markers
        .iter()
        .filter_map(|(marker, len)| md.find(marker).map(|pos| (pos, *len)))
        .min_by_key(|(pos, _)| *pos)?;
    let rest = md[idx + marker_len..].trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value_end = rest[1..].find(quote)?;
    Some(rest[1..1 + value_end].to_string())
}


pub struct SyTask {
    pub item_id: String,
    pub title: String,
    pub status: String,
    pub branch: String,
    pub pr: String,
    pub body_doc: Option<String>,
}

/// Строки доски через MCP database render.
pub async fn read_rows(sy: &SiYuan, board: &SyBoard) -> Result<Vec<SyTask>> {
    let v = sy.db_render(&board.av_id, &board.block_id).await?;
    // structuredContent = {action, data:{...}} — берём data
    let data = v.get("data").unwrap_or(&v);
    let keys = data
        .pointer("/view/columns")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let key_name = |id: &str| -> String {
        keys.iter()
            .find(|k| k.get("id").and_then(|i| i.as_str()) == Some(id))
            .and_then(|k| k.get("name").and_then(|n| n.as_str()))
            .unwrap_or("")
            .to_string()
    };
    let items = data
        .pointer("/view/rows")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for it in items {
        let mut t = SyTask {
            item_id: it.get("id").and_then(|i| i.as_str()).unwrap_or("").into(),
            title: String::new(),
            status: String::new(),
            branch: String::new(),
            pr: String::new(),
            body_doc: None,
        };
        for cell in it.get("cells").and_then(|c| c.as_array()).cloned().unwrap_or_default() {
            let kid = cell
                .pointer("/value/keyID")
                .and_then(|k| k.as_str())
                .unwrap_or("");
            let name = key_name(kid);
            let value = cell_value(&cell);
            match name.as_str() {
                "Primary Key" | "Title" | "Название" | "Первичный ключ" => t.title = value,
                "Status" | "Статус" => t.status = value,
                "Branch" => t.branch = value,
                "PR" | "Pr" => t.pr = value,
                _ => {}
            }
        }
        if !t.title.is_empty() {
            out.push(t);
        }
    }
    Ok(out)
}

fn cell_value(cell: &Value) -> String {
    let c = cell.get("value").unwrap_or(&Value::Null);
    if let Some(s) = c.as_str() {
        return s.to_string();
    }
    // ячейка несёт активный type: после конвертации ключа (text→url) в значении
    // остаётся stale text.content — читать нужно только поле активного типа
    let single = |ptr: &str| -> Option<String> {
        c.pointer(ptr).and_then(|s| s.as_str()).map(String::from)
    };
    let by_type = match c.get("type").and_then(|t| t.as_str()) {
        Some("block") => single("/block/content"),
        Some("text") => single("/text/content"),
        Some("url") => single("/url/content"),
        Some("email") => single("/email/content"),
        Some("phone") => single("/phone/content"),
        Some("select") | Some("mSelect") => {
            return c
                .get("mSelect")
                .and_then(|a| a.as_array())
                .map(|options| select_contents(options))
                .unwrap_or_default();
        }
        _ => None,
    };
    if let Some(v) = by_type {
        return v;
    }
    // фоллбек для значений без type: старый порядок указателей
    for ptr in ["/block/content", "/select/content", "/text/content", "/url/content", "/email/content", "/phone/content"] {
        if let Some(s) = single(ptr) {
            return s;
        }
    }
    c.get("mSelect")
        .and_then(|a| a.as_array())
        .map(|options| select_contents(options))
        .unwrap_or_default()
}

fn select_contents(options: &[Value]) -> String {
    options
        .iter()
        .filter_map(|o| o.get("content").and_then(|x| x.as_str()))
        .collect::<Vec<_>>()
        .join(",")
}

/// Тело задачи: саб-док доски по заголовку.
pub async fn body_doc_id(sy: &SiYuan, board: &SyBoard, title: &str) -> Result<Option<String>> {
    let hpath = format!("{}/{}", board.hpath, title);
    sy.doc_id_by_hpath(&board.notebook, &hpath).await
}

/// ключ Status в database.
async fn status_key(sy: &SiYuan, av: &str) -> Result<String> {
    let v = sy.mcp_tool("database", serde_json::json!({ "action": "keys", "id": av })).await?;
    let keys = v
        .pointer("/data/keys")
        .or_else(|| v.get("keys"))
        .and_then(|k| k.as_array())
        .cloned()
        .unwrap_or_default();
    keys.iter()
        .find(|k| k.get("name").and_then(|n| n.as_str()) == Some("Status"))
        .and_then(|k| k.get("id").and_then(|i| i.as_str()).map(String::from))
        .ok_or_else(|| anyhow!("ключ Status не найден в database"))
}

pub fn body_template(title: &str, ty: &str, ctx: &str) -> String {
    match ty {
        "spike" => format!(
            "# {title}\n\n## Контекст\n{ctx}\n\n## Варианты\n<что рассматривали: суть → плюсы/минусы/замеры; сравнение — таблица>\n\n## Вывод\n<рекомендация + доказательства: «путь A, потому что …»>\n\n## Границы\n<что НЕ исследовано и почему; где вывод не действует>\n"
        ),
        "bug" => format!(
            "# {title}\n\n## Сценарий (BDD)\n```gherkin\nСценарий: <название>\n  Дано <состояние мира>\n  Когда <действие>\n  Тогда <ожидание>  # факт: <что было на самом деле>\n```\n\n{ctx}\n\n## Красный тест\n<имя теста, который воспроизводит; не воспроизвёлся на стенде → детали окружения>\n\n## Причина\n<механизм: почему система так себя вела>\n\n## Фикс\n<что изменилось; красный тест теперь зелёный>\n\n## Профилактика\n<как класс бага закрыт: тест/инвариант/линтер>\n"
        ),
        _ => format!(
            "# {title}\n\n## Постановка\n{ctx}\n\n## Решение\n<принципиально, как для нового инженера: как теперь устроено и почему так — концепция, без файлов>\n\n## Юзкейсы\n<сценарии: кто что делает>\n\n## Сиквенсы\n<взаимодействия текстом, по строке на шаг: А → Б: что>\n\n## Проверка\n<исполняемое: команды, тесты, живой прогон>\n"
        ),
    }
}

/// Создание задачи: строка Draft + тело-саб-док (идемпотентно по заголовку).
pub async fn create(
    sy: &SiYuan,
    project: &str,
    title: &str,
    ty: &str,
    ctx: &str,
) -> Result<(String, String)> {
    let board = find_board(sy, project).await?;
    // идемпотентность: строка или тело с таким заголовком уже есть?
    let rows = read_rows(sy, &board).await?;
    if rows.iter().any(|r| r.title == title) {
        bail!("живая строка с тайтлом «{title}» уже есть на доске — дубль");
    }
    // тело сначала, затем СТРОКА ПРИВЯЗАНАЯ к нему (blockID): клик по карточке
    // открывает документ; detached-строки не создаём
    let body_path = format!("{}/{}", board.hpath, title);
    let body = sy.create_doc_md(&board.notebook, &body_path, &body_template(title, ty, ctx)).await?;
    let item = sy.db_item_add_bound(&board.av_id, &body).await?;
    let sk = status_key(sy, &board.av_id).await?;
    sy.db_item_set_status(&board.av_id, &sk, &item, "Draft").await?;
    // тип — тег в теле
    if !ty.is_empty() {
        let _ = sy.append_block_md(&body, &format!("\n#{ty}\n")).await;
    }
    Ok((item, body))
}

/// Резолв задачи по id (itemID/docID/префикс/хвост) или подстроке заголовка.
pub async fn resolve(sy: &SiYuan, board: &SyBoard, needle: &str) -> Result<SyTask> {
    let needle = needle.trim_start_matches('-');
    let rows = read_rows(sy, board).await?;
    let by_id: Vec<&SyTask> = rows
        .iter()
        .filter(|r| r.item_id == needle || r.item_id.starts_with(needle) || r.item_id.ends_with(needle))
        .collect();
    if by_id.len() == 1 {
        return Ok((*by_id[0]).clone_shallow());
    }
    if !by_id.is_empty() {
        bail!("неоднозначный id: {} совпадений", by_id.len());
    }
    // тело по префиксу
    for r in &rows {
        if let Some(doc) = body_doc_id(sy, board, &r.title).await? {
            if doc.starts_with(needle) {
                return Ok(r.clone_shallow());
            }
        }
    }
    let by_title: Vec<&SyTask> = rows
        .iter()
        .filter(|r| r.title.to_lowercase().contains(&needle.to_lowercase()))
        .collect();
    if by_title.len() == 1 {
        return Ok(by_title[0].clone_shallow());
    }
    if by_title.len() > 1 {
        bail!("неоднозначная подстрока: {} совпадений", by_title.len());
    }
    bail!("задача «{needle}» не найдена")
}

impl SyTask {
    fn clone_shallow(&self) -> Self {
        Self {
            item_id: self.item_id.clone(),
            title: self.title.clone(),
            status: self.status.clone(),
            branch: self.branch.clone(),
            pr: self.pr.clone(),
            body_doc: None,
        }
    }
}

/// Коммент: строка в «## Комменты» тела задачи (реплай — отступ).
pub async fn comment(sy: &SiYuan, board: &SyBoard, task: &SyTask, author: &str, text: &str, reply_to: Option<&str>) -> Result<()> {
    let body = body_doc_id(sy, board, &task.title)
        .await?
        .ok_or_else(|| anyhow!("тело задачи «{}» не найдено", task.title))?;
    let ts = chrono::Utc::now().format("%Y-%m-%d %H:%M");
    let line = match reply_to {
        Some(_) => format!("\n  - **{author}** ({ts}): {text}"),
        None => format!("\n- **{author}** ({ts}): {text}"),
    };
    sy.append_block_md(&body, &line).await?;
    Ok(())
}

/// Закрытие: статус Done (+note комментом).
pub async fn close(sy: &SiYuan, board: &SyBoard, task: &SyTask, note: Option<&str>) -> Result<()> {
    let sk = status_key(sy, &board.av_id).await?;
    sy.db_item_set_status(&board.av_id, &sk, &task.item_id, "Done").await?;
    if let Some(n) = note {
        let _ = comment(sy, board, task, "AGENTS", &format!("закрыта: {n}"), None).await;
    }
    Ok(())
}

/// Значение PR-ячейки (ключ типа url): пустая строка — очистка, иначе http(s)-ссылка.
fn pr_cell_value(pr: &str) -> Result<Value> {
    let pr = pr.trim();
    if pr.is_empty() {
        return Ok(serde_json::json!({ "url": { "content": "" } }));
    }
    if !pr.starts_with("http://") && !pr.starts_with("https://") {
        bail!("ключ PR хранит ссылку (url): ожидаю http(s)://…, получено «{pr}» (ветку пиши в --branch)");
    }
    Ok(serde_json::json!({ "url": { "content": pr } }))
}

/// Смена статуса/branch/pr (title-переименование не поддерживаем v1).
pub async fn edit(sy: &SiYuan, board: &SyBoard, task: &SyTask, status: Option<&str>, branch: Option<&str>, pr: Option<&str>) -> Result<()> {
    // валидация PR до любых записей: без частичных обновлений
    let pr_value = pr.map(pr_cell_value).transpose()?;
    let keys = sy.mcp_tool("database", serde_json::json!({ "action": "keys", "id": board.av_id })).await?;
    let arr = keys
        .pointer("/data/keys")
        .or_else(|| keys.get("keys"))
        .and_then(|k| k.as_array())
        .cloned()
        .unwrap_or_default();
    let find = |name: &str| -> Option<String> {
        arr.iter()
            .find(|k| k.get("name").and_then(|n| n.as_str()) == Some(name))
            .and_then(|k| k.get("id").and_then(|i| i.as_str()).map(String::from))
    };
    if let Some(st) = status {
        let k = find("Status").context("ключ Status не найден")?;
        sy.db_item_set_status(&board.av_id, &k, &task.item_id, st).await?;
    }
    if let Some(v) = branch {
        let k = find("Branch").context("ключ Branch не найден")?;
        sy.mcp_tool(
            "database",
            serde_json::json!({ "action": "item_update", "id": board.av_id, "keyID": k,
                    "itemID": task.item_id, "value": { "text": { "content": v } } }),
        )
        .await?;
    }
    if let Some(value) = pr_value {
        let k = find("PR").context("ключ PR не найден")?;
        sy.mcp_tool(
            "database",
            serde_json::json!({ "action": "item_update", "id": board.av_id, "keyID": k,
                    "itemID": task.item_id, "value": value }),
        )
        .await?;
    }
    Ok(())
}

/// Доска кронов: док /<project>/Кроны <project> с database-блоком.
pub async fn find_board_cron(sy: &SiYuan, project: &str) -> Result<SyBoard> {
    find_board_titled(sy, project, &format!("Кроны {project}")).await
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_av_id_reads_dash_case_attribute_view_block() {
        let md = r#"<div data-type="NodeAttributeView" data-av-id="20261104093449-q2h9ju2" data-av-type="kanban"></div>"#;
        assert_eq!(extract_av_id(md).as_deref(), Some("20261104093449-q2h9ju2"));
    }

    #[test]
    fn extract_av_id_reads_camel_case_carrier_block() {
        let md = r#"<div avID="20261104000238-96ch3l2"></div>"#;
        assert_eq!(extract_av_id(md).as_deref(), Some("20261104000238-96ch3l2"));
    }

    #[test]
    fn extract_av_id_returns_none_without_av_marker() {
        let md = r#"<div data-type="NodeDocument"></div>"#;
        assert_eq!(extract_av_id(md), None);
    }

    #[test]
    fn pick_search_hit_takes_exact_name_and_hpath() {
        let results = vec![json!({
            "avID": "av-1", "avName": "Задачи 1xgames", "blockID": "b-1",
            "hPath": "Projects/1xgames/Задачи 1xgames"
        })];
        let hit = pick_search_hit(&results, "Projects/1xgames/Задачи 1xgames", "Задачи 1xgames", "doc-x");
        assert_eq!(hit, Some(("av-1".into(), "b-1".into())));
    }

    #[test]
    fn pick_search_hit_skips_foreign_first_hit() {
        let results = vec![
            json!({
                "avID": "av-k", "avName": "Кроны 1xgames", "blockID": "b-k",
                "hPath": "Projects/1xgames/Кроны 1xgames"
            }),
            json!({
                "avID": "av-1", "avName": "Задачи 1xgames", "blockID": "b-1",
                "hPath": "Projects/1xgames/Задачи 1xgames"
            }),
        ];
        let hit = pick_search_hit(&results, "Projects/1xgames/Задачи 1xgames", "Задачи 1xgames", "doc-x");
        assert_eq!(hit, Some(("av-1".into(), "b-1".into())));
    }

    #[test]
    fn pick_search_hit_returns_none_without_valid_hits() {
        let results = vec![json!({
            "avID": "av-k", "avName": "Кроны 1xgames", "blockID": "b-k",
            "hPath": "Projects/1xgames/Кроны 1xgames"
        })];
        let hit = pick_search_hit(&results, "Projects/1xgames/Задачи 1xgames", "Задачи 1xgames", "doc-x");
        assert_eq!(hit, None);
    }

    #[test]
    fn pick_search_hit_accepts_hit_anchored_by_block_id() {
        let results = vec![json!({
            "avID": "av-9", "avName": "Задачи 1xgames", "blockID": "doc-77",
            "hPath": "Другой/путь"
        })];
        let hit = pick_search_hit(&results, "Projects/1xgames/Задачи 1xgames", "Задачи 1xgames", "doc-77");
        assert_eq!(hit, Some(("av-9".into(), "doc-77".into())));
    }

    fn cell_of(value: Value) -> Value {
        json!({ "value": value })
    }

    #[test]
    fn cell_value_prefers_url_over_stale_text_on_url_typed_cell() {
        // после конвертации ключа text→url в значении остаётся старый text.content
        let cell = cell_of(json!({
            "type": "url",
            "text": { "content": "https://old.example/pull/1" },
            "url": { "content": "https://new.example/pull/2" }
        }));
        assert_eq!(cell_value(&cell), "https://new.example/pull/2");
    }

    #[test]
    fn cell_value_returns_empty_for_url_cell_with_unfilled_url() {
        // до-миграционный артефакт: url пуст, текст застрял в text — честно пусто
        let cell = cell_of(json!({
            "type": "url",
            "text": { "content": "https://old.example/pull/1" },
            "url": { "content": "" }
        }));
        assert_eq!(cell_value(&cell), "");
    }

    #[test]
    fn cell_value_reads_text_typed_cell() {
        let cell = cell_of(json!({ "type": "text", "text": { "content": "feature/x" } }));
        assert_eq!(cell_value(&cell), "feature/x");
    }

    #[test]
    fn cell_value_reads_block_typed_cell() {
        let cell = cell_of(json!({ "type": "block", "block": { "content": "Заголовок" } }));
        assert_eq!(cell_value(&cell), "Заголовок");
    }

    #[test]
    fn cell_value_joins_multi_select_options() {
        let cell = cell_of(json!({
            "type": "mSelect",
            "mSelect": [{ "content": "Done", "color": "1" }, { "content": "Todo", "color": "2" }]
        }));
        assert_eq!(cell_value(&cell), "Done,Todo");
    }

    #[test]
    fn cell_value_falls_back_to_pointers_without_type() {
        let cell = cell_of(json!({ "text": { "content": "без type" } }));
        assert_eq!(cell_value(&cell), "без type");
    }

    #[test]
    fn pr_cell_value_wraps_https_link_into_url_payload() {
        let v = pr_cell_value("https://github.com/yurvon-screamo/uwuwu-agent/pull/36").unwrap();
        assert_eq!(v, json!({ "url": { "content": "https://github.com/yurvon-screamo/uwuwu-agent/pull/36" } }));
    }

    #[test]
    fn pr_cell_value_accepts_http_link() {
        let v = pr_cell_value("http://gitea.local/origa/pull/9").unwrap();
        assert_eq!(v, json!({ "url": { "content": "http://gitea.local/origa/pull/9" } }));
    }

    #[test]
    fn pr_cell_value_trims_whitespace_around_link() {
        let v = pr_cell_value("  https://github.com/a/b/pull/1  ").unwrap();
        assert_eq!(v, json!({ "url": { "content": "https://github.com/a/b/pull/1" } }));
    }

    #[test]
    fn pr_cell_value_empty_string_clears_the_cell() {
        let v = pr_cell_value("").unwrap();
        assert_eq!(v, json!({ "url": { "content": "" } }));
    }

    #[test]
    fn pr_cell_value_rejects_non_link_input() {
        let err = pr_cell_value("main").unwrap_err().to_string();
        assert!(err.contains("http(s)://"), "неожиданное сообщение: {err}");
        assert!(err.contains("--branch"), "неожиданное сообщение: {err}");
    }
}
