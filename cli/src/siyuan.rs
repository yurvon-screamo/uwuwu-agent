//! SiYuan backend: REST (Kernel API) + MCP (/mcp) клиент и операции
//! поверх смигрированного uwuwu-воркспейса.
//!
//! Конфигурация: UWUWU_SIYUAN_URL (деф. http://127.0.0.1:6806) и
//! UWUWU_SIYUAN_TOKEN; при пустом токене читается api.token из
//! $UWUWU_SIYUAN_WS/conf/conf.json (деф. ~/siyuan-demo-ws).

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Ядро в волне деградации индекса: последующие вызовы post() падают сразу,
/// не долбя ядро. Волна кормится ретраями (репро 04.11: серии с шагом ~2–3с —
/// это post()-ретраи), поэтому после финального bail окно держит от повторных
/// попыток. Поле инстанса, а не процесс-статик: CLI = один инстанс на процесс,
/// семантика та же (внутрипроцессная), а тесты изолированы структурно —
/// герметичность свиты не зависит от порядка/параллельности.
const WAVE_COOLDOWN_SECS: u64 = 90;
/// Ретраи на «metadata is not ready»: начальная попытка + до двух ретраев
/// (штатный лаг индекса после крупных вставок пробивается вторым-третьим),
/// затем bail с хинтом — не 4×~10с как прежде.
const METADATA_RETRY_BACKOFFS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(5)];
const WAVE_HINT: &str = "ядро в волне деградации индекса: пауза 2–5 минут, затем `uwuwu-cli doctor`; при повторении — docker restart siyuan-demo. Сразу не ретраить — волна кормится ретраями";

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Паузы ретраев post(): `metadata_backoffs` — паузы на «metadata is not ready»
/// (исчерпание = bail с хинтом + окно волны), `other_step_ms` — шаг прочих
/// ошибок (800·(n+1)ms, как прежде; окно НЕ армится).
#[derive(Clone)]
struct RetryPacing {
    metadata_backoffs: Vec<Duration>,
    other_step_ms: u64,
}

impl RetryPacing {
    fn production() -> Self {
        Self {
            metadata_backoffs: METADATA_RETRY_BACKOFFS.to_vec(),
            other_step_ms: 800,
        }
    }

    #[cfg(test)]
    fn instant() -> Self {
        Self {
            metadata_backoffs: vec![Duration::from_millis(5), Duration::from_millis(5)],
            other_step_ms: 1,
        }
    }
}

/// Результат чтения файла воркспейса через файловый API ядра.
#[derive(Clone, Debug, PartialEq)]
pub enum FileFetch {
    /// Файл существует, содержимое прочитано.
    Bytes(Vec<u8>),
    /// Файла нет (kernel отдал код 404 — прямое чтение ФС мимо, индекс не участвует).
    Missing,
    /// Проба не состоялась (транспорт/доступ) — состояние неизвестно.
    Failed(String),
}

/// Загруженный ассет: оригинальное имя файла и финальный путь в assets/,
/// сгенерированный ядром (`<stem>-<timestamp>-<random>.<ext>`).
#[derive(Debug)]
pub struct UploadedAsset {
    pub original_name: String,
    pub asset_path: String,
}

pub struct SiYuan {
    pub base: String,
    pub token: String,
    client: reqwest::Client,
    // MCP-сессия (ленивая)
    mcp_sid: std::sync::Mutex<Option<String>>,
    /// 90s fail-fast окно волны (unix-secs истечения; 0 = не армировано).
    wave_cooldown_until: AtomicU64,
    /// Инъекция мгновенного пейсинга ретраев для тестов (RowInsertPacing-прецедент
    /// PR #125): None в проде — production-константы в post().
    test_pacing: Option<RetryPacing>,
}

/// Корень воркспейса (FS-пробы doctor'а, конфиг-фоллбек токена):
/// UWUWU_SIYUAN_WS, иначе ~/siyuan-demo-ws.
pub(crate) fn workspace_root() -> PathBuf {
    std::env::var("UWUWU_SIYUAN_WS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("siyuan-demo-ws")
        })
}

impl SiYuan {
    pub fn from_env() -> Result<Self> {
        Self::from_env_with_timeout(120)
    }

    /// Конфигурация from_env с другим таймаутом клиента: kernel-пробы зовутся
    /// с коротким (деф. 10s), чтобы тихий вис ядра различался быстро;
    /// штатные команды держат 120s.
    pub fn from_env_with_timeout(timeout_secs: u64) -> Result<Self> {
        let base =
            std::env::var("UWUWU_SIYUAN_URL").unwrap_or_else(|_| "http://127.0.0.1:6806".into());
        let token = match std::env::var("UWUWU_SIYUAN_TOKEN") {
            Ok(t) if !t.is_empty() => t,
            _ => {
                let ws = workspace_root();
                let conf: Value = serde_json::from_str(
                    &std::fs::read_to_string(ws.join("conf/conf.json")).with_context(|| {
                        format!(
                            "нет conf/conf.json в {} (задай UWUWU_SIYUAN_TOKEN)",
                            ws.display()
                        )
                    })?,
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
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(timeout_secs))
                .build()?,
            mcp_sid: std::sync::Mutex::new(None),
            wave_cooldown_until: AtomicU64::new(0),
            test_pacing: None,
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
            wave_cooldown_until: AtomicU64::new(0),
            test_pacing: Some(RetryPacing {
                metadata_backoffs: vec![Duration::from_millis(5), Duration::from_millis(5)],
                other_step_ms: 1,
            }),
        }
    }

    fn arm_wave_cooldown(&self) {
        self.wave_cooldown_until
            .store(now_unix_secs() + WAVE_COOLDOWN_SECS, Ordering::Relaxed);
    }

    fn wave_cooldown_active(&self) -> bool {
        self.wave_cooldown_until.load(Ordering::Relaxed) > now_unix_secs()
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let pacing = self
            .test_pacing
            .clone()
            .unwrap_or_else(RetryPacing::production);
        self.post_with_pacing(path, body, pacing).await
    }

    /// post() с инъекцией ретрай-пауз — тесты зовут с мгновенным пейсингом,
    /// штатный post() с production-константами выше.
    /// Прочие ошибки (транспорт, code≠0) ретраются как прежде, 4 попытки,
    /// и окно волны НЕ армят: транспортный отказ — класс режимов 1–3
    /// (pasta/kernel-down), гасится ops-процедурами, не клиентом.
    async fn post_with_pacing(
        &self,
        path: &str,
        body: Value,
        pacing: RetryPacing,
    ) -> Result<Value> {
        if self.wave_cooldown_active() {
            bail!("POST {path}: пропущено — {WAVE_HINT} (окно fail-fast активно)");
        }
        let mut last = String::new();
        let mut metadata_attempts = 0usize;
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
                        match pacing.metadata_backoffs.get(metadata_attempts) {
                            Some(backoff) => {
                                metadata_attempts += 1;
                                tokio::time::sleep(*backoff).await;
                                continue;
                            }
                            None => {
                                self.arm_wave_cooldown();
                                bail!("POST {path}: {last} — {WAVE_HINT}");
                            }
                        }
                    }
                }
                Err(e) => last = e.to_string(),
            }
            tokio::time::sleep(std::time::Duration::from_millis(
                pacing.other_step_ms * (attempt as u64 + 1),
            ))
            .await;
        }
        bail!("POST {path}: {last}")
    }

    /// One-shot POST без ретраев: полный конверт ядра `{code,msg,data}`.
    /// Проба должна быть быстрой и честной — сырое сообщение ядра видно
    /// на первом ответе (штатный post() маскирует сбой 4 ретраями с бэкоффом).
    /// Контракт ошибок: транспорт → контекст; тело не JSON → bail со
    /// сниппетом; HTTP не-success → статус + тело (auth-отказ ядра:
    /// 401 + `{"code":-1,"msg":"Auth failed…"}`); конверт code≠0 → code/msg.
    pub async fn post_probe(&self, path: &str, body: Value) -> Result<Value> {
        let resp = self
            .client
            .post(format!("{}{}", self.base, path))
            .header("Authorization", format!("Token {}", self.token))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("kernel {path}: транспорт недоступен"))?;
        let status = resp.status();
        let raw = resp.text().await.unwrap_or_default();
        let envelope: Value = serde_json::from_str(&raw).with_context(|| {
            format!(
                "kernel {path}: HTTP {status}, тело не JSON: {}",
                body_snippet(&raw)
            )
        })?;
        if !status.is_success() {
            bail!("kernel {path}: HTTP {status}, {}", body_snippet(&raw));
        }
        let code = envelope.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            bail!(
                "kernel {path}: code={} msg={}",
                code,
                envelope.get("msg").and_then(|m| m.as_str()).unwrap_or("?")
            );
        }
        Ok(envelope)
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
        Ok(v.pointer("/notebooks")
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
        Ok(v.pointer("/notebook/id")
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

    pub async fn create_doc_md(
        &self,
        notebook: &str,
        hpath: &str,
        markdown: &str,
    ) -> Result<String> {
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

    /// Одноразовый appendBlock без слепых ретраев: неидемпотентная запись при
    /// потерянном ответе на ретрае дублируется — контрольный повтор делает
    /// вызывающий код (confirm-чтение), транспорт честно сообщает один сбой.
    /// Wave-гейт как у post(): окно fail-fast не пробивается записью.
    pub async fn append_block_md_once(&self, doc_id: &str, markdown: &str) -> Result<()> {
        if self.wave_cooldown_active() {
            bail!("POST /api/block/appendBlock: пропущено — {WAVE_HINT} (окно fail-fast активно)");
        }
        self.post_probe(
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
        let Ok(text) = self.block_children_text(doc_id).await else {
            return;
        };
        let ids = parse_block_ids(&text);
        if ids.len() >= 2 {
            let first_line = text.lines().next().unwrap_or("");
            if doc_content_is_empty(&ids[..1], first_line) {
                let _ = self.delete_block(&ids[0]).await;
            }
        }
    }

    pub(crate) async fn block_children_text(&self, doc_id: &str) -> Result<String> {
        self.mcp_tool_text("block", json!({ "action": "get_children", "id": doc_id }))
            .await
    }

    pub async fn delete_block(&self, block_id: &str) -> Result<()> {
        self.post("/api/block/deleteBlock", json!({ "id": block_id }))
            .await?;
        Ok(())
    }

    /// Заменить содержимое блока (тип сохраняется: html-блок остаётся html).
    pub async fn update_block_md(&self, block_id: &str, markdown: &str) -> Result<()> {
        self.post(
            "/api/block/updateBlock",
            json!({ "dataType": "markdown", "data": markdown, "id": block_id }),
        )
        .await?;
        Ok(())
    }

    /// Вставить markdown-блоки сразу после блока previous_id.
    pub async fn insert_block_md_after(&self, previous_id: &str, markdown: &str) -> Result<()> {
        self.post(
            "/api/block/insertBlock",
            json!({
                "dataType": "markdown",
                "data": markdown,
                "previousID": previous_id,
            }),
        )
        .await?;
        Ok(())
    }

    /// Расположение .sy-файла дока в воркспейсе: (box, path от data/) —
    /// FS-истина порядка детей (индекс ядра порядок не отдаёт).
    pub async fn doc_file_location(&self, doc_id: &str) -> Result<(String, String)> {
        let v = self
            .post("/api/block/getBlockInfo", json!({ "id": doc_id }))
            .await?;
        let box_id = v
            .pointer("/box")
            .and_then(|b| b.as_str())
            .ok_or_else(|| anyhow!("getBlockInfo {doc_id}: нет box в ответе {v}"))?
            .to_string();
        let path = v
            .pointer("/path")
            .and_then(|p| p.as_str())
            .ok_or_else(|| anyhow!("getBlockInfo {doc_id}: нет path в ответе {v}"))?
            .to_string();
        Ok((box_id, path))
    }

    /// Упорядоченные дети дока верхнего уровня из .sy-файла ( Children массив
    /// в порядке документа). Нужен для адресации «блок после маркера»:
    /// SQL-индекс порядок детей не хранит, kramdown-пропы html-блоков
    /// недокументированы — .sy детерминирован (NodeHTMLBlock).
    pub(crate) async fn doc_children(&self, doc_id: &str) -> Result<Vec<DocChild>> {
        let (box_id, path) = self.doc_file_location(doc_id).await?;
        let bytes = match self
            .fetch_workspace_file(&format!("data/{box_id}{path}"))
            .await
        {
            FileFetch::Bytes(bytes) => bytes,
            FileFetch::Missing => bail!("{}: .sy дока нет в data/ (box={box_id})", doc_id),
            FileFetch::Failed(err) => bail!("getFile {doc_id}: {err}"),
        };
        let tree: Value =
            serde_json::from_slice(&bytes).with_context(|| format!("{}: .sy не JSON", doc_id))?;
        Ok(doc_children_from_sy(&tree))
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

    /// Экспорт дока в markdown по docId (exportMdContent). Выход санитизируется
    /// от zero-width-маркеров ядра (read-choke-point всех контент-потребителей);
    /// сырая kernel-проба `kernel export` идёт мимо — через post_probe.
    pub async fn export_md_by_id(&self, doc_id: &str) -> Result<String> {
        crate::siyuan_id::ensure_node_id(doc_id, "doc_id")?;
        let v = self
            .post("/api/export/exportMdContent", json!({ "id": doc_id }))
            .await?;
        let content = v
            .pointer("/content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        Ok(crate::md_artifacts::strip_zero_width(&content))
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

    /// hpath всех доков воркспейса с явным потолком. Заголовки (hpath)
    /// контентный like не матчит — их резолвит doc-секция search.
    /// Явный LIMIT обязателен: без него ядро капает выдачу на 64 строки
    /// (проба 07.11: 1642 дока, truncated:true; limit 2000 — полный состав).
    pub async fn doc_titles(&self, cap: usize) -> Result<Vec<String>> {
        let rows = self
            .sql(&format!(
                "select hpath from blocks where type='d' limit {cap}"
            ))
            .await?;
        Ok(rows
            .iter()
            .filter_map(|row| row.get("hpath").and_then(|h| h.as_str()))
            .map(str::to_string)
            .collect())
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
    /// (ядро читает файл с диска, индекс не участвует); код 404 = файла нет; прочее
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
                let envelope: Value =
                    serde_json::from_slice(&resp.bytes().await.unwrap_or_default())
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

    /// Upload файла в assets/ воркспейса, контракт ядра 3.8.9: POST multipart
    /// `/api/asset/upload` (`assetsDir` + `file[]`). Ровно один файл за вызов —
    /// succMap ключуется оригинальным именем, два файла с одним basename в
    /// одном вызове конфликтуют ключами. Уникальность финального имени даёт
    /// ядро, своей naming-логики нет. One-shot (как putFile): сбой громкий.
    pub async fn upload_asset(&self, file: &std::path::Path) -> Result<UploadedAsset> {
        let bytes = std::fs::read(file)
            .with_context(|| format!("чтение файла {}: недоступен", file.display()))?;
        let original_name = file
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from)
            .with_context(|| format!("имя файла {} не UTF-8", file.display()))?;
        let form = reqwest::multipart::Form::new()
            .text("assetsDir", "assets".to_string())
            .part(
                "file[]",
                reqwest::multipart::Part::bytes(bytes).file_name(original_name.clone()),
            );
        let resp = self
            .client
            .post(format!("{}/api/asset/upload", self.base))
            .header("Authorization", format!("Token {}", self.token))
            .multipart(form)
            .send()
            .await
            .context("asset upload: транспорт недоступен")?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() || body.get("code").and_then(|c| c.as_i64()) != Some(0) {
            bail!("asset upload {original_name}: HTTP {status}, {body}");
        }
        let asset_path = body
            .pointer("/data/succFiles/0/path")
            .and_then(|p| p.as_str())
            .map(String::from)
            .with_context(|| {
                format!("asset upload {original_name}: ядро не вернуло succFiles: {body}")
            })?;
        Ok(UploadedAsset {
            original_name,
            asset_path,
        })
    }

    /// Переименовать док (kernel renameDoc): меняет тайтл дока и hpath-лист,
    /// storage-путь (id-цепочка) и blockID неизменны — привязка строк канбанов
    /// живёт. `path` — storage-путь ОТНОСИТЕЛЬНО КОРНЯ НОУТБУКА
    /// (`/предки…/<docId>.sy`); kernel сам префиксует `data/<notebook>/`.
    /// Ретраи — штатный post() (metadata-волна и транспорт).
    pub async fn rename_doc(&self, notebook: &str, path: &str, title: &str) -> Result<()> {
        self.post(
            "/api/filetree/renameDoc",
            json!({ "notebook": notebook, "path": path, "title": title }),
        )
        .await
        .map(|_| ())
    }

    /// Атрибуты блока (IAL): /api/attr/getBlockAttrs. Кастом-атрибуты
    /// (`custom-*`) — носитель расписания крон-задач; чтение мгновенное
    /// (SQL-колонка ial отстаёт от свежих записей атрибутов).
    pub async fn get_block_attrs(&self, block_id: &str) -> Result<HashMap<String, String>> {
        let data = self
            .post("/api/attr/getBlockAttrs", json!({ "id": block_id }))
            .await?;
        let map = data
            .as_object()
            .ok_or_else(|| anyhow!("attr getBlockAttrs {block_id}: ожидался объект"))?;
        Ok(map
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect())
    }

    /// Записать атрибут блока (/api/attr/setBlockAttrs). Пустое значение
    /// снимает атрибут (ядро удаляет ключ из IAL — проба 07.11).
    pub async fn set_block_attr(&self, block_id: &str, name: &str, value: &str) -> Result<()> {
        self.post(
            "/api/attr/setBlockAttrs",
            json!({ "id": block_id, "attrs": { name: value } }),
        )
        .await
        .map(|_| ())
    }

    /// Список файлов директории воркспейса (POST JSON, контракт 3.8.9),
    /// путь от корня воркспейса. Несуществующая директория — честная ошибка
    /// ядра (code=404 «path does not exist», проба ls 05.11); benign-консьюмерам
    /// (undo --list на фреш-воркспейсе) — try_list_dir.
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

    /// Список файлов директории с таксономией отсутствия: None — директории
    /// нет (ядро: code=404 «path does not exist», проба ls 05.11), Err —
    /// остальные сбои (транспорт/не JSON/code≠0). One-shot, без ретраев:
    /// benign-чтение не должно молотить волну.
    pub async fn try_list_dir(&self, path: &str) -> Result<Option<Vec<String>>> {
        let resp = self
            .client
            .post(format!("{}/api/file/readDir", self.base))
            .header("Authorization", format!("Token {}", self.token))
            .json(&json!({ "path": path }))
            .send()
            .await
            .context("kernel /api/file/readDir: транспорт недоступен")?;
        let status = resp.status();
        let envelope: Value = resp.json().await.unwrap_or(Value::Null);
        let code = envelope.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code == 404 {
            return Ok(None);
        }
        if !status.is_success() || code != 0 {
            bail!(
                "kernel /api/file/readDir: HTTP {status}, code={code} msg={}",
                envelope.get("msg").and_then(|m| m.as_str()).unwrap_or("?")
            );
        }
        Ok(Some(
            envelope
                .get("data")
                .and_then(|d| d.as_array())
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|entry| entry.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect(),
        ))
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
            .rev()
            .find_map(|l| l.strip_prefix("data: "))
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
        let _ = self.mcp_call("notifications/initialized", json!({})).await;
        Ok(r)
    }

    /// Ответ tools/call как исходный текст content[0] (markdown-инструменты: block).
    pub async fn mcp_tool_text(&self, name: &str, args: Value) -> Result<String> {
        self.ensure_mcp_session().await?;
        let r = self
            .mcp_call("tools/call", json!({ "name": name, "arguments": args }))
            .await?;
        if r.get("isError").and_then(|e| e.as_bool()).unwrap_or(false) {
            bail!(
                "tool {name}: {}",
                r.pointer("/content/0/text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("?")
            );
        }
        Ok(r.pointer("/content/0/text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// Логический фейл шлюза/ядра внутри успешного MCP-ответа (HTTP 200,
    /// isError:false) не должен проходить как данные: ok=false (конвенция
    /// шлюза AFFiNE, ADR-006) или ядерный конверт code≠0 — это ошибка
    /// операции. require_ok-барьер AFFiNE-эры (PR #17), перенесённый на
    /// единый вход SiYuan-клиента.
    fn require_payload_ok(tool: &str, payload: &Value) -> Result<()> {
        if payload.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            let msg = payload
                .get("msg")
                .or_else(|| payload.get("error"))
                .and_then(|m| m.as_str())
                .unwrap_or("?");
            bail!("tool {tool}: ok=false: {msg}");
        }
        match payload.get("code").and_then(|c| c.as_i64()) {
            Some(0) | None => Ok(()),
            Some(code) => {
                let msg = payload.get("msg").and_then(|m| m.as_str()).unwrap_or("?");
                bail!("tool {tool}: code={code}: {msg}");
            }
        }
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
                r.pointer("/content/0/text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("?")
            );
        }
        // structuredContent при наличии, иначе parse content[0].text;
        // оба канала проходят барьер логического фейла
        if let Some(sc) = r.get("structuredContent") {
            Self::require_payload_ok(name, sc)?;
            return Ok(sc.clone());
        }
        let text = r
            .pointer("/content/0/text")
            .and_then(|t| t.as_str())
            .unwrap_or("{}");
        let parsed: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        Self::require_payload_ok(name, &parsed)?;
        Ok(parsed)
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
        self.mcp_tool(
            "database",
            json!({ "action": "search", "keyword": keyword }),
        )
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
        Ok(v.pointer("/data/itemID")
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
        Ok(v.pointer("/data/itemID")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub async fn db_item_set_status(
        &self,
        av_id: &str,
        key_id: &str,
        item_id: &str,
        status: &str,
    ) -> Result<()> {
        self.mcp_tool(
            "database",
            json!({ "action": "item_update", "id": av_id, "keyID": key_id,
                    "itemID": item_id, "value": { "mSelect": [{ "content": status }] } }),
        )
        .await?;
        Ok(())
    }
}

/// Голова тела ответа для сообщений об ошибках: диагностика без простыни.
fn body_snippet(raw: &str) -> String {
    const SNIPPET_CHARS: usize = 200;
    let mut head: String = raw.chars().take(SNIPPET_CHARS).collect();
    if raw.chars().count() > SNIPPET_CHARS {
        head.push('…');
    }
    head
}

/// Верхнеуровневые дети дока из .sy в порядке документа — минимум для
/// адресации диаграмм по маркеру.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DocChild {
    pub(crate) id: String,
    pub(crate) kind: DocChildKind,
    /// Текст абзаца/заголовка (для html-блока — сам html).
    pub(crate) text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocChildKind {
    Paragraph,
    Heading,
    Html,
    Other,
}

/// Парсер Children массива .sy-дока (проба ядра 3.8.10: NodeParagraph /
/// NodeHeading несут текст во вложенных NodeText.Data, NodeHTMLBlock —
/// html в собственном Data).
pub(crate) fn doc_children_from_sy(tree: &Value) -> Vec<DocChild> {
    tree.get("Children")
        .and_then(|c| c.as_array())
        .map(|children| {
            children
                .iter()
                .filter_map(|node| {
                    let id = node.get("ID")?.as_str()?.to_string();
                    let kind = match node.get("Type").and_then(|t| t.as_str()) {
                        Some("NodeParagraph") => DocChildKind::Paragraph,
                        Some("NodeHeading") => DocChildKind::Heading,
                        Some("NodeHTMLBlock") => DocChildKind::Html,
                        _ => DocChildKind::Other,
                    };
                    let text = match kind {
                        DocChildKind::Html | DocChildKind::Other => node
                            .get("Data")
                            .and_then(|d| d.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        DocChildKind::Paragraph | DocChildKind::Heading => node
                            .get("Children")
                            .and_then(|c| c.as_array())
                            .map(|texts| {
                                texts
                                    .iter()
                                    .filter(|t| {
                                        t.get("Type").and_then(|v| v.as_str()) == Some("NodeText")
                                    })
                                    .filter_map(|t| t.get("Data").and_then(|d| d.as_str()))
                                    .collect::<Vec<_>>()
                                    .join("")
                            })
                            .unwrap_or_default(),
                    };
                    Some(DocChild { id, kind, text })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Адрес диаграммы: маркер-абзац `⁂ diagram:<имя>` и следующий блок.
/// Маркеров с одним именем несколько → первое вхождение (детерминизм,
/// как в replace_md_section); сосед маркера — любой вид, is_html решает
/// операцию (update/delete против insert-after).
pub(crate) fn find_marker_location(
    children: &[DocChild],
    marker_prefix: &str,
    name: &str,
) -> Option<crate::html_embed::MarkerLocation> {
    let marker_text = format!("{marker_prefix}{name}");
    let pos = children
        .iter()
        .position(|c| c.kind == DocChildKind::Paragraph && c.text == marker_text)?;
    let following = children
        .get(pos + 1)
        .map(|next| crate::html_embed::FollowingBlock {
            id: next.id.clone(),
            is_html: next.kind == DocChildKind::Html,
        });
    Some(crate::html_embed::MarkerLocation {
        marker_id: children[pos].id.clone(),
        following,
    })
}

/// id блоков из markdown-дампа get_children (строки вида `- [p] текст (20261104-x)`).
/// id распознаётся по форме ядерного id: 14 цифр, дефис, 7 alnum.
pub(crate) fn parse_block_ids(children_text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in children_text.lines() {
        let Some(open) = line.rfind('(') else {
            continue;
        };
        let candidate = line[open + 1..].trim_end_matches(')');
        // Кандидат — произвольный текст после последней '('; срезы по байтам
        // паникуют на не-ASCII (репро doctor'ом: 22-байтный текст с кириллицей).
        let is_block_id = candidate.len() == 22
            && candidate.is_char_boundary(14)
            && candidate.is_char_boundary(15)
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
        self.post("/api/filetree/removeDocByID", json!({ "id": doc_id }))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{body_snippet, doc_content_is_empty, parse_block_ids, FileFetch, RetryPacing};
    use crate::siyuan::SiYuan;
    use serde_json::json;
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn metadata_error_template() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(
            json!({"code": -1, "msg": "commit tx failed: document metadata is not ready"}),
        )
    }

    #[tokio::test]
    async fn metadata_not_ready_bails_after_three_requests_with_wave_hint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/block/appendBlock"))
            .respond_with(metadata_error_template())
            .expect(3)
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .post_with_pacing(
                "/api/block/appendBlock",
                json!({"id": "x"}),
                RetryPacing::instant(),
            )
            .await
            .expect_err("persistent metadata error must bail");

        let text = err.to_string();
        assert!(text.contains("metadata is not ready"));
        assert!(text.contains("uwuwu-cli doctor"));
    }

    #[tokio::test]
    async fn transient_metadata_lag_recovers_on_retry_without_arming_cooldown() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/block/appendBlock"))
            .respond_with(metadata_error_template())
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/block/appendBlock"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code": 0, "data": ""})))
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        sy.post_with_pacing(
            "/api/block/appendBlock",
            json!({"id": "x"}),
            RetryPacing::instant(),
        )
        .await
        .expect("retry after transient lag must succeed");

        // Окно не армировано: следующий вызов доходит до мока.
        sy.post_with_pacing(
            "/api/block/appendBlock",
            json!({"id": "x"}),
            RetryPacing::instant(),
        )
        .await
        .expect("cooldown must stay disarmed after successful retry");
    }

    #[tokio::test]
    async fn wave_cooldown_fails_fast_subsequent_calls_after_final_bail() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/query/sql"))
            .respond_with(metadata_error_template())
            .expect(3)
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        let first = sy
            .post_with_pacing(
                "/api/query/sql",
                json!({"stmt": "select 1"}),
                RetryPacing::instant(),
            )
            .await;
        assert!(first.is_err(), "persistent wave must bail");

        let second = sy
            .post_with_pacing(
                "/api/query/sql",
                json!({"stmt": "select 1"}),
                RetryPacing::instant(),
            )
            .await;
        assert!(second.is_err(), "cooldown must fail fast the next call");
        assert!(second.unwrap_err().to_string().contains("окно fail-fast"));
    }

    #[tokio::test]
    async fn non_metadata_errors_do_not_arm_wave_cooldown() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/query/sql"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"code": -1, "msg": "не найдено"})),
            )
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        assert!(sy
            .post_with_pacing(
                "/api/query/sql",
                json!({"stmt": "select 1"}),
                RetryPacing::instant()
            )
            .await
            .is_err());

        // Окно не армировано: следующий вызов доходит до мока (4+ запросов суммарно).
        assert!(sy
            .post_with_pacing(
                "/api/query/sql",
                json!({"stmt": "select 1"}),
                RetryPacing::instant()
            )
            .await
            .is_err());
    }

    #[test]
    fn body_snippet_truncates_long_bodies() {
        let long = "x".repeat(500);
        let snippet = body_snippet(&long);
        assert_eq!(snippet.chars().count(), 201);
        assert!(snippet.ends_with('…'));
        assert_eq!(body_snippet("short"), "short");
    }

    #[tokio::test]
    async fn post_probe_returns_full_envelope_on_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/query/sql"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"code":0,"msg":"","data":[{"n":71621}],"limit":64,"truncated":false}"#,
            ))
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        let envelope = sy
            .post_probe(
                "/api/query/sql",
                json!({ "stmt": "select count(*) as n from blocks" }),
            )
            .await
            .unwrap();
        assert_eq!(envelope.pointer("/data/0/n"), Some(&json!(71621)));
        assert_eq!(envelope.get("limit"), Some(&json!(64)));
    }

    #[tokio::test]
    async fn post_probe_fails_without_retries_on_nonzero_code() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/query/sql"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"code":10201,"msg":"metadata is not ready"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .post_probe("/api/query/sql", json!({ "stmt": "select 1" }))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("10201") && err.contains("metadata is not ready"),
            "{err}"
        );
        // expect(1): ровно один запрос — если бы проба ретраила, мок упал бы на съёмке
    }

    #[tokio::test]
    async fn post_probe_reports_auth_failure_with_status_and_envelope() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/query/sql"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_string(r#"{"code":-1,"msg":"Auth failed: invalid token"}"#),
            )
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .post_probe("/api/query/sql", json!({ "stmt": "select 1" }))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("401") && err.contains("Auth failed"), "{err}");
    }

    #[tokio::test]
    async fn post_probe_reports_non_json_body_with_snippet() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/query/sql"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>login</html>"))
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .post_probe("/api/query/sql", json!({ "stmt": "select 1" }))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("не JSON") && err.contains("<html>login</html>"),
            "{err}"
        );
    }

    #[test]
    fn parse_block_ids_reads_trailing_kernel_ids() {
        let text =
            "- [p] первый (20261104130857-b9drqmf)\n- [h] ## Заголовок (20261104130919-on7yaie)\n";
        assert_eq!(
            parse_block_ids(text),
            vec!["20261104130857-b9drqmf", "20261104130919-on7yaie"]
        );
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
            &[
                "20261104130857-b9drqmf".into(),
                "20261104131412-wbpz3sx".into()
            ],
            "- a (20261104130857-b9drqmf)\n- b (20261104131412-wbpz3sx)\n"
        ));
    }

    #[test]
    fn parse_block_ids_survives_trailing_multibyte_paren_text() {
        // Паник-фикстура doctor'ом: кириллический кандидат — ПОСЛЕДНЯЯ скобка
        // строки, ровно 22 байта (5«ы»=10б + abc=3б + «ы» на байтах 13–14 + 7б хвоста):
        // байт 14 — continuation «ы» → старые байт-срезы candidate[..14] паниковали.
        let text = "- [p] сноска (ыыыыыabcы-x-yzab)\n- [p] реальный (20261105101010-abc1234)\n";
        let ids = parse_block_ids(text);
        assert_eq!(ids, vec!["20261105101010-abc1234".to_string()]);
    }

    #[test]
    fn doc_file_path_joins_notebook_ancestors_and_doc_id() {
        let got = SiYuan::doc_file_path(
            "20261103224312-fbrf5fp",
            &["20261103233646-vzgad8m"],
            "20261104215722-1p0eetv",
        );
        assert_eq!(
            got,
            "data/20261103224312-fbrf5fp/20261103233646-vzgad8m/20261104215722-1p0eetv.sy"
        );
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

        assert_eq!(
            sy.fetch_workspace_file("data/nb/doc.sy").await,
            FileFetch::Bytes(b"{\"ID\":\"doc\",\"Type\":\"NodeDocument\"}".to_vec())
        );
        assert_eq!(
            sy.fetch_workspace_file("data/nb/other.sy").await,
            FileFetch::Missing
        );
    }

    #[tokio::test]
    async fn fetch_workspace_file_reports_failure_on_transport_error() {
        // несуществующий порт = соединение не состоялось → Failed, не Missing
        let sy = SiYuan::for_tests("http://127.0.0.1:9".into());
        assert!(matches!(
            sy.fetch_workspace_file("data/nb/doc.sy").await,
            FileFetch::Failed(_)
        ));
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
                ResponseTemplate::new(200).set_body_string(if ok {
                    r#"{"code":0,"msg":"","data":null}"#
                } else {
                    r#"{"code":-1}"#
                })
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

        sy.put_workspace_file("data/assets/undo/j.json", b"{\"k\":1}")
            .await
            .unwrap();
        let read_back = sy.fetch_workspace_file("data/assets/undo/j.json").await;
        assert!(matches!(read_back, FileFetch::Bytes(_)));
        if let FileFetch::Bytes(bytes) = read_back {
            assert!(
                bytes.windows(7).any(|w| w == b"{\"k\":1}"),
                "контент в multipart-теле: {bytes:?}"
            );
        }
    }

    #[tokio::test]
    async fn list_dir_returns_entry_names() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/file/readDir"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                // правда ядра (проба 05.11): несуществующий путь — 404-конверт top-level
                let envelope = if body.get("path").and_then(|p| p.as_str()) == Some("data/assets/undo") {
                    serde_json::json!({ "code": 0, "data": [
                        { "name": "20261104-clean.json", "isDir": false, "isSymlink": false, "updated": 1 },
                        { "name": "sub", "isDir": true, "isSymlink": false, "updated": 1 }
                    ] })
                } else {
                    serde_json::json!({ "code": 404, "msg": "path does not exist" })
                };
                ResponseTemplate::new(200).set_body_string(envelope.to_string())
            })
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        let names = sy.list_dir("data/assets/undo").await.unwrap();
        assert_eq!(
            names,
            vec!["20261104-clean.json".to_string(), "sub".to_string()]
        );
        let err = sy.list_dir("data/assets/nowhere").await.expect_err(
            "list_dir честен: несуществующая директория — ошибка ядра, не пустой список",
        );
        assert!(
            err.to_string().contains("path does not exist"),
            "суть отказа: {err}"
        );
    }

    #[tokio::test]
    async fn try_list_dir_missing_dir_is_none_existing_dir_has_names() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/file/readDir"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let envelope =
                    if body.get("path").and_then(|p| p.as_str()) == Some("data/assets/undo") {
                        serde_json::json!({ "code": 0, "data": [{ "name": "j.json" }] })
                    } else {
                        serde_json::json!({ "code": 404, "msg": "path does not exist" })
                    };
                ResponseTemplate::new(200).set_body_string(envelope.to_string())
            })
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        let names = sy
            .try_list_dir("data/assets/undo")
            .await
            .unwrap()
            .expect("директория есть");
        assert_eq!(names, vec!["j.json".to_string()]);
        let missing = sy.try_list_dir("data/assets/nowhere").await.unwrap();
        assert!(missing.is_none(), "нет директории — None, не ошибка");
    }

    #[tokio::test]
    async fn upload_asset_returns_kernel_generated_path_from_succ_files() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/asset/upload"))
            .respond_with(|req: &wiremock::Request| {
                // multipart: ядро ждёт assetsDir + file[] с оригинальным именем
                let body = String::from_utf8_lossy(&req.body);
                let ok = body.contains("name=\"assetsDir\"")
                    && body.contains("name=\"file[]\"")
                    && body.contains("filename=\"скрин 2.png\"")
                    && body.contains("png-bytes");
                if ok {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "code": 0,
                        "data": { "succFiles": [{ "index": 0, "name": "скрин 2.png",
                                  "path": "assets/скрин 2-20261106090000-abc1234.png" }] }
                    }))
                } else {
                    ResponseTemplate::new(200)
                        .set_body_string(r#"{"code":-1,"msg":"нет полей multipart"}"#)
                }
            })
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());
        let dir = tempfile::tempdir().expect("tmp dir");
        let file = dir.path().join("скрин 2.png");
        std::fs::write(&file, b"png-bytes").expect("write fixture");

        let asset = sy.upload_asset(&file).await.expect("upload ok");
        assert_eq!(asset.original_name, "скрин 2.png");
        assert_eq!(
            asset.asset_path,
            "assets/скрин 2-20261106090000-abc1234.png"
        );
    }

    #[tokio::test]
    async fn upload_asset_kernel_error_envelope_is_loud_bail() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/asset/upload"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"code":-1,"msg":"asset size exceeds limit"}"#),
            )
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());
        let dir = tempfile::tempdir().expect("tmp dir");
        let file = dir.path().join("big.png");
        std::fs::write(&file, b"bytes").expect("write fixture");

        let err = sy
            .upload_asset(&file)
            .await
            .expect_err("kernel code=-1 must bail");
        assert!(
            err.to_string().contains("asset size exceeds limit"),
            "суть отказа ядра видна: {err}"
        );
    }

    #[tokio::test]
    async fn upload_asset_missing_file_bails_before_network() {
        let server = MockServer::start().await;
        // ни одного маунта: любой сетевой вызов упал бы по соединению —
        // отказ обязан случиться раньше, на чтении файла
        let sy = SiYuan::for_tests(server.uri());
        let err = sy
            .upload_asset(std::path::Path::new("/nonexistent/dir/x.png"))
            .await
            .expect_err("missing file must bail");
        assert!(
            err.to_string().contains("недоступен"),
            "отказ на чтении файла: {err}"
        );
    }

    #[tokio::test]
    async fn rename_doc_sends_notebook_relative_path_and_title() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/filetree/renameDoc"))
            .respond_with(|req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let ok = body.get("notebook").and_then(|n| n.as_str()) == Some("nb-1")
                    && body.get("path").and_then(|p| p.as_str()) == Some("/root/board/doc-id.sy")
                    && body.get("title").and_then(|t| t.as_str()) == Some("новый тайтл");
                if ok {
                    ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
                } else {
                    ResponseTemplate::new(200)
                        .set_body_string(r#"{"code":-1,"msg":"rename request shape mismatch"}"#)
                }
            })
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        sy.rename_doc("nb-1", "/root/board/doc-id.sy", "новый тайтл")
            .await
            .expect("rename ok");
    }

    /// Мок MCP-сервера: initialize по протоколу, tools/call — фиксированный result.
    async fn mount_mcp_tool_result(server: &MockServer, result: serde_json::Value) {
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(move |req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    return ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":1}}}"#,
                        )
                        .append_header("mcp-session-id", "sess-ok-barrier");
                }
                ResponseTemplate::new(200).set_body_string(
                    serde_json::json!({ "jsonrpc": "2.0", "id": 2, "result": result }).to_string(),
                )
            })
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn mcp_tool_bails_on_ok_false_in_structured_content() {
        let server = MockServer::start().await;
        mount_mcp_tool_result(
            &server,
            json!({
                "content": [{ "type": "text", "text": "{\"action\":\"item_update\",\"ok\":false,\"msg\":\"WRITE_QUEUE_TIMEOUT\"}" }],
                "structuredContent": { "action": "item_update", "ok": false, "msg": "WRITE_QUEUE_TIMEOUT" }
            }),
        )
        .await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .mcp_tool("database", json!({ "action": "item_update" }))
            .await
            .expect_err("ok=false inside a successful MCP response must not pass as data");

        let text = err.to_string();
        assert!(text.contains("ok=false"), "барьер называет форму: {text}");
        assert!(
            text.contains("WRITE_QUEUE_TIMEOUT"),
            "причина шлюза: {text}"
        );
    }

    #[tokio::test]
    async fn mcp_tool_bails_on_nonzero_code_in_structured_content() {
        let server = MockServer::start().await;
        mount_mcp_tool_result(
            &server,
            json!({
                "content": [{ "type": "text", "text": "{\"action\":\"keys\",\"code\":-1,\"msg\":\"boom\"}" }],
                "structuredContent": { "action": "keys", "code": -1, "msg": "boom" }
            }),
        )
        .await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .mcp_tool("database", json!({ "action": "keys" }))
            .await
            .expect_err("kernel envelope code=-1 without isError must not pass as data");

        let text = err.to_string();
        assert!(text.contains("code=-1"), "барьер называет код: {text}");
        assert!(text.contains("boom"), "причина ядра: {text}");
    }

    #[tokio::test]
    async fn mcp_tool_bails_on_nonzero_code_in_content_json() {
        let server = MockServer::start().await;
        mount_mcp_tool_result(
            &server,
            json!({
                "content": [{ "type": "text", "text": "{\"action\":\"keys\",\"code\":10201,\"msg\":\"metadata is not ready\"}" }]
            }),
        )
        .await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .mcp_tool("database", json!({ "action": "keys" }))
            .await
            .expect_err("kernel error envelope relayed as content JSON must not pass as data");

        let text = err.to_string();
        assert!(text.contains("10201"), "код назван: {text}");
    }

    #[tokio::test]
    async fn mcp_tool_passes_clean_structured_content_through() {
        let server = MockServer::start().await;
        mount_mcp_tool_result(
            &server,
            json!({
                "content": [{ "type": "text", "text": "{\"action\":\"keys\",\"data\":{\"keys\":[]}}" }],
                "structuredContent": { "action": "keys", "data": { "keys": ["k1"] } }
            }),
        )
        .await;
        let sy = SiYuan::for_tests(server.uri());

        let v = sy
            .mcp_tool("database", json!({ "action": "keys" }))
            .await
            .expect("success envelope without error markers must pass");

        assert_eq!(
            v.pointer("/data/keys/0").and_then(|k| k.as_str()),
            Some("k1")
        );
    }

    #[tokio::test]
    async fn mcp_tool_passes_kernel_success_envelope_in_content_json() {
        let server = MockServer::start().await;
        mount_mcp_tool_result(
            &server,
            json!({
                "content": [{ "type": "text", "text": "{\"action\":\"render\",\"code\":0,\"msg\":\"\",\"data\":{\"rows\":[]}}" }]
            }),
        )
        .await;
        let sy = SiYuan::for_tests(server.uri());

        let v = sy
            .mcp_tool("database", json!({ "action": "render" }))
            .await
            .expect("kernel success envelope (code=0) must pass");

        assert!(v.get("data").is_some(), "данные на месте: {v}");
    }

    #[tokio::test]
    async fn export_md_by_id_strips_zero_width_but_keeps_fences_verbatim() {
        let server = MockServer::start().await;
        let noisy =
            "текст \u{200b}`code`\u{200b} хвост\n\n```rust\nkeep \u{200b}\u{200d} inside\n```\n";
        Mock::given(method("POST"))
            .and(path("/api/export/exportMdContent"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
                ResponseTemplate::new(200).set_body_string(
                    json!({ "code": 0, "data": { "content": noisy, "id": id } }).to_string(),
                )
            })
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        let md = sy
            .export_md_by_id("20261107123400-abcdef1")
            .await
            .expect("экспорт успешен");

        assert_eq!(
            md, "текст `code` хвост\n\n```rust\nkeep \u{200b}\u{200d} inside\n```\n",
            "zero-width снят вне фенсов, фенс дословен"
        );
    }
}
