//! SiYuan backend: REST (Kernel API) + MCP (/mcp) клиент и операции
//! поверх смигрированного uwuwu-воркспейса.
//!
//! Конфигурация: UWUWU_SIYUAN_URL (деф. http://127.0.0.1:6806) и
//! UWUWU_SIYUAN_TOKEN; при пустом токене читается api.token из
//! $UWUWU_SIYUAN_WS/conf/conf.json (деф. ~/siyuan-demo-ws).

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

/// Результат чтения файла воркспейса через файловый API ядра.
#[derive(Clone, Debug, PartialEq)]
pub enum FileFetch {
    /// Файл существует, содержимое прочитано.
    Bytes(Vec<u8>),
    /// Файла нет (kernel отдал код 404 — os.Stat мимо, индекс не участвует).
    Missing,
    /// Проба не состоялась (транспорт/доступ) — состояние неизвестно.
    Failed(String),
}

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

    /// Тестовый конструктор: поля client/mcp_sid приватны для siyuan,
    /// сиблинг-модуль siyuan_task_tests соберёт только через него.
    #[cfg(test)]
    pub(crate) fn for_tests(base: String) -> Self {
        Self {
            base,
            token: "test-token".into(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("test reqwest client"),
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

    /// Первый док по hPath (для read-команд; дубли пути — легаси-ситуация).
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
                Ok(ids.first().and_then(|i| i.as_str()).map(|s| s.to_string()))
            }
            Err(_) => Ok(None),
        }
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

    pub async fn delete_block(&self, block_id: &str) -> Result<()> {
        self.post("/api/block/deleteBlock", json!({ "id": block_id })).await?;
        Ok(())
    }

    /// Экспорт дока в markdown по hPath. `notebook_id` — **ИД** ноутбука
    /// (формат 20261103224312-fbrf5fp, как возвращает `ensure_notebook`),
    /// не имя: kernel getIDsByHPath принимает только ID (см. `siyuan_id`).
    pub async fn export_md(&self, notebook_id: &str, hpath: &str) -> Result<String> {
        crate::siyuan_id::ensure_node_id(notebook_id, "notebook_id")?;
        let id = self
            .doc_id_by_hpath(notebook_id, hpath)
            .await?
            .ok_or_else(|| anyhow!("док {hpath} не найден в ноутбуке {notebook_id}"))?;
        self.export_md_by_id(&id).await
    }

    /// Экспорт дока в markdown по docId (exportMdContent).
    pub async fn export_md_by_id(&self, doc_id: &str) -> Result<String> {
        crate::siyuan_id::ensure_node_id(doc_id, "doc_id")?;
        let v = self
            .post("/api/export/exportMdContent", json!({ "id": doc_id }))
            .await?;
        Ok(v.pointer("/content")
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

    /// Скачать файл из воркспейса (assets/…). Путь относительно data/
    /// (конвенция blob-команды: `assets/<key>`).
    pub async fn download_file(&self, path: &str) -> Result<Vec<u8>> {
        match self.fetch_workspace_file(&format!("data/{path}")).await {
            FileFetch::Bytes(bytes) => Ok(bytes),
            FileFetch::Missing => bail!("getFile {path}: файла нет в data/"),
            FileFetch::Failed(err) => bail!("getFile {path}: {err}"),
        }
    }

    /// Чтение файла воркспейса, контракт ядра 3.8.9: POST JSON
    /// `{"path": …}` от корня воркспейса. 200 = сырые байты файла
    /// (os.Stat ядра, индекс не участвует); код 404 = файла нет; прочее
    /// или транспортная ошибка = Failed (проба не состоялась).
    /// Без ретраев post(): проба живости должна быть быстрой и честной.
    pub async fn fetch_workspace_file(&self, path: &str) -> FileFetch {
        let resp = self
            .client
            .post(format!("{}/api/file/getFile", self.base))
            .header("Authorization", format!("Token {}", self.token))
            .json(&json!({ "path": path }))
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => return FileFetch::Failed(e.to_string()),
        };
        match resp.status().as_u16() {
            200 => match resp.bytes().await {
                Ok(bytes) => FileFetch::Bytes(bytes.to_vec()),
                Err(e) => FileFetch::Failed(e.to_string()),
            },
            status => {
                let envelope: Value = serde_json::from_slice(&resp.bytes().await.unwrap_or_default())
                    .unwrap_or(Value::Null);
                if envelope.get("code").and_then(|c| c.as_i64()) == Some(404) {
                    FileFetch::Missing
                } else {
                    FileFetch::Failed(format!("HTTP {status}: {envelope}"))
                }
            }
        }
    }

    /// Запись файла в воркспейс, контракт ядра 3.8.9: POST multipart
    /// (`path` от корня воркспейса + поле file). Ядро само создаёт
    /// недостающие директории.
    pub async fn put_workspace_file(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let filename = path.rsplit('/').next().unwrap_or(path).to_string();
        let form = reqwest::multipart::Form::new()
            .text("path", path.to_string())
            .part(
                "file",
                reqwest::multipart::Part::bytes(bytes.to_vec()).file_name(filename),
            );
        let resp = self
            .client
            .post(format!("{}/api/file/putFile", self.base))
            .header("Authorization", format!("Token {}", self.token))
            .multipart(form)
            .send()
            .await
            .context("putFile request")?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if status.is_success() && body.get("code").and_then(|c| c.as_i64()) == Some(0) {
            return Ok(());
        }
        bail!("putFile {path}: HTTP {status}, {}", body)
    }

    /// Список файлов директории воркспейса (POST JSON, контракт 3.8.9),
    /// путь от корня воркспейса. Несуществующая директория — пустой список.
    pub async fn list_dir(&self, path: &str) -> Result<Vec<String>> {
        let data = self
            .post("/api/file/readDir", json!({ "path": path }))
            .await?;
        Ok(data
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| entry.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect())
    }

    /// FS-путь файла дока: data/<ноутбук>/<предки…>/<docId>.sy — ядро
    /// хранит дерево по блок-id, не по hpath (проба не зависит от индекса
    /// и тайтл-санитизации).
    pub fn doc_file_path(notebook_id: &str, ancestor_docs: &[&str], doc_id: &str) -> String {
        let mut path = format!("data/{notebook_id}");
        for ancestor in ancestor_docs {
            path.push('/');
            path.push_str(ancestor);
        }
        format!("{path}/{doc_id}.sy")
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

    /// Строки database (render): items с клетками; page с 1 (серверный
    /// pageSize — 50, отсюда пагинация).
    pub async fn db_render(&self, av_id: &str, block_id: &str, page: u64) -> Result<Value> {
        self.mcp_tool(
            "database",
            json!({ "action": "render", "id": av_id, "blockID": block_id, "page": page }),
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
    use super::{doc_content_is_empty, parse_block_ids, FileFetch};
    use crate::siyuan::SiYuan;
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

    #[test]
    fn doc_file_path_joins_notebook_ancestors_and_doc_id() {
        let got = SiYuan::doc_file_path("20261103224312-fbrf5fp", &["20261103233646-vzgad8m"], "20261104215722-1p0eetv");
        assert_eq!(got, "data/20261103224312-fbrf5fp/20261103233646-vzgad8m/20261104215722-1p0eetv.sy");
        assert_eq!(
            SiYuan::doc_file_path("nb-1", &[], "20261103224557-4yurg8d"),
            "data/nb-1/20261103224557-4yurg8d.sy"
        );
    }

    #[tokio::test]
    async fn fetch_workspace_file_reads_raw_bytes_on_http_200() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/file/getFile"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let wants = body.get("path").and_then(|p| p.as_str()) == Some("data/nb/doc.sy");
                let (status, payload) = if wants {
                    (200, b"{\"ID\":\"doc\",\"Type\":\"NodeDocument\"}".to_vec())
                } else {
                    (202, br#"{"code":404,"msg":"file does not exist"}"#.to_vec())
                };
                ResponseTemplate::new(status).set_body_bytes(payload)
            })
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        assert_eq!(sy.fetch_workspace_file("data/nb/doc.sy").await, FileFetch::Bytes(b"{\"ID\":\"doc\",\"Type\":\"NodeDocument\"}".to_vec()));
        assert_eq!(sy.fetch_workspace_file("data/nb/other.sy").await, FileFetch::Missing);
    }

    #[tokio::test]
    async fn fetch_workspace_file_reports_failure_on_transport_error() {
        // несуществующий порт = соединение не состоялось → Failed, не Missing
        let sy = SiYuan::for_tests("http://127.0.0.1:9".into());
        assert!(matches!(sy.fetch_workspace_file("data/nb/doc.sy").await, FileFetch::Failed(_)));
    }

    #[tokio::test]
    async fn put_workspace_file_roundtrips_via_get_file() {
        let server = MockServer::start().await;
        let stored: Arc<std::sync::Mutex<Vec<u8>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stored_put = stored.clone();
        Mock::given(method("POST"))
            .and(path("/api/file/putFile"))
            .respond_with(move |req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body).to_string();
                let ok = body.contains("name=\"path\"") && body.contains("data/assets/undo/j.json");
                if ok {
                    *stored_put.lock().unwrap() = req.body.clone();
                }
                ResponseTemplate::new(200).set_body_string(if ok { r#"{"code":0,"msg":"","data":null}"# } else { r#"{"code":-1}"# })
            })
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/file/getFile"))
            .respond_with(move |_req: &wiremock::Request| {
                let bytes = stored.lock().unwrap().clone();
                ResponseTemplate::new(200).set_body_bytes(bytes)
            })
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        sy.put_workspace_file("data/assets/undo/j.json", b"{\"k\":1}").await.unwrap();
        let read_back = sy.fetch_workspace_file("data/assets/undo/j.json").await;
        assert!(matches!(read_back, FileFetch::Bytes(_)));
        if let FileFetch::Bytes(bytes) = read_back {
            assert!(bytes.windows(7).any(|w| w == b"{\"k\":1}"), "контент в multipart-теле: {bytes:?}");
        }
    }

    #[tokio::test]
    async fn list_dir_returns_entry_names() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/file/readDir"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let data = if body.get("path").and_then(|p| p.as_str()) == Some("data/assets/undo") {
                    serde_json::json!([
                        { "name": "20261104-clean.json", "isDir": false, "isSymlink": false, "updated": 1 },
                        { "name": "sub", "isDir": true, "isSymlink": false, "updated": 1 }
                    ])
                } else {
                    serde_json::json!([])
                };
                ResponseTemplate::new(200).set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
            })
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        let names = sy.list_dir("data/assets/undo").await.unwrap();
        assert_eq!(names, vec!["20261104-clean.json".to_string(), "sub".to_string()]);
        assert!(sy.list_dir("data/assets/nowhere").await.unwrap().is_empty());
    }
}
