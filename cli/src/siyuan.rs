//! SiYuan backend: REST (Kernel API) + MCP (/mcp) клиент и операции
//! поверх смигрированного uwuwu-воркспейса.
//!
//! Конфигурация: UWUWU_SIYUAN_URL (деф. http://127.0.0.1:6806) и
//! UWUWU_SIYUAN_TOKEN; при пустом токене читается api.token из
//! $UWUWU_SIYUAN_WS/conf/conf.json (деф. ~/siyuan-demo-ws).

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

pub struct SiYuan {
    pub base: String,
    pub token: String,
    client: reqwest::Client,
    // MCP-сессия (ленивая)
    mcp_sid: std::sync::Mutex<Option<String>>,
}

impl SiYuan {
    pub fn from_env() -> Result<Self> {
        let base = std::env::var("UWUWU_SIYUAN_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:6806".into());
        let token = match std::env::var("UWUWU_SIYUAN_TOKEN") {
            Ok(t) if !t.is_empty() => t,
            _ => {
                let ws = std::env::var("UWUWU_SIYUAN_WS").unwrap_or_else(|_| {
                    format!("{}/siyuan-demo-ws", std::env::var("HOME").unwrap_or_default())
                });
                let conf: Value = serde_json::from_str(
                    &std::fs::read_to_string(PathBuf::from(&ws).join("conf/conf.json"))
                        .with_context(|| format!("нет conf/conf.json в {ws} (задай UWUWU_SIYUAN_TOKEN)"))?,
                )?;
                conf.pointer("/api/token")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("api/token не найден в conf.json"))?
                    .to_string()
            }
        };
        Ok(Self {
            base,
            token,
            client: reqwest::Client::builder().timeout(std::time::Duration::from_secs(120)).build()?,
            mcp_sid: std::sync::Mutex::new(None),
        })
    }

    /// Прямая сборка клиента (тесты с мок-сервером; from_env — рабочий путь).
    pub fn at(base: &str, token: &str) -> Self {
        Self {
            base: base.to_string(),
            token: token.to_string(),
            client: reqwest::Client::builder().timeout(std::time::Duration::from_secs(120)).build().expect("reqwest client"),
            mcp_sid: std::sync::Mutex::new(None),
        }
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let mut last = String::new();
        for attempt in 0..4u32 {
            let resp = self
                .client
                .post(format!("{}{}", self.base, path))
                .header("Authorization", format!("Token {}", self.token))
                .json(&body)
                .send()
                .await;
            match resp {
                Ok(r) => {
                    let v: Value = r.json().await.unwrap_or(Value::Null);
                    if v.get("code").and_then(|c| c.as_i64()) == Some(0) {
                        return Ok(v.get("data").cloned().unwrap_or(Value::Null));
                    }
                    last = format!(
                        "code={} msg={}",
                        v.get("code").and_then(|c| c.as_i64()).unwrap_or(-999),
                        v.get("msg").and_then(|m| m.as_str()).unwrap_or("?")
                    );
                    if last.contains("metadata is not ready") {
                        tokio::time::sleep(std::time::Duration::from_millis(1500 + attempt as u64 * 1000)).await;
                        continue;
                    }
                }
                Err(e) => last = e.to_string(),
            }
            tokio::time::sleep(std::time::Duration::from_millis(800 * (attempt as u64 + 1))).await;
        }
        bail!("POST {path}: {last}")
    }

    // ---------- REST: ядро ----------

    pub async fn version(&self) -> Result<String> {
        let v = self.post("/api/system/version", json!({})).await?;
        Ok(v.as_str().unwrap_or("?").to_string())
    }

    pub async fn sql(&self, stmt: &str) -> Result<Vec<Value>> {
        let v = self.post("/api/query/sql", json!({ "stmt": stmt })).await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    pub async fn notebooks(&self) -> Result<Vec<Value>> {
        let v = self.post("/api/notebook/lsNotebooks", json!({})).await?;
        Ok(v
            .pointer("/notebooks")
            .and_then(|n| n.as_array())
            .cloned()
            .unwrap_or_default())
    }

    pub async fn ensure_notebook(&self, name: &str) -> Result<String> {
        for nb in self.notebooks().await? {
            if !nb.get("closed").and_then(|c| c.as_bool()).unwrap_or(false)
                && nb.get("name").and_then(|n| n.as_str()) == Some(name)
            {
                return Ok(nb.get("id").and_then(|i| i.as_str()).unwrap_or("").into());
            }
        }
        let v = self
            .post("/api/notebook/createNotebook", json!({ "name": name }))
            .await?;
        Ok(v
            .pointer("/notebook/id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .into())
    }

    /// Все доки по hPath; индекс ядра может отдать несколько id на путь (дубли).
    pub async fn doc_ids_by_hpath(&self, notebook: &str, hpath: &str) -> Result<Vec<String>> {
        let data = self
            .post(
                "/api/filetree/getIDsByHPath",
                json!({ "notebook": notebook, "path": hpath }),
            )
            .await?;
        Ok(data
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|i| i.as_str().map(String::from))
            .collect())
    }

    pub async fn create_doc_md(&self, notebook: &str, hpath: &str, markdown: &str) -> Result<String> {
        let v = self
            .post(
                "/api/filetree/createDocWithMd",
                json!({ "notebook": notebook, "path": hpath, "markdown": markdown }),
            )
            .await?;
        Ok(v.as_str().unwrap_or("").to_string())
    }

    pub async fn append_block_md(&self, doc_id: &str, markdown: &str) -> Result<()> {
        self.post(
            "/api/block/appendBlock",
            json!({ "dataType": "markdown", "data": markdown, "parentID": doc_id }),
        )
        .await?;
        Ok(())
    }

    /// docId неизменен: контент перезаписывается на месте (привязка строк-канбана живёт).
    /// Очистка детей верифицируется повторным get_children; пустой док ядра —
    /// один ZWJ-параграф (SiYuan не оставляет док без блока).
    pub async fn overwrite_doc_md(&self, doc_id: &str, markdown: &str) -> Result<()> {
        for _ in 0..3 {
            let text = self.block_children_text(doc_id).await?;
            let ids = parse_block_ids(&text);
            if doc_content_is_empty(&ids, &text) {
                if !markdown.trim().is_empty() {
                    self.append_block_md(doc_id, markdown).await?;
                    self.drop_leading_blank_block(doc_id).await;
                }
                return Ok(());
            }
            for block_id in ids {
                self.delete_block(&block_id).await?;
            }
        }
        bail!(
            "не удалось очистить контент дока {doc_id}: блоки остаются после {} волн удаления",
            3
        )
    }

    /// Пустой док ядра несёт ZWJ-заглушку; после записи контента в «пустой» док
    /// заглушка остаётся первым блоком — снять её, пока контент уже на месте.
    async fn drop_leading_blank_block(&self, doc_id: &str) {
        let Ok(text) = self.block_children_text(doc_id).await else { return };
        let ids = parse_block_ids(&text);
        if ids.len() >= 2 {
            let first_line = text.lines().next().unwrap_or("");
            if doc_content_is_empty(&ids[..1], first_line) {
                let _ = self.delete_block(&ids[0]).await;
            }
        }
    }

    async fn block_children_text(&self, doc_id: &str) -> Result<String> {
        self.mcp_tool_text("block", json!({ "action": "get_children", "id": doc_id }))
            .await
    }

    /// Блоки верхнего уровня дока (дети документ-блока). Через MCP block
    /// get_children, не SQL: индекс blocks отстаёт от мутаций.
    async fn top_level_block_ids(&self, doc_id: &str) -> Result<Vec<String>> {
        let text = self.block_children_text(doc_id).await?;
        Ok(parse_block_ids(&text))
    }

    pub async fn delete_block(&self, block_id: &str) -> Result<()> {
        self.post("/api/block/deleteBlock", json!({ "id": block_id })).await?;
        Ok(())
    }

    /// Экспорт дока в markdown по hPath (read-команды; задачи идут по docId).
    pub async fn export_md(&self, notebook: &str, hpath: &str) -> Result<String> {
        let id = self
            .doc_ids_by_hpath(notebook, hpath)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("док {hpath} не найден в ноутбуке {notebook}"))?;
        self.export_md_by_id(&id).await
    }

    /// Экспорт дока в markdown по docId.
    pub async fn export_md_by_id(&self, doc_id: &str) -> Result<String> {
        let v = self
            .post("/api/export/exportMdContent", json!({ "id": doc_id }))
            .await?;
        Ok(v
            .pointer("/content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// Полнотекстовый поиск по блокам.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Value>> {
        let stmt = format!(
            "select id, type, content, hpath, box from blocks \
             where content like '%{q}%' and type != 'l' limit {n}",
            q = query.replace('\'', "''"),
            n = limit
        );
        self.sql(&stmt).await
    }

    /// Скачать файл из воркспейса (assets/…).
    pub async fn download_file(&self, path: &str) -> Result<Vec<u8>> {
        let url = format!("{}/api/file/getFile?path={}", self.base, path.replace(' ', "%20"));
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Token {}", self.token))
            .send()
            .await
            .context("file request")?;
        if !resp.status().is_success() {
            bail!("getFile {path}: HTTP {}", resp.status());
        }
        Ok(resp.bytes().await?.to_vec())
    }

    // ---------- MCP: database (доски) ----------

    async fn mcp_call(&self, method: &str, params: Value) -> Result<Value> {
        let is_notification = method.starts_with("notifications/");
        let body = if is_notification {
            json!({ "jsonrpc": "2.0", "method": method, "params": params })
        } else {
            json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
        };
        let mut headers = vec![
            ("Authorization", format!("Token {}", self.token)),
            ("Content-Type", "application/json".into()),
            ("Accept", "application/json, text/event-stream".into()),
        ];
        {
            let guard = self.mcp_sid.lock().unwrap();
            if let Some(sid) = guard.as_ref() {
                headers.push(("Mcp-Session-Id", sid.clone()));
            }
        }
        let resp = self
            .client
            .post(format!("{}/mcp", self.base))
            .headers(
                headers
                    .iter()
                    .fold(reqwest::header::HeaderMap::new(), |mut m, (k, v)| {
                        if let (Ok(hk), Ok(hv)) = (
                            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                            reqwest::header::HeaderValue::from_str(v),
                        ) {
                            m.insert(hk, hv);
                        }
                        m
                    }),
            )
            .body(body.to_string())
            .send()
            .await
            .context("mcp request")?;
        if let Some(sid) = resp.headers().get("mcp-session-id") {
            *self.mcp_sid.lock().unwrap() = Some(sid.to_str()?.to_string());
        }
        let text = resp.text().await?;
        // streamable-http: последняя строка data:; пустое тело = нотификация (202)
        let payload = text
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .last()
            .unwrap_or(&text);
        if payload.trim().is_empty() {
            return Ok(Value::Null);
        }
        let v: Value = serde_json::from_str(payload).context("mcp response parse")?;
        if let Some(err) = v.get("error") {
            bail!("mcp {method}: {err}");
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    pub async fn mcp_initialize(&self) -> Result<Value> {
        let r = self
            .mcp_call(
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": { "name": "uwuwu-cli", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await?;
        let _ = self
            .mcp_call("notifications/initialized", json!({}))
            .await;
        Ok(r)
    }

    /// Ответ tools/call как исходный текст content[0] (markdown-инструменты: block).
    pub async fn mcp_tool_text(&self, name: &str, args: Value) -> Result<String> {
        self.ensure_mcp_session().await?;
        let r = self.mcp_call("tools/call", json!({ "name": name, "arguments": args })).await?;
        if r.get("isError").and_then(|e| e.as_bool()).unwrap_or(false) {
            bail!(
                "tool {name}: {}",
                r.pointer("/content/0/text").and_then(|t| t.as_str()).unwrap_or("?")
            );
        }
        Ok(r.pointer("/content/0/text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub async fn mcp_tool(&self, name: &str, args: Value) -> Result<Value> {
        // ленивая инициализация MCP-сессии при первом обращении
        self.ensure_mcp_session().await?;
        let r = self
            .mcp_call("tools/call", json!({ "name": name, "arguments": args }))
            .await?;
        if r.get("isError").and_then(|e| e.as_bool()).unwrap_or(false) {
            bail!(
                "tool {name}: {}",
                r.pointer("/content/0/text").and_then(|t| t.as_str()).unwrap_or("?")
            );
        }
        // structuredContent при наличии, иначе parse content[0].text
        if let Some(sc) = r.get("structuredContent") {
            return Ok(sc.clone());
        }
        let text = r.pointer("/content/0/text").and_then(|t| t.as_str()).unwrap_or("{}");
        Ok(serde_json::from_str(text).unwrap_or(Value::Null))
    }

    async fn ensure_mcp_session(&self) -> Result<()> {
        let need_init = {
            let guard = self.mcp_sid.lock().unwrap();
            guard.is_none()
        };
        if need_init {
            self.mcp_initialize().await?;
        }
        Ok(())
    }

    /// Список database-блоков (досок) по SQL.
    pub async fn boards(&self) -> Result<Vec<Value>> {
        self.sql(
            "select id, content, hpath, box from blocks \
             where type = 'query_embed' or markdown like '%action=\"query_embed\"%' limit 50",
        )
        .await
    }

    /// Поиск database по имени через MCP database search.
    pub async fn db_search(&self, keyword: &str) -> Result<Value> {
        self.mcp_tool("database", json!({ "action": "search", "keyword": keyword }))
            .await
    }

    /// Строки database (render): items с клетками; page/pageSize — ядро пагинирует (деф. 50).
    pub async fn db_render(&self, av_id: &str, block_id: &str, page: u32, page_size: u32) -> Result<Value> {
        self.mcp_tool(
            "database",
            json!({ "action": "render", "id": av_id, "blockID": block_id, "page": page, "pageSize": page_size }),
        )
        .await
    }

    pub async fn db_item_add(&self, av_id: &str, title: &str) -> Result<String> {
        let v = self
            .mcp_tool(
                "database",
                json!({ "action": "item_add", "id": av_id, "detached": true, "content": title }),
            )
            .await?;
        Ok(v
            .pointer("/data/itemID")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// Строка, привязанная к блоку-доку (blockID): клик по карточке открывает документ.
    pub async fn db_item_add_bound(&self, av_id: &str, block_id: &str) -> Result<String> {
        let v = self
            .mcp_tool(
                "database",
                json!({ "action": "item_add", "id": av_id, "blockID": block_id }),
            )
            .await?;
        Ok(v
            .pointer("/data/itemID")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub async fn db_item_set_status(&self, av_id: &str, key_id: &str, item_id: &str, status: &str) -> Result<()> {
        self.mcp_tool(
            "database",
            json!({ "action": "item_update", "id": av_id, "keyID": key_id,
                    "itemID": item_id, "value": { "mSelect": [{ "content": status }] } }),
        )
        .await?;
        Ok(())
    }
}


/// id блоков из markdown-дампа get_children (строки вида `- [p] текст (20261104-x)`).
/// id распознаётся по форме ядерного id: 14 цифр, дефис, 7 alnum.
pub(crate) fn parse_block_ids(children_text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in children_text.lines() {
        let Some(open) = line.rfind('(') else { continue };
        let candidate = line[open + 1..].trim_end_matches(')');
        let is_block_id = candidate.len() == 22
            && candidate[..14].chars().all(|c| c.is_ascii_digit())
            && candidate.as_bytes()[14] == b'-'
            && candidate[15..].chars().all(|c| c.is_ascii_alphanumeric());
        if is_block_id {
            ids.push(candidate.to_string());
        }
    }
    ids
}

/// Пустой с точки зрения ядра док: детей нет, либо один параграф без
/// контента/ZWJ (`- [p]  (id)`) — SiYuan не оставляет док без блока.
pub(crate) fn doc_content_is_empty(block_ids: &[String], children_text: &str) -> bool {
    let [only] = block_ids else {
        return block_ids.is_empty();
    };
    children_text
        .lines()
        .find(|line| line.contains(only.as_str()))
        .map(|line| {
            // `- [p] контент (id)` или `- [p] контент (id` — ядро отдаёт оба варианта
            let stripped = line
                .strip_suffix(&format!("({only})"))
                .or_else(|| line.strip_suffix(&format!("({only}")));
            let Some(prefix) = stripped else { return false };
            let prefix = prefix.trim_end();
            match prefix.strip_prefix("- [p]") {
                Some(body) => body.trim().chars().all(|c| c == '\u{200d}'),
                None => false,
            }
        })
        .unwrap_or(false)
}

impl SiYuan {
    /// Удалить док по id.
    pub async fn remove_doc(&self, doc_id: &str) -> Result<()> {
        self.post("/api/filetree/removeDocByID", json!({ "id": doc_id })).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{doc_content_is_empty, parse_block_ids};

    #[test]
    fn parse_block_ids_reads_trailing_kernel_ids() {
        let text = "- [p] первый (20261104130857-b9drqmf)\n- [h] ## Заголовок (20261104130919-on7yaie)\n";
        assert_eq!(parse_block_ids(text), vec!["20261104130857-b9drqmf", "20261104130919-on7yaie"]);
    }

    #[test]
    fn parse_block_ids_takes_block_id_from_multiline_content() {
        let text = "- [c] ```html_embed\n<div style=\"padding:12px\">x (не id)</div>\n``` (20261104130857-u3d7you)\n";
        assert_eq!(parse_block_ids(text), vec!["20261104130857-u3d7you"]);
    }

    #[test]
    fn doc_content_is_empty_accepts_id_without_closing_paren() {
        let bare = "- [p]  (20261104131411-bfha8c2";
        let ids = parse_block_ids(bare);
        assert_eq!(ids.len(), 1);
        assert!(doc_content_is_empty(&ids, bare));
    }

    #[test]
    fn parse_block_ids_skips_plain_text_parentheses() {
        let text = "- [p] скобки (не блок) в тексте\n";
        assert!(parse_block_ids(text).is_empty());
    }

    #[test]
    fn doc_content_is_empty_accepts_no_children_or_bare_paragraph() {
        assert!(doc_content_is_empty(&[], ""));
        let bare = "- [p]  (20261104131411-bfha8c2)";
        let ids = parse_block_ids(bare);
        assert_eq!(ids.len(), 1);
        assert!(doc_content_is_empty(&ids, bare));
        let with_text = "- [p] доп-строка (20261104131412-wbpz3sx)";
        let ids_text = parse_block_ids(with_text);
        assert!(!doc_content_is_empty(&ids_text, with_text));
        assert!(!doc_content_is_empty(
            &["20261104130857-b9drqmf".into(), "20261104131412-wbpz3sx".into()],
            "- a (20261104130857-b9drqmf)\n- b (20261104131412-wbpz3sx)\n"
        ));
    }
}
