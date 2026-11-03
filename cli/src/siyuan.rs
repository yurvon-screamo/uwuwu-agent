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

    /// Существование дока по hPath; возвращает docId, если найден.
    pub async fn doc_id_by_hpath(&self, notebook: &str, hpath: &str) -> Result<Option<String>> {
        let v = self
            .post(
                "/api/filetree/getIDsByHPath",
                json!({ "notebook": notebook, "path": hpath }),
            )
            .await;
        match v {
            Ok(data) => {
                let ids = data.as_array().cloned().unwrap_or_default();
                Ok(ids
                    .first()
                    .and_then(|i| i.as_str())
                    .map(|s| s.to_string()))
            }
            Err(_) => Ok(None),
        }
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

    /// Экспорт дока в markdown по hPath.
    pub async fn export_md(&self, notebook: &str, hpath: &str) -> Result<String> {
        let id = self
            .doc_id_by_hpath(notebook, hpath)
            .await?
            .ok_or_else(|| anyhow!("док {hpath} не найден в ноутбуке {notebook}"))?;
        let v = self
            .post("/api/export/exportMdContent", json!({ "id": id }))
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

    pub async fn mcp_tool(&self, name: &str, args: Value) -> Result<Value> {
        // ленивая инициализация MCP-сессии при первом обращении
        {
            let guard = self.mcp_sid.lock().unwrap();
            if guard.is_none() {
                drop(guard);
                self.mcp_initialize().await?;
            }
        }
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

    /// Строки database (render): items с клетками.
    pub async fn db_render(&self, av_id: &str, block_id: &str) -> Result<Value> {
        self.mcp_tool(
            "database",
            json!({ "action": "render", "id": av_id, "blockID": block_id }),
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

impl SiYuan {
    /// Удалить док по id.
    pub async fn remove_doc(&self, doc_id: &str) -> Result<()> {
        self.post("/api/filetree/removeDocByID", json!({ "id": doc_id })).await?;
        Ok(())
    }
}
