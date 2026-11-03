//! JSON-RPC client for the affine-mcp gateway (Streamable HTTP, MCP 2025-03-26).
//!
//! The gateway is the only stable API surface over AFFiNE's internal GraphQL:
//! uwuwu-cli talks plain HTTP + JSON-RPC here, no MCP SDK involved. One client
//! owns one server session (initialize → mcp-session-id header on every call).
//!
//! Session exclusivity: sequential runs share the saved session for free;
//! a concurrent run that loses the flock gets its own private session and
//! never touches the shared file. JSON-RPC ids start from a per-process
//! random base — the gateway matches responses by (session, id), so a shared
//! session with colliding ids cross-delivers responses between processes
//! (задача IyzxmFdujA). Lock lives on a dedicated file that is never
//! deleted or rewritten — the session file itself gets replaced by
//! handshakes, and locking a replaced inode would fork the lock.
//!
//! Slightly over the 200-line module limit: session lifecycle, retry policy
//! and error classification are one cohesive transport contract; extracting
//! any part would force the caller to understand transport internals.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use fs4::fs_std::FileExt;
use serde_json::{json, Value};

use crate::affine_sse;

/// Повторяем только транзиентное. Дефолт 3 ретрая = до 4 попыток на вызов;
/// env UWUWU_RETRY_ATTEMPTS (0 = без ретраев).
const DEFAULT_RETRIES: usize = 3;
const MAX_RETRIES: usize = 12;
/// База экспоненциального бэкофа (full jitter), env UWUWU_RETRY_BASE_MS.
const DEFAULT_BASE_MS: u64 = 1000;
/// Потолок бэкофа одной паузы. Retry-After от шлюза уважается поверх этого
/// капа (серверная инструкция приоритетна) но тоже клампится — см. ниже.
const BACKOFF_CAP_MS: u64 = 20_000;
/// Кламп Retry-After: 503 с «спи 10 минут» не спим дольше полминуты —
/// длинный outage клиент всё равно не переживает, честнее отдать ошибку.
const RETRY_AFTER_CAP_SECS: u64 = 30;
/// Клиентский таймаут одного запроса, env UWUWU_CALL_TIMEOUT_SECS (шов для тестов).
const DEFAULT_TIMEOUT_SECS: u64 = 120;
/// Handshake ретраится листовым циклом (initialize не ходит через call_tool —
/// вложенных пере-handshake'ов нет).
const HANDSHAKE_TRIES: usize = 3;

pub struct AffineConfig {
    pub url: String,
    pub token: String,
    pub workspace: String,
}

impl AffineConfig {
    /// URL defaults to the local gateway; token and workspace must be set.
    pub fn from_env() -> Result<Self> {
        let url = std::env::var("UWUWU_AFFINE_URL")
            .unwrap_or_else(|_| "http://localhost:3000/mcp".to_string());
        let token = std::env::var("UWUWU_AFFINE_TOKEN")
            .map_err(|_| anyhow!("UWUWU_AFFINE_TOKEN is not set"))?;
        let workspace = std::env::var("UWUWU_AFFINE_WORKSPACE_ID")
            .map_err(|_| anyhow!("UWUWU_AFFINE_WORKSPACE_ID is not set"))?;
        Ok(Self {
            url,
            token,
            workspace,
        })
    }
}

/// Повторяемость операции по имени тулзы. Чтение идемпотентно — ретраится
/// всегда; запись ретраится только когда шлюз ГАРАНТИРОВАННО не применял
/// запрос (отказ до обработки, 429/5xx, ok:false+retryable). Таймаут/обрыв
/// для записи — Fatal: ответ потерян, операция могла примениться, повтор
/// продублировал бы контент (идемпотентность create — отдельная задача).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OpKind {
    Read,
    Write,
}

fn op_kind(tool: &str) -> OpKind {
    // чтения — закрытый список префиксов; всё остальное = write
    // (safe default: новый read-тулз теряет ретраи, но запись никогда
    // не дублируется)
    const READ: &[&str] = &[
        "get_", "list_", "search_", "find_", "read_", "export_", "analyze_",
    ];
    if READ.iter().any(|p| tool.starts_with(p)) {
        OpKind::Read
    } else {
        OpKind::Write
    }
}

/// Конфиг повторов из env; мусорное значение → warning + дефолт, значения
/// клампятся (безумные attempts не сорвут крон в часовой цикл).
#[derive(Clone, Copy, Debug)]
struct RetryConfig {
    /// Число ПОВТОРОВ после первой попытки (0 = один заход без ретраев).
    retries: usize,
    base_ms: u64,
    timeout_secs: u64,
}

impl RetryConfig {
    fn from_env() -> Self {
        Self::from_values(
            std::env::var("UWUWU_RETRY_ATTEMPTS").ok().as_deref(),
            std::env::var("UWUWU_RETRY_BASE_MS").ok().as_deref(),
            std::env::var("UWUWU_CALL_TIMEOUT_SECS").ok().as_deref(),
        )
    }

    fn from_values(attempts: Option<&str>, base_ms: Option<&str>, timeout: Option<&str>) -> Self {
        Self {
            retries: env_usize(
                "UWUWU_RETRY_ATTEMPTS",
                attempts,
                DEFAULT_RETRIES,
                0,
                MAX_RETRIES,
            ),
            base_ms: env_u64("UWUWU_RETRY_BASE_MS", base_ms, DEFAULT_BASE_MS, 0, 60_000),
            timeout_secs: env_u64(
                "UWUWU_CALL_TIMEOUT_SECS",
                timeout,
                DEFAULT_TIMEOUT_SECS,
                1,
                600,
            ),
        }
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

fn env_usize(name: &str, raw: Option<&str>, default: usize, lo: usize, hi: usize) -> usize {
    match raw {
        None => default,
        Some(s) => match s.trim().parse::<usize>() {
            Ok(v) => v.clamp(lo, hi),
            Err(_) => {
                eprintln!("uwuwu-cli: {name}={s:?} не число — беру дефолт {default}");
                default
            }
        },
    }
}

fn env_u64(name: &str, raw: Option<&str>, default: u64, lo: u64, hi: u64) -> u64 {
    match raw {
        None => default,
        Some(s) => match s.trim().parse::<u64>() {
            Ok(v) => v.clamp(lo, hi),
            Err(_) => {
                eprintln!("uwuwu-cli: {name}={s:?} не число — беру дефолт {default}");
                default
            }
        },
    }
}

/// xorshift64* без внешних зависимостей: full-jitter пауз и случайная база
/// JSON-RPC id. Детерминируемый при заданном seed — расписание бэкофа
/// тестируется чистой функцией.
struct XorShift64(u64);

impl XorShift64 {
    fn seeded(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn from_clock() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let pid = u64::from(std::process::id());
        Self::seeded(nanos ^ (pid << 32) ^ pid.wrapping_mul(0x9E37_79B9_7F4A_7C15))
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Случайное из [0, n).
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
}

/// Полная пауза перед повтором попытки `attempt` (0-based): base·2^attempt
/// с клампом BACKOFF_CAP_MS и full jitter → [0, capped]. Чистая функция.
fn backoff_delay_ms(base_ms: u64, attempt: usize, rng: &mut XorShift64) -> u64 {
    let raw = base_ms.saturating_mul(1u64 << (attempt as u32).min(20));
    let capped = raw.min(BACKOFF_CAP_MS);
    rng.below(capped + 1)
}

/// Retry-After: только целые секунды (date-форма не поддерживается — на
/// практике не встречается), кламп ≤ RETRY_AFTER_CAP_SECS.
fn parse_retry_after(value: Option<&str>) -> Option<Duration> {
    let secs = value?.trim().parse::<u64>().ok()?;
    Some(Duration::from_secs(secs.min(RETRY_AFTER_CAP_SECS)))
}

/// Эксклюзивность общей сессии: flock на ОТДЕЛЬНОМ lock-файле, который
/// никогда не удаляется и не перезаписывается. Лок на самом session-файле
/// ломался бы сменой inode (handshake переписывает файл). Держится до Drop:
/// последовательные запуски шарят сессию бесплатно, конкурентные получают
/// private-сессию (цена — один handshake, 4–7с).
struct SessionLock(std::fs::File);

impl SessionLock {
    fn acquire(path: &PathBuf) -> Option<Self> {
        // родительский каталог может не существовать при самом первом запуске
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = match std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(path)
        {
            Ok(f) => f,
            // ошибка ФС — не «лок занят»: молча все стали бы private
            // (handshake-шторм); предупреждаем и всё же работаем приватно
            Err(e) => {
                eprintln!(
                    "uwuwu-cli: не смог открыть {}: {e} — работаю без лока",
                    path.display()
                );
                return None;
            }
        };
        match file.try_lock_exclusive() {
            Ok(true) => Some(Self(file)),
            // Ok(false) = лок занят другим процессом — штатный private-путь
            _ => None,
        }
    }
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub struct AffineClient {
    url: String,
    token: String,
    session: String,
    http: reqwest::Client,
    next_id: u64,
    /// Some = shared-режим (держим лок, публикуем handshake в общий файл);
    /// None = private (конкурентный запуск: своя сессия, файл не трогаем).
    shared_lock: Option<SessionLock>,
}

impl AffineClient {
    /// Open a session: переиспользуем сохранённый session-id (initialize =
    /// sign-in афины ~4-7с на каждый запуск — главный источник латентности).
    /// Протухшая сессия даёт ошибку на первом tool → полный handshake.
    pub async fn connect(cfg: &AffineConfig) -> Result<Self> {
        let retry = RetryConfig::from_env();
        let mut client = Self {
            url: cfg.url.clone(),
            token: cfg.token.clone(),
            session: String::new(),
            http: http_client(retry.timeout())?,
            // случайная база id: шлюз матчит ответ по (session, id) — у
            // процессов на одной сессии не должно быть пересечений id.
            // Кламп в JS-safe диапазон (< 2^52): шлюз на JS, бо́льшие id
            // теряют точность при парсинге и ломают матчинг ответов
            next_id: (XorShift64::from_clock().next_u64() % (1u64 << 52)) | 1,
            shared_lock: None,
        };
        let dir = uwuwu_dir();
        let session_file = dir.join("mcp-session.txt");
        if let Some(lock) = SessionLock::acquire(&dir.join("mcp-session.lock")) {
            client.shared_lock = Some(lock);
            if let Ok(saved) = std::fs::read_to_string(&session_file) {
                let sid = saved.trim().to_string();
                if !sid.is_empty() {
                    client.session = sid;
                    return Ok(client);
                }
            }
            client.handshake(true, &retry).await?;
            return Ok(client);
        }
        // лок занят: у общего файла есть живой владелец — своя сессия,
        // файл не читаем (его сессия занята) и не пишем (не подменяем)
        client.handshake(false, &retry).await?;
        Ok(client)
    }

    fn publishes(&self) -> bool {
        self.shared_lock.is_some()
    }

    /// Полный handshake: initialize + initialized. publish=false — приватная
    /// сессия, общий файл не пишется (все пути пере-handshake'а наследуют
    /// режим старта — иначе private-процесс подменил бы общую сессию своей).
    /// Ретраи листовым циклом: транспортные отказы и 429/5xx; таймаут не
    /// ретраим — осиротевшая сессия жрёт слот шлюза до idle-timeout.
    async fn handshake(&mut self, publish: bool, retry: &RetryConfig) -> Result<()> {
        let mut rng = XorShift64::from_clock();
        let mut last_err = None;
        for attempt in 0..HANDSHAKE_TRIES {
            if attempt > 0 {
                let pause =
                    Duration::from_millis(backoff_delay_ms(retry.base_ms, attempt - 1, &mut rng));
                eprintln!(
                    "uwuwu-cli: повтор handshake {attempt}/{} через {:.1}s: {}",
                    HANDSHAKE_TRIES - 1,
                    pause.as_secs_f32(),
                    last_err
                        .as_ref()
                        .map(|e: &anyhow::Error| e.to_string())
                        .unwrap_or_default()
                );
                tokio::time::sleep(pause).await;
            }
            match self.initialize_once(retry.timeout()).await {
                Ok(sid) => {
                    self.session = sid;
                    if publish {
                        let path = uwuwu_dir().join("mcp-session.txt");
                        let _ = std::fs::create_dir_all(
                            path.parent().unwrap_or(std::path::Path::new(".")),
                        );
                        let _ = std::fs::write(path, &self.session);
                    }
                    self.post_notification(json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/initialized"
                    }))
                    .await?;
                    return Ok(());
                }
                Err(e) => {
                    if !e.retryable {
                        return Err(e.err);
                    }
                    last_err = Some(e.err);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("handshake failed")))
    }

    /// Одна попытка initialize. Возвращает session-id из заголовка.
    async fn initialize_once(&mut self, timeout: Duration) -> Result<String, HandshakeError> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": self.alloc_id(),
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "uwuwu-cli", "version": env!("CARGO_PKG_VERSION")}
            }
        });
        // СВЕЖИЙ клиент без пула: handshake не должен переиспользовать
        // соединения, которые шлюз уже привязал к другой (мёртвой) сессии
        let http = match http_client(timeout) {
            Ok(h) => h,
            Err(e) => return Err(HandshakeError::fatal(e)),
        };
        let resp = match http
            .post(&self.url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&payload)
            .send()
            .await
        {
            Ok(r) => r,
            // таймаут не ретраим: осиротевшая сессия жрёт слот шлюза
            Err(e) if e.is_timeout() => {
                return Err(HandshakeError::fatal(anyhow!("initialize timeout: {e}")))
            }
            Err(e) => {
                return Err(HandshakeError::retryable(anyhow!(
                    "initialize send failed: {e}"
                )))
            }
        };
        let status = resp.status();
        let sid_hdr = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let raw = resp.text().await.unwrap_or_default();
        if status.as_u16() == 429 || status.is_server_error() {
            return Err(HandshakeError::retryable(anyhow!(
                "initialize failed: HTTP {status}: {raw}"
            )));
        }
        if !status.is_success() {
            return Err(HandshakeError::fatal(anyhow!(
                "initialize failed: HTTP {status}: {raw}"
            )));
        }
        match sid_hdr {
            Some(sid) => Ok(sid),
            None => Err(HandshakeError::fatal(anyhow!(
                "gateway did not return mcp-session-id"
            ))),
        }
    }

    /// Call a gateway tool; returns its structured output (or the parsed
    /// text payload when the server sent no structuredContent).
    /// Ретраи: транспорт (connect/429/5xx) и ok:false+retryable — всегда;
    /// таймаут/обрыв — только чтение (запись могла примениться).
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let retry = RetryConfig::from_env();
        let op = op_kind(name);
        let mut rng = XorShift64::from_clock();
        let mut stale_retried = false;
        let mut last_err: Option<anyhow::Error> = None;
        // Retry-After от 429/503 заменяет бэкоф одной ближайшей паузы
        let mut honor_retry_after: Option<Duration> = None;
        for attempt in 0..=retry.retries {
            if attempt > 0 {
                let pause = honor_retry_after.take().unwrap_or_else(|| {
                    Duration::from_millis(backoff_delay_ms(retry.base_ms, attempt - 1, &mut rng))
                });
                eprintln!(
                    "uwuwu-cli: повтор {attempt}/{} через {:.1}s: {}",
                    retry.retries,
                    pause.as_secs_f32(),
                    last_err.as_ref().map(short_reason).unwrap_or_default()
                );
                tokio::time::sleep(pause).await;
            }
            let call_id = self.alloc_id();
            let body = json!({
                "jsonrpc": "2.0",
                "id": call_id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments}
            });
            match self.raw_post(body, call_id).await {
                Ok(resp) => match Self::tool_failure(&resp) {
                    // повторяемого сбоя нет — извлекаем результат
                    None => return Self::tool_result(name, &resp),
                    Some((detail, true)) => {
                        last_err = Some(anyhow!("tool {name} failed: {detail}"));
                    }
                    Some((detail, false)) => {
                        return Err(anyhow!("tool {name} failed: {detail}"));
                    }
                },
                Err(GatewayError::StaleSession(e)) if !stale_retried => {
                    // сессия протухла (idle timeout шлюза) — полный handshake
                    // и повтор; режим публикации наследуется от старта
                    stale_retried = true;
                    self.session = String::new();
                    eprintln!("uwuwu-cli: сессия протухла — пере-handshake");
                    self.handshake(self.publishes(), &retry)
                        .await
                        .context("пере-handshake после stale-сессии")?;
                    last_err = Some(e);
                }
                // осознанная расходимость с python-зеркалом (wiki_ops.call):
                // здесь stale после пере-handshake дожимает попытки, там —
                // fail fast; не уравнивать вслепую
                Err(GatewayError::StaleSession(e)) => last_err = Some(e),
                Err(GatewayError::Transport { err, retry_after }) => {
                    last_err = Some(err);
                    honor_retry_after = retry_after;
                }
                Err(GatewayError::Ambiguous(e)) => {
                    if op == OpKind::Write {
                        return Err(anyhow!(
                            "tool {name}: ответ потерян, операция могла примениться — \
                             проверь результат перед ручным повтором: {e}"
                        ));
                    }
                    last_err = Some(e);
                }
                Err(GatewayError::Fatal(e)) => return Err(e),
            }
        }
        Err(anyhow!(
            "{name} failed after {} retries: {}\n\
             шлюз недоступен по UWUWU_AFFINE_URL={} — проверь адрес шлюза \
             и перезапусти терминал после смены env; если шлюз лежит целиком, \
             держи паузу 120–180с между попытками",
            retry.retries,
            last_err.map(|e| e.to_string()).unwrap_or_default(),
            self.url
        ))
    }

    /// ok:false — шлюзовая конвенция логических фейлов. Кандидаты в порядке
    /// зеркала python `_ok_false`: structuredContent, result верхнего
    /// уровня, content[0].text-блоб. Возвращает ГОЛЫЙ detail — префикс
    /// «tool {name} failed:» навешивает call_tool (один раз, без наслоений).
    /// retryable:true = шлюз классифицировал сбой как транзиентный; контракт
    /// (задача W9etJF1FVm): true ⇒ запрос гарантированно не применён.
    fn tool_failure(body: &Value) -> Option<(String, bool)> {
        let payload = body.get("result")?;
        if let Some(sc) = payload.get("structuredContent") {
            if is_ok_false(sc) {
                return Some((ok_false_detail(sc), retryable_flag(sc)));
            }
        }
        if is_ok_false(payload) {
            return Some((ok_false_detail(payload), retryable_flag(payload)));
        }
        let text = payload.get("content")?.get(0)?.get("text")?.as_str()?;
        let parsed = serde_json::from_str::<Value>(text).ok()?;
        if is_ok_false(&parsed) {
            return Some((
                format!("text payload; {}", ok_false_detail(&parsed)),
                retryable_flag(&parsed),
            ));
        }
        None
    }

    fn tool_result(name: &str, body: &Value) -> Result<Value> {
        let payload = body
            .get("result")
            .ok_or_else(|| anyhow!("RPC response has no result: {body}"))?;
        if let Some(err) = payload.get("error") {
            anyhow::bail!("tool {name} failed: {err}");
        }
        // gateway convention: logical failures answer ok:false inside a
        // successful RPC result — surface them as errors
        if payload.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            anyhow::bail!("tool {name} failed: {}", ok_false_detail(payload));
        }
        if let Some(sc) = payload.get("structuredContent") {
            return Ok(sc.clone());
        }
        Self::text_payload(payload)
    }

    /// Tools answer either with structuredContent or with a text blob that
    /// itself contains JSON (the gateway's style). Non-JSON text is wrapped.
    fn text_payload(payload: &Value) -> Result<Value> {
        let text = payload
            .get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow!("tool result has neither structuredContent nor text"))?;
        let parsed =
            serde_json::from_str::<Value>(text).unwrap_or_else(|_| Value::String(text.to_string()));
        // text-blob tools wrap logical failures as {ok:false} INSIDE the
        // blob — surface them instead of returning error JSON as data
        if parsed.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            anyhow::bail!("tool error (text payload): {}", ok_false_detail(&parsed));
        }
        Ok(parsed)
    }

    fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Fire-and-forget notification: 202 with an empty body is success.
    async fn post_notification(&self, body: Value) -> Result<()> {
        let resp = self
            .http
            .post(&self.url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("mcp-session-id", &self.session)
            .json(&body)
            .send()
            .await
            .with_context(|| "gateway notification failed")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("gateway HTTP {status}: {text}");
        }
        // успех = fire-and-forget: шлюз держит SSE-стрим открытым (~6с) —
        // чтение тела на каждое уведомление съедало полкоманды
        Ok(())
    }

    async fn raw_post(
        &self,
        body: Value,
        expected_id: u64,
    ) -> std::result::Result<Value, GatewayError> {
        let mut req = self
            .http
            .post(&self.url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        if !self.session.is_empty() {
            req = req.header("mcp-session-id", &self.session);
        }
        let resp = match req.json(&body).send().await {
            Ok(resp) => resp,
            // таймаут = ответ потерян: чтение можно повторить, запись — нет
            Err(e) if e.is_timeout() => {
                return Err(GatewayError::Ambiguous(anyhow!("gateway timeout")));
            }
            Err(e) => {
                return Err(GatewayError::Transport {
                    err: e.into(),
                    retry_after: None,
                })
            }
        };
        let status = resp.status();
        let retry_after = parse_retry_after(
            resp.headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
        );
        // протухшая/чужая сессия: 400/404 на этапе обработки — сигнал к re-handshake
        // (проверяется ДО общего 4xx→Fatal)
        if (status.as_u16() == 400 || status.as_u16() == 404) && !self.session.is_empty() {
            return Err(GatewayError::StaleSession(anyhow!(
                "HTTP {status}: session rejected"
            )));
        }
        let raw = resp.text().await.unwrap_or_default();
        if status.as_u16() == 429 || status.is_server_error() {
            return Err(GatewayError::Transport {
                err: anyhow!("HTTP {status}: {raw}"),
                retry_after,
            });
        }
        if !status.is_success() {
            return Err(GatewayError::Fatal(anyhow!("HTTP {status}: {raw}")));
        }
        let payload = affine_sse::parse_response(&raw, expected_id).map_err(|e| {
            // обрыв потока / чужой id / пустое тело — ответ не дошел: чтение
            // повторяемо, запись нет (классифицируется по Ambiguous выше)
            GatewayError::Ambiguous(anyhow!("unparsable gateway response: {e}"))
        })?;
        if let Some(err) = payload.get("error") {
            return Err(GatewayError::Fatal(anyhow!("RPC error: {err}")));
        }
        Ok(payload)
    }
}

fn http_client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .context("http client build")
}

/// Каталог uwuwu-состояния: session-файл и lock живут рядом (единый HOME).
fn uwuwu_dir() -> PathBuf {
    std::env::var("HOME")
        .map(|h| PathBuf::from(h).join(".uwuwu"))
        .unwrap_or_else(|_| PathBuf::from(".uwuwu"))
}

fn is_ok_false(v: &Value) -> bool {
    v.get("ok").and_then(|x| x.as_bool()) == Some(false)
}

fn retryable_flag(v: &Value) -> bool {
    v.get("retryable")
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
}

fn ok_false_detail(v: &Value) -> String {
    v.get("error")
        .and_then(|e| e.as_str())
        .unwrap_or("unknown tool error")
        .to_string()
}

fn short_reason(e: &anyhow::Error) -> String {
    let s = e.to_string();
    s.chars().take(90).collect()
}

/// Транспортный отказ → handshake-цикл повторяет; остальное (таймаут,
/// протокол, не-2xx кроме 429/5xx) — нет.
struct HandshakeError {
    err: anyhow::Error,
    retryable: bool,
}

impl HandshakeError {
    fn retryable(err: anyhow::Error) -> Self {
        Self {
            err,
            retryable: true,
        }
    }
    fn fatal(err: anyhow::Error) -> Self {
        Self {
            err,
            retryable: false,
        }
    }
}

enum GatewayError {
    /// connect/429/5xx — запрос заведомо не дошёл или отвергнут до обработки.
    Transport {
        err: anyhow::Error,
        retry_after: Option<Duration>,
    },
    /// ответ потерян после отправки (таймаут, обрыв потока, чужой id):
    /// чтение повторяемо, запись могла примениться.
    Ambiguous(anyhow::Error),
    /// сессия отклонена шлюзом — нужен полный handshake и повтор вызова
    StaleSession(anyhow::Error),
    Fatal(anyhow::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_returns_structured_content() {
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"structuredContent": {"ok": true, "answer": 42}}
        });
        let out = AffineClient::tool_result("search", &body).unwrap();
        assert_eq!(out["answer"], 42);
    }

    #[test]
    fn tool_result_surfaces_rpc_error() {
        // error внутри result (top-level JSON-RPC error ловит raw_post раньше)
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"error": {"code": -32601, "message": "method not found"}}
        });
        let err = AffineClient::tool_result("nope", &body)
            .unwrap_err()
            .to_string();
        assert!(err.contains("tool nope failed"), "got: {err}");
    }

    #[test]
    fn tool_result_surfaces_gateway_ok_false_convention() {
        // шлюз отвечает логические фейлы как ok:false внутри успешного RPC
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"ok": false, "error": "инцидент батча"}
        });
        let err = AffineClient::tool_result("add_database_row", &body)
            .unwrap_err()
            .to_string();
        assert!(err.contains("инцидент батча"), "got: {err}");
    }

    #[test]
    fn text_payload_parses_json_blob_and_flags_nested_ok_false() {
        let payload = json!({
            "content": [{"type": "text", "text": "{\"ok\": false, \"error\": \"внутри блоба\"}"}]
        });
        let err = AffineClient::text_payload(&payload)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("text payload") && err.contains("внутри блоба"),
            "got: {err}"
        );
    }

    #[test]
    fn text_payload_parses_json_data_and_wraps_plain_text() {
        let data = json!({"content": [{"type": "text", "text": "{\"answer\": 7}"}]});
        assert_eq!(AffineClient::text_payload(&data).unwrap()["answer"], 7);
        let plain = json!({"content": [{"type": "text", "text": "просто текст"}]});
        assert_eq!(
            AffineClient::text_payload(&plain).unwrap(),
            json!("просто текст")
        );
    }

    #[test]
    fn text_payload_requires_content_text() {
        let no_content = json!({"result": {}});
        assert!(AffineClient::text_payload(&no_content).is_err());
    }

    #[test]
    fn op_kind_reads_and_writes_cover_gateway_tools() {
        // чтения
        for t in [
            "get_doc",
            "get_workspace",
            "list_docs",
            "list_tags",
            "list_docs_by_tag",
            "search_docs",
            "find_doc_by_title",
            "read_doc",
            "read_database_cells",
            "get_capabilities",
            "analyze_doc_fidelity",
            "export_doc_markdown",
            "export_with_fidelity_report",
            "get_mindmap",
        ] {
            assert_eq!(op_kind(t), OpKind::Read, "{t} должен быть Read");
        }
        // записи
        for t in [
            "create_doc_from_markdown",
            "create_doc",
            "create_semantic_page",
            "create_tag",
            "create_workspace",
            "create_mindmap",
            "add_mindmap_node",
            "add_tag_to_doc",
            "add_database_row",
            "append_markdown",
            "append_semantic_section",
            "append_block",
            "update_workspace",
            "update_mindmap_node",
            "update_database_cell",
            "set_mindmap_style",
            "set_mindmap_lock",
            "set_mindmap_layout",
            "delete_tag",
            "delete_workspace",
            "move_doc",
            "reparent_mindmap_node",
            "remove_tag_from_doc",
            "replace_doc_with_markdown",
            "trash_doc",
            "restore_doc",
            "upload_blob",
            "publish_doc",
            "revoke_doc",
        ] {
            assert_eq!(op_kind(t), OpKind::Write, "{t} должен быть Write");
        }
        // незнакомое = write (safe default)
        assert_eq!(op_kind("frobnicate"), OpKind::Write);
    }

    #[test]
    fn backoff_schedule_is_exponential_capped_and_jittered() {
        let mut rng = XorShift64::seeded(42);
        let base = 1000;
        // attempt 0: [0..1000], attempt 1: [0..2000], attempt 2: [0..4000]...
        for (attempt, ceiling) in [(0u64, 1000u64), (1, 2000), (2, 4000), (3, 8000)] {
            let d = backoff_delay_ms(base, attempt as usize, &mut rng);
            assert!(d <= ceiling, "attempt {attempt}: {d} > {ceiling}");
        }
        // cap: base 1000 на attempt 10 без капа был бы ~1e6 — зажат до 20_000
        for _ in 0..20 {
            assert!(backoff_delay_ms(base, 10, &mut rng) <= BACKOFF_CAP_MS);
        }
        // base 0 → пауза 0 (тесты и «без пауз»)
        assert_eq!(backoff_delay_ms(0, 5, &mut rng), 0);
    }

    #[test]
    fn retry_after_is_parsed_and_clamped() {
        assert_eq!(parse_retry_after(Some("5")), Some(Duration::from_secs(5)));
        // кламп: сервер сказал минуту — спим не дольше 30с
        assert_eq!(
            parse_retry_after(Some("600")),
            Some(Duration::from_secs(RETRY_AFTER_CAP_SECS))
        );
        assert_eq!(parse_retry_after(Some("abc")), None);
        assert_eq!(parse_retry_after(None), None);
    }

    #[test]
    fn retry_config_validates_env_values() {
        // мусор → дефолты
        let cfg = RetryConfig::from_values(Some("abc"), Some(""), Some("1.5"));
        assert_eq!(cfg.retries, DEFAULT_RETRIES);
        assert_eq!(cfg.base_ms, DEFAULT_BASE_MS);
        assert_eq!(cfg.timeout_secs, DEFAULT_TIMEOUT_SECS);
        // клампы
        let cfg = RetryConfig::from_values(Some("99"), Some("1000000"), Some("0"));
        assert_eq!(cfg.retries, MAX_RETRIES);
        assert_eq!(cfg.base_ms, 60_000);
        assert_eq!(cfg.timeout_secs, 1);
        // 0 попыток = без ретраев — валидно
        let cfg = RetryConfig::from_values(Some("0"), Some("0"), Some("5"));
        assert_eq!(cfg.retries, 0);
        assert_eq!(cfg.base_ms, 0);
        assert_eq!(cfg.timeout_secs, 5);
        // отсутствующие — дефолты
        let cfg = RetryConfig::from_values(None, None, None);
        assert_eq!(cfg.retries, DEFAULT_RETRIES);
    }

    #[test]
    fn tool_failure_detects_retryable_in_all_payload_shapes() {
        // top-level result
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"ok": false, "error": "перегрузка", "retryable": true}
        });
        let (detail, retryable) = AffineClient::tool_failure(&body).unwrap();
        assert!(detail.contains("перегрузка") && retryable);
        // structuredContent — зеркало python _ok_false: проверяется ПЕРВЫМ,
        // иначе tool_result отдал бы отказ как данные
        let sc = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"structuredContent": {"ok": false, "error": "гонка sc", "retryable": true}}
        });
        let (detail, retryable) = AffineClient::tool_failure(&sc).unwrap();
        assert!(detail.contains("гонка sc") && retryable);
        // text-blob — detail помечен формой ответа, префикса "tool … failed" нет
        let blob = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"content": [{"type": "text", "text": "{\"ok\": false, \"error\": \"гонка\", \"retryable\": true}"}]}
        });
        let (detail, retryable) = AffineClient::tool_failure(&blob).unwrap();
        assert!(detail.contains("text payload") && detail.contains("гонка") && retryable);
        assert!(
            !detail.contains("tool "),
            "префикс навешивает call_tool: {detail}"
        );
        // без флага — не ретраим
        let plain = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"ok": false, "error": "нет прав"}
        });
        let (_, retryable) = AffineClient::tool_failure(&plain).unwrap();
        assert!(!retryable);
        // успешный ответ — None
        let ok = json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true, "answer": 1}});
        assert!(AffineClient::tool_failure(&ok).is_none());
    }

    #[test]
    fn random_id_bases_do_not_collide_and_stay_js_safe() {
        // смоделированные «процессы»: разные seed — разные базы
        let mut a = XorShift64::seeded(0xDEAD_BEEF);
        let mut b = XorShift64::seeded(0xDEAD_BEEF + 1);
        let base_of = |rng: &mut XorShift64| (rng.next_u64() % (1u64 << 52)) | 1;
        let (base_a, base_b) = (base_of(&mut a), base_of(&mut b));
        assert_ne!(base_a, base_b);
        // JS-safe: шлюз на JS, id > 2^53 теряют точность (живой 400)
        assert!(base_a < 1u64 << 53);
        assert!(base_b < 1u64 << 53);
        // детерминизм: один seed → одна последовательность
        let mut x = XorShift64::seeded(7);
        let mut y = XorShift64::seeded(7);
        for _ in 0..100 {
            assert_eq!(x.next_u64(), y.next_u64());
        }
    }
}
