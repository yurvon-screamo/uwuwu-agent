//! SiYuan backend: REST (Kernel API) + MCP (/mcp) клиент и операции
//! поверх смигрированного uwuwu-воркспейса.
//!
//! Конфигурация: UWUWU_SIYUAN_URL (деф. http://127.0.0.1:6806) и
//! UWUWU_SIYUAN_TOKEN; при пустом токене — фоллбек-цепочка:
//! fish universal vars (~/.config/fish/fish_variables) → api.token из
//! $UWUWU_SIYUAN_WS/conf/conf.json (деф. ~/siyuan-demo-ws) — ядро
//! пересохраняет conf.json правами своего uid (0600), конфиг-фоллбек
//! не единственный носитель (задача 20261111013319).

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
/// Паузы между попытками авто-отката overwrite: ядро, уронившее запись,
/// лежит в рестарте (quadlet ~6с + бут ядра ~10–15с) — первые попытки
/// уходят в отказ, смысл ретраев — пережить окно рестарта. Итог — 4 попытки
/// и ~40–45с ожидания. UWUWU_TEST_RESTORE_BACKOFF_MS (мс через запятую)
/// перекрывает расписание: хук тестовый, но env читается на любом пути —
/// прод его не выставляет, чужое значение меняет прод-бэкоффы громко
/// (предупреждение в stderr + прод-константы, никогда не частично).
const RESTORE_BACKOFFS: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

fn restore_backoffs() -> Vec<Duration> {
    let Ok(schedule) = std::env::var("UWUWU_TEST_RESTORE_BACKOFF_MS") else {
        return RESTORE_BACKOFFS.to_vec();
    };
    let parsed: Result<Vec<Duration>, _> = schedule
        .split(',')
        .map(|part| part.trim().parse::<u64>().map(Duration::from_millis))
        .collect();
    match parsed {
        Ok(backoffs) if !backoffs.is_empty() => backoffs,
        _ => {
            eprintln!(
                "предупреждение: UWUWU_TEST_RESTORE_BACKOFF_MS={schedule:?} не спарсился \
                 (нужны мс через запятую) — расписание игнорируется, прод-бэкоффы"
            );
            RESTORE_BACKOFFS.to_vec()
        }
    }
}
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

/// Исход одиночной mcp-попытки (mcp_call_once): тип-носитель причины —
/// строки не переживают границы функций. StaleSession — HTTP 404 при
/// активном sid в запросе: ядро не знает сессию (реестр в памяти процесса
/// погас рестартом), вызов не дошёл до исполнения; транспорт — всё прочее.
enum McpAttemptErr {
    StaleSession,
    Transport(anyhow::Error),
}

impl From<anyhow::Error> for McpAttemptErr {
    fn from(err: anyhow::Error) -> Self {
        McpAttemptErr::Transport(err)
    }
}

impl McpAttemptErr {
    /// Перевод исхода попытки в anyhow-ошибку (handshake-пути, где recovery
    /// неприменим); StaleSession здесь невозможен по построению (запрос без
    /// активного sid) — текст ошибки фиксирует инвариант.
    fn into_transport(self) -> anyhow::Error {
        match self {
            McpAttemptErr::Transport(err) => err,
            McpAttemptErr::StaleSession => anyhow!("mcp: сессия отклонена (404) без активного sid"),
        }
    }
}

pub struct SiYuan {
    pub base: String,
    /// Токен под мьютексом: 401-триггерный рефреш мутует его из цикла запроса
    /// (задача 20261111233714: ротация api.token — самолечение без рестартов).
    token: std::sync::Mutex<String>,
    client: reqwest::Client,
    // MCP-сессия (ленивая)
    mcp_sid: std::sync::Mutex<Option<String>>,
    /// 90s fail-fast окно волны (unix-secs истечения; 0 = не армировано).
    wave_cooldown_until: AtomicU64,
    /// Инъекция мгновенного пейсинга ретраев для тестов (RowInsertPacing-прецедент
    /// PR #125): None в проде — production-константы в post().
    test_pacing: Option<RetryPacing>,
    /// Источник рефреш-токена: walk-past-equal цепочка fish → conf.json →
    /// docker exec. Дефолт — реальная цепочка; тесты подменяют замыканием.
    refresher: TokenRefresher,
}

/// Провайдер перечитанного токена: вход — провалившийся токен (stale),
/// выход — отличный от него или громкий отказ с перечнем источников.
pub(crate) type TokenRefresher = std::sync::Arc<dyn Fn(&str) -> Result<String> + Send + Sync>;

/// Корень воркспейса (FS-пробы doctor'а, конфиг-фоллбек токена):
/// UWUWU_SIYUAN_WS, иначе ~/siyuan-demo-ws.
pub(crate) fn workspace_root() -> PathBuf {
    std::env::var("UWUWU_SIYUAN_WS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("siyuan-demo-ws")
        })
}

/// Файл universal-переменных fish — штатный носитель env-канона хоста
/// (AGENTS.md: «всё в fish universal vars + environment.d»). Читается
/// напрямую, минуя env процесса: сессии и демоны, спавненные без экспорта
/// (кроны, wakeup-рестарты, factory-агенты), токен в env не несут —
/// задача 20261111013319.
fn fish_vars_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/fish/fish_variables")
}

/// Рас-экранировка значения из fish_variables: fish пишет спецсимволы как
/// `\xHH` (`:`, `.`, `-` в URL/путях), а также `\n`, `\t`, `\r`; `\\`
/// буквализирует бэкслеш. Неизвестная последовательность сохраняется дословно.
/// Граница применимости: байты ≥ 0x80 реконструируются как latin-1
/// (`byte as char`), не как UTF-8-последовательности — fish экранирует
/// не-ASCII байтами `\xHH`; ступень рассчитана на ASCII-носители
/// (токены, пути/URL со спецсимволами ASCII), не на произвольный текст.
fn fish_unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('x') => {
                chars.next();
                let hex: String = chars.by_ref().take(2).collect();
                match u8::from_str_radix(&hex, 16) {
                    Ok(byte) => out.push(byte as char),
                    Err(_) => {
                        out.push('\\');
                        out.push('x');
                        out.push_str(&hex);
                    }
                }
            }
            Some('n') => {
                chars.next();
                out.push('\n');
            }
            Some('t') => {
                chars.next();
                out.push('\t');
            }
            Some('r') => {
                chars.next();
                out.push('\r');
            }
            Some('\\') => {
                chars.next();
                out.push('\\');
            }
            _ => out.push('\\'),
        }
    }
    out
}

/// Токен из fish universal vars: строки `SETUVAR [флаги] имя:значение`,
/// значение — хвост после первого `:` имени. Побеждает последняя запись
/// (fish хранит одну запись на переменную; дубль — последнее записанное).
/// Файл отсутствует/не читается или переменной в нём нет — None.
fn token_from_fish_vars(path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    content
        .lines()
        .filter(|l| l.starts_with("SETUVAR"))
        .filter_map(|l| {
            let marker = "UWUWU_SIYUAN_TOKEN:";
            let idx = l.find(marker)?;
            if idx == 0 || !l[..idx].ends_with(' ') {
                return None; // имя не целое слово (значение чужой переменной)
            }
            Some(fish_unescape(&l[idx + marker.len()..]))
        })
        .rev()
        .find(|v| !v.is_empty())
}

/// api/token из conf.json воркспейса (последний фоллбек). Класс прав ядра:
/// stable 3.8.10 пересохраняет conf.json `0600` от uid контейнера — хост-юзер
/// не читает; ошибка различает «нет файла» и «не читается» и называет мост.
fn token_from_conf(ws_root: &std::path::Path) -> Result<String> {
    let path = ws_root.join("conf/conf.json");
    let raw = std::fs::read_to_string(&path).map_err(|err| {
        let why = if err.kind() == std::io::ErrorKind::NotFound {
            "файла нет".to_string()
        } else {
            format!(
                "не читается ({err}) — ядро пишет conf.json 0600 от uid контейнера; \
                 мост: docker exec siyuan-demo chmod 644 /siyuan/workspace/conf/conf.json"
            )
        };
        anyhow!("{}: {why}", path.display())
    })?;
    api_token_from_conf_json(&raw, &path.display().to_string())
}

/// Цепочка источников токена CLI (стойкость к пересохранениям ядра):
/// env UWUWU_SIYUAN_TOKEN → fish universal vars → conf.json воркспейса.
/// Явный env приоритетен (override); полный отказ — одна ошибка с перечнем
/// мест и подсказкой, не голый 401.
fn resolve_token(
    env: Option<String>,
    fish_path: &std::path::Path,
    ws_root: &std::path::Path,
) -> Result<String> {
    if let Some(t) = env.filter(|t| !t.is_empty()) {
        return Ok(t);
    }
    let mut attempts: Vec<String> = Vec::new();
    match token_from_fish_vars(fish_path) {
        Some(t) => return Ok(t),
        None => attempts.push(format!(
            "fish vars {} — файла нет или UWUWU_SIYUAN_TOKEN не задан",
            fish_path.display()
        )),
    }
    match token_from_conf(ws_root) {
        Ok(t) => return Ok(t),
        Err(err) => attempts.push(err.to_string()),
    }
    bail!(
        "UWUWU_SIYUAN_TOKEN не найден: env пуст; {}. \
         Носитель токена: fish `set -U UWUWU_SIYUAN_TOKEN <токен>` \
         или environment.d uwuwu-siyuan.conf.",
        attempts.join("; ")
    );
}

/// api/token из сырого conf.json (строка). Общая для conf.json на диске и
/// docker exec-канала.
fn api_token_from_conf_json(raw: &str, origin: &str) -> Result<String> {
    let conf: Value = serde_json::from_str(raw).with_context(|| format!("{origin}: не JSON"))?;
    conf.pointer("/api/token")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("{origin}: api/token не найден"))
}

/// Последнее звено рефреш-цепочки: токен изнутри контейнера. conf.json хосту
/// недоступен надёжно (ядро пересохраняет 0600 через temp+rename), docker exec
/// читает источник истины напрямую: контейнер жив ⟺ kernel жив. Имя контейнера
/// — дефолт контура. Жёсткий таймаут: зависший/медленный docker-демон (штатная
/// ситуация процедуры ротации — контейнер перезапускается) не вешает CLI
/// синхронным вызовом без предела — ровно класс тихого зависания, против
/// которого задача.
fn docker_exec_token() -> Result<String> {
    const CONTAINER: &str = "siyuan-demo";
    const CONF_IN_CONTAINER: &str = "/siyuan/workspace/conf/conf.json";
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    let mut child = std::process::Command::new("docker")
        .args(["exec", CONTAINER, "cat", CONF_IN_CONTAINER])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| anyhow!("docker недоступен ({err})"))?;
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                bail!(
                    "docker exec {CONTAINER} не ответил за {TIMEOUT:?} — демон завис или медленный"
                );
            }
            Err(err) => bail!("docker exec {CONTAINER}: {err}"),
        }
    }
    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        use std::io::Read;
        pipe.read_to_string(&mut stdout)
            .map_err(|err| anyhow!("docker exec {CONTAINER}: stdout не читается ({err})"))?;
    }
    api_token_from_conf_json(
        &stdout,
        &format!("docker exec {CONTAINER} {CONF_IN_CONTAINER}"),
    )
}

/// Рефреш-цепочка 401-самолечения, семантика walk-past-equal (пиннинг плана
/// 20261111233714, High ревью): источники опрашиваются по порядку fish vars →
/// conf.json → docker exec; значения, равные провалившемуся токену, пропускаются
/// (протухший кэш не считается находкой — иначе в окне ротации fish-кэш
/// останавливал цепочку до единственного живого источника); первый отличный —
/// находка; все отказали или везде равный — громкий отказ с перечнем источников.
/// Стартовый resolve_token остаётся first-hit: 401 доказывает подозрение кэшей
/// только на рефреш-пути.
fn refresh_walk_past_equal(
    stale: &str,
    fish_path: &std::path::Path,
    ws_root: &std::path::Path,
    docker: &dyn Fn() -> Result<String>,
) -> Result<String> {
    let mut attempts: Vec<String> = Vec::new();
    match token_from_fish_vars(fish_path) {
        Some(t) if t != stale => return Ok(t),
        Some(_) => attempts.push("fish vars: токен равен провалившемуся (кэш протух)".into()),
        None => attempts.push(format!("fish vars {} — значения нет", fish_path.display())),
    }
    match token_from_conf(ws_root) {
        Ok(t) if t != stale => return Ok(t),
        Ok(_) => attempts.push("conf.json: токен равен провалившемуся".into()),
        Err(err) => attempts.push(err.to_string()),
    }
    match docker() {
        Ok(t) if t != stale => return Ok(t),
        Ok(_) => attempts.push("docker exec: токен равен провалившемуся".into()),
        Err(err) => attempts.push(format!("docker exec: {err:#}")),
    }
    bail!(
        "отличного от провалившегося токена не нашёл ни один источник: {}; \
         ротируй токен или синхронизируй носители \
         (fish `set -U UWUWU_SIYUAN_TOKEN <свежий>` / environment.d uwuwu-siyuan.conf)",
        attempts.join("; ")
    )
}

/// Дефолтный рефрешер: чистая цепочка над живыми путями хоста.
fn default_refresher() -> TokenRefresher {
    std::sync::Arc::new(|stale: &str| {
        refresh_walk_past_equal(
            stale,
            &fish_vars_path(),
            &workspace_root(),
            &docker_exec_token,
        )
    })
}

/// Классификация исхода одиночного POST (send_post): политика ретраев
/// живёт в post_with_pacing, разбор ответа — здесь.
enum PostFailure {
    /// Транспортный отказ (pasta/kernel-down) — обычный ретрай.
    Transport(String),
    /// Rate-limit kernel'а (HTTP 429 опорный, конверт-msg фоллбэк) — fail-fast.
    RateLimited(String),
    /// metadata-not-ready lag — ретрай с бэкоффом, исчерпание армит волну.
    MetadataNotReady(String),
    /// 401/Auth failed — рефреш-класс (walk-past-equal, один на вызов).
    AuthFailed,
    /// Прочий code≠0 — обычный ретрай.
    Kernel(String),
}

impl PostFailure {
    fn as_last(&self) -> String {
        match self {
            PostFailure::Transport(msg)
            | PostFailure::RateLimited(msg)
            | PostFailure::MetadataNotReady(msg)
            | PostFailure::Kernel(msg) => msg.clone(),
            PostFailure::AuthFailed => String::new(),
        }
    }
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
        let token = resolve_token(
            std::env::var("UWUWU_SIYUAN_TOKEN").ok(),
            &fish_vars_path(),
            &workspace_root(),
        )?;
        Ok(Self {
            base,
            token: std::sync::Mutex::new(token),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(timeout_secs))
                .build()?,
            mcp_sid: std::sync::Mutex::new(None),
            wave_cooldown_until: AtomicU64::new(0),
            test_pacing: None,
            refresher: default_refresher(),
        })
    }

    /// Тестовый конструктор: поля client/mcp_sid приватны для siyuan,
    /// сиблинг-модуль siyuan_task_tests соберёт только через него.
    /// Рефрешер — герметичная заглушка (громкий отказ): тест, поймавший 401,
    /// не читает реальные носители хоста и не зовёт реальный docker;
    /// 401-сценарии — через for_tests_with_refresher.
    #[cfg(test)]
    pub(crate) fn for_tests(base: String) -> Self {
        Self {
            base,
            token: std::sync::Mutex::new("test-token".into()),
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
            refresher: std::sync::Arc::new(|_: &str| {
                anyhow::bail!(
                    "рефрешер не настроен: для 401-сценариев юзай for_tests_with_refresher"
                )
            }),
        }
    }

    /// Тестовая инъекция рефрешера: замыкание получает провалившийся токен
    /// и возвращает отличный (или громкий отказ).
    #[cfg(test)]
    pub(crate) fn for_tests_with_refresher(base: String, refresher: TokenRefresher) -> Self {
        let mut sy = Self::for_tests(base);
        sy.refresher = refresher;
        sy
    }

    /// Снапшот токена под локом — для header'ов запросов и рефреша.
    fn current_token(&self) -> String {
        self.token.lock().unwrap().clone()
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
    /// Auth-семантика (задача 20261111233714): 401 — один раз за вызов
    /// рефреш-токен walk-past-equal и немедленный повтор (самолечение
    /// ротации без рестартов); 429 — fail-fast без ретраев (каждая
    /// 401-попытка с источника продлевает rate-limit-бан kernel'а).
    /// Одиночный POST с текущим токеном: успех → data конверта; исход
    /// классифицируется в PostFailure — без ретрай-политики.
    async fn send_post(&self, path: &str, body: &Value, token: &str) -> Result<Value, PostFailure> {
        let resp = self
            .client
            .post(format!("{}{}", self.base, path))
            .header("Authorization", format!("Token {token}"))
            .json(body)
            .send()
            .await
            .map_err(|e| PostFailure::Transport(e.to_string()))?;
        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(PostFailure::RateLimited(
                "HTTP 429 — kernel rate-limit: серия неудачных токенов забанила                  источник, бан бьёт и валидные токены (10+ мин). Не долбить:                  каждая 401-попытка продлевает бан."
                    .into(),
            ));
        }
        let raw = resp.text().await.unwrap_or_default();
        let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        if v.get("code").and_then(|c| c.as_i64()) == Some(0) {
            return Ok(v.get("data").cloned().unwrap_or(Value::Null));
        }
        let last = format!(
            "code={} msg={}",
            v.get("code").and_then(|c| c.as_i64()).unwrap_or(-999),
            v.get("msg").and_then(|m| m.as_str()).unwrap_or("?")
        );
        // Вспомогательный 429-фоллбэк: конверт с rate-limit msg
        // (опорный сигнал — HTTP-статус выше).
        if last.contains("Слишком много неудачных попыток") {
            return Err(PostFailure::RateLimited(format!(
                "rate-limit конвертом — {last}. Не долбить: каждая 401-попытка продлевает бан."
            )));
        }
        if last.contains("metadata is not ready") {
            return Err(PostFailure::MetadataNotReady(last));
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || last.contains("Auth failed") {
            return Err(PostFailure::AuthFailed);
        }
        Err(PostFailure::Kernel(last))
    }

    /// Ретрай-политика: 4 попытки; metadata-lag — бэкофф по пейсингу,
    /// исчерпание армит окно волны; прочие ошибки — обычный ретрай,
    /// и окно волны НЕ армят: транспортный отказ — класс режимов 1–3
    /// (pasta/kernel-down), гасится ops-процедурами, не клиентом.
    /// Auth-семантика (задача 20261111233714): 401 — один раз за вызов
    /// рефреш-токен walk-past-equal и немедленный повтор (самолечение
    /// ротации без рестартов); 429 — fail-fast без ретраев (каждая
    /// 401-попытка с источника продлевает rate-limit-бан kernel'а).
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
        let mut auth_refreshed = false;
        for attempt in 0..4u32 {
            let token = self.current_token();
            match self.send_post(path, &body, &token).await {
                Ok(value) => return Ok(value),
                Err(PostFailure::RateLimited(reason)) => bail!("POST {path}: {reason}"),
                Err(PostFailure::MetadataNotReady(msg)) => {
                    last = msg;
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
                Err(PostFailure::AuthFailed) => {
                    if auth_refreshed {
                        bail!(
                            "POST {path}: 401 повторился после рефреша — все \
                             источники дают невалидный токен; {last}"
                        );
                    }
                    auth_refreshed = true;
                    let fresh = (self.refresher)(&token).map_err(|err| {
                        anyhow!("POST {path}: 401, рефреш не спас: {err:#}; исходный отказ: {last}")
                    })?;
                    *self.token.lock().unwrap() = fresh;
                    continue; // немедленный повтор, без ретрай-сна
                }
                Err(failure) => last = failure.as_last(),
            }
            tokio::time::sleep(std::time::Duration::from_millis(
                pacing.other_step_ms * (attempt as u64 + 1),
            ))
            .await;
        }
        if auth_refreshed {
            // Рефреш случился на исчерпанной попытке: токен структуры уже
            // свежий — следующий вызов пройдёт; не маскировать это старым 401.
            bail!("POST {path}: {last} — токен уже рефрешнут, повтори вызов со свежим");
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
            .header("Authorization", format!("Token {}", self.current_token()))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("kernel {path}: транспорт недоступен"))?;
        let status = resp.status();
        let raw = resp.text().await.unwrap_or_default();
        let envelope: Value = serde_json::from_str(&raw).with_context(|| {
            format!(
                "kernel {path}: HTTP {status}, тело не JSON: {}",
                body_snippet(&raw, REST_SNIPPET_CHARS)
            )
        })?;
        if !status.is_success() {
            bail!(
                "kernel {path}: HTTP {status}, {}",
                body_snippet(&raw, REST_SNIPPET_CHARS)
            );
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
    /// Транзакционность на пре-имидже: до мутаций экспорт текущего контента;
    /// сбой после начала очистки — авто-откат с бэкофф-ретраями (переживает
    /// рестарт ядра — инциденты 08.11: смерти процесса, каждая попытка
    /// начинается с чистой MCP-сессии), все попытки неуспешны — громкая
    /// ошибка с дампом пре-имиджа в файл. Запись нового контента — once
    /// с контрольным чтением (sync-лаг ядра прячет записанный блок — откат
    /// стёр бы легший контент).
    pub async fn overwrite_doc_md(
        &self,
        doc_id: &str,
        doc_title: &str,
        markdown: &str,
    ) -> Result<()> {
        // сбой чтения пре-имиджа ≠ «док был пуст»: Err экспорта отменяет
        // overwrite до любой мутации (класс PR #132 — чтение не врёт о пустоте)
        let raw = self.export_md_by_id(doc_id).await.with_context(|| {
            format!("overwrite {doc_id}: пре-имидж не получен — мутации отменены")
        })?;
        let pre_image = crate::markdown::strip_export_wrapper(&raw, doc_title);

        if let Err(err) = self.clear_doc_children(doc_id).await {
            return self.restore_or_fail(doc_id, &pre_image, err).await;
        }
        if markdown.trim().is_empty() {
            return Ok(());
        }
        if let Err(err) = self.append_block_md_once(doc_id, markdown).await {
            let marker = first_nonempty_line(markdown);
            if !marker.is_empty() && self.doc_contains_polled(doc_id, &marker).await {
                self.drop_leading_blank_block(doc_id).await;
                return Ok(());
            }
            return self.restore_or_fail(doc_id, &pre_image, err).await;
        }
        self.drop_leading_blank_block(doc_id).await;
        Ok(())
    }

    /// Очистка детей дока до пустоты: волны get_children → delete_block.
    /// Сбой удаления или блоки, живые после трёх волн, — Err: звавший
    /// (overwrite) решает судьбу дока через авто-откат.
    async fn clear_doc_children(&self, doc_id: &str) -> Result<()> {
        for _ in 0..3 {
            let text = self.block_children_text(doc_id).await?;
            let ids = parse_block_ids(&text);
            if doc_content_is_empty(&ids, &text) {
                return Ok(());
            }
            for block_id in ids {
                self.delete_block(&block_id).await?;
            }
        }
        bail!("не удалось очистить контент дока {doc_id}: блоки остаются после 3 волн удаления")
    }

    /// Контрольное чтение после сбоя записи: маркер (первая непустая строка
    /// записанного) в экспорте? Пауза между попытками — sync-лаг ядра прячет
    /// только что записанный блок. true — запись легла, повторять нельзя.
    async fn doc_contains_polled(&self, doc_id: &str, marker: &str) -> bool {
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            if let Ok(exported) = self.export_md_by_id(doc_id).await {
                if exported.contains(marker) {
                    return true;
                }
            }
        }
        false
    }

    /// Авто-откат overwrite: док уже очищен (частично или полностью) — вернуть
    /// пре-имидж: дочистка остатков + аппенд пре-имиджа once. Откат идёт
    /// циклом с бэкофф-ретраями: ядро, уронившее запись, лежит в рестарте
    /// (инциденты 08.11 — смерти процесса ядра, quadlet поднимает за ~6с),
    /// одиночная немедленная попытка всегда уходила в тот же мёртвый
    /// транспорт. Ретраи безопасны: попытка сходима по построению — «дочистка
    /// до пустоты + аппенд того же пре-имиджа» перезаписывает док, частично
    /// применённая или применённая-но-неподтверждённая попытка при повторе
    /// не дублирует контент. Перед каждой попыткой — инвалидация MCP-сессии
    /// (рестарт убил её на стороне ядра, стейл sid = 404 на первом же
    /// get_children) и сброс wave-cooldown (metadata-not-ready поднимающегося
    /// ядра не должен армить fail-fast окно против аварийного восстановления).
    /// Все попытки неуспешны → дамп пре-имиджа в файл 0600, громкая ошибка
    /// с путём и обеими причинами. Возвращается всегда Err (исходная
    /// причина): команда упала, контент сохранён — повтор безопасен.
    async fn restore_or_fail(
        &self,
        doc_id: &str,
        pre_image: &str,
        cause: anyhow::Error,
    ) -> Result<()> {
        if pre_image.trim().is_empty() {
            // восстанавливать нечего: док был пуст до операции
            return Err(cause);
        }
        let backoffs = restore_backoffs();
        let total_attempts = backoffs.len() + 1;
        let mut last_err = None;
        for backoff in std::iter::once(Duration::ZERO).chain(backoffs) {
            if !backoff.is_zero() {
                tokio::time::sleep(backoff).await;
            }
            self.invalidate_mcp_session();
            self.wave_cooldown_until.store(0, Ordering::Relaxed);
            let rolled_back = async {
                self.clear_doc_children(doc_id).await?;
                self.append_block_md_once(doc_id, pre_image).await
            };
            match rolled_back.await {
                Ok(()) => {
                    return Err(cause.context(format!(
                        "исходный контент дока {doc_id} восстановлен из пре-имиджа"
                    )));
                }
                Err(rollback_err) => last_err = Some(rollback_err),
            }
        }
        let Some(rollback_err) = last_err else {
            unreachable!("цикл делает хотя бы одну попытку — last_err заполнен");
        };
        let dump_path = std::env::temp_dir().join(format!("uwuwu-overwrite-preimage-{doc_id}.md"));
        // 0600 с creation: пре-имидж может быть приватным (PERSON) — окно
        // с дефолтными правами между create и set_permissions исключено
        let dump_result = (|| -> std::io::Result<()> {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&dump_path)?;
            use std::io::Write;
            file.write_all(pre_image.as_bytes())?;
            Ok(())
        })();
        let dump_note = match dump_result {
            Ok(()) => format!("исходный контент сохранён в {}", dump_path.display()),
            Err(dump_err) => format!(
                "дамп пре-имиджа в {} не удался: {dump_err}",
                dump_path.display()
            ),
        };
        bail!(
            "overwrite {doc_id}: {cause:#}; откат не удался после {total_attempts} попыток \
             ({rollback_err:#}) — {dump_note}"
        );
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
            .header("Authorization", format!("Token {}", self.current_token()))
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
            .header("Authorization", format!("Token {}", self.current_token()))
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
            .header("Authorization", format!("Token {}", self.current_token()))
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
            .header("Authorization", format!("Token {}", self.current_token()))
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

    /// Единая точка mcp-вызовов cli с recovery сессии. Рестарт ядра гасит
    /// реестр сессий (в памяти процесса): вызов с кэшированным sid получает
    /// HTTP 404 «session not found» — протокольный reject на входе
    /// streamable-http эндпоинта, вызов не дошёл до исполнения, поэтому один
    /// re-init + повтор безопасен и для мутаций (once-семантика по
    /// построению). Глубина 1 структурно: повтор идёт через mcp_call_once,
    /// recovery-путь не входит в recovery (опыт platform/SiYuan MCP-клиент,
    /// PR #60/#63). Транспортный класс (ядро лежит: connection refused,
    /// 502 шлюза) — не 404, без ретраев: смежная закрытая линия.
    async fn mcp_call(&self, method: &str, params: Value) -> Result<Value> {
        match self.mcp_call_once(method, params.clone()).await {
            Ok(v) => Ok(v),
            Err(McpAttemptErr::Transport(err)) => Err(err),
            Err(McpAttemptErr::StaleSession) => {
                self.invalidate_mcp_session();
                self.ensure_mcp_session().await.context(
                    "mcp recovery: сессия отклонена ядром (404), ре-инициализация не удалась",
                )?;
                match self.mcp_call_once(method, params).await {
                    Ok(v) => Ok(v),
                    Err(McpAttemptErr::StaleSession) => {
                        bail!("mcp {method}: сессия отклонена (404) и после ре-инициализации")
                    }
                    Err(McpAttemptErr::Transport(err)) => Err(err),
                }
            }
        }
    }

    async fn mcp_call_once(&self, method: &str, params: Value) -> Result<Value, McpAttemptErr> {
        let is_notification = method.starts_with("notifications/");
        let body = if is_notification {
            json!({ "jsonrpc": "2.0", "method": method, "params": params })
        } else {
            json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
        };
        let mut headers = vec![
            ("Authorization", format!("Token {}", self.current_token())),
            ("Content-Type", "application/json".into()),
            ("Accept", "application/json, text/event-stream".into()),
        ];
        let session_cached = {
            let guard = self.mcp_sid.lock().unwrap();
            match guard.as_ref() {
                Some(sid) => {
                    headers.push(("Mcp-Session-Id", sid.clone()));
                    true
                }
                None => false,
            }
        };
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
        // Барьеры до доверия ответу (паритет lib/platform.ts, PR #60; задачи
        // 20261109083931-ajpznm8 и 20261109095605-qxcouod): стейл-сессия
        // детектится по структурным признакам — 404 + факт активного sid в
        // запросе, не по тексту тела (формулировка ядра меняется между
        // версиями); детект до статусного bail — plain-text 404 не валит
        // JSON-парсер и не маскируется «mcp response parse», recovery
        // (re-init + один повтор) восстанавливает команду. Прочий не-2xx —
        // статусный bail со сниппетом тела (401-конверт не парсится в тихий
        // Null); конверт-детект после parse — kernel-конверт code≠0 без
        // jsonrpc поверх HTTP 200 не проходит как данные (ADR-030 «худший
        // режим отказа — тихий ноль»).
        let status = resp.status();
        let sid_hdr = resp.headers().get("mcp-session-id").cloned();
        let text = resp.text().await.context("mcp response body")?;
        if status == reqwest::StatusCode::NOT_FOUND && session_cached {
            return Err(McpAttemptErr::StaleSession);
        }
        if !status.is_success() {
            return Err(anyhow!(
                "mcp {method}: HTTP {status}: {}",
                body_snippet(&text, MCP_SNIPPET_CHARS)
            )
            .into());
        }
        if let Some(sid) = sid_hdr {
            *self.mcp_sid.lock().unwrap() =
                Some(sid.to_str().context("mcp session id header")?.to_string());
        }
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
        if v.get("jsonrpc").is_none() {
            if let Some(code) = v.get("code").and_then(|c| c.as_i64()) {
                if code != 0 {
                    let msg = v.get("msg").and_then(|m| m.as_str()).unwrap_or("?");
                    return Err(anyhow!("mcp {method}: kernel code={code} msg={msg}").into());
                }
            }
        }
        if let Some(err) = v.get("error") {
            return Err(anyhow!("mcp {method}: {err}").into());
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Handshake сессии. Идёт через mcp_call_once напрямую — recovery-путь
    /// сам не проходит через recovery (урок PR #63): initialize без sid не
    /// бывает stale, и это структурно исключает рекурсию восстановления.
    pub async fn mcp_initialize(&self) -> Result<Value> {
        let r = self
            .mcp_call_once(
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": { "name": "uwuwu-cli", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await
            .map_err(McpAttemptErr::into_transport)?;
        let _ = self
            .mcp_call_once("notifications/initialized", json!({}))
            .await;
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

    /// Сброс кэша MCP-сессии: рестарт ядра убивает сессии на его стороне
    /// (реестр в памяти процесса), tools/call со стейл sid отвечает 404
    /// «session not found». Нужен пути отката overwrite — попытка после
    /// рестарта обязана начаться с чистой сессии, иначе все ретраи падают
    /// одинаково на первом же get_children.
    fn invalidate_mcp_session(&self) {
        *self.mcp_sid.lock().unwrap() = None;
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

    /// Новая колонка database (/api/av/addAttributeViewKey): keyID генерится
    /// клиентом — ядро обязывает передавать валидный node-id; data:null,
    /// возвращаем сгенерённый id.
    pub async fn add_attr_view_key(
        &self,
        av_id: &str,
        name: &str,
        key_type: &str,
    ) -> Result<String> {
        let key_id = crate::siyuan_id::new_node_id();
        self.post(
            "/api/av/addAttributeViewKey",
            json!({
                "avID": av_id,
                "keyID": key_id,
                "keyName": name,
                "keyType": key_type,
                "keyIcon": "",
                "previousKeyID": ""
            }),
        )
        .await?;
        Ok(key_id)
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
        self.db_item_set_select(av_id, key_id, item_id, status)
            .await
    }

    /// Значение select/mSelect-ячейки: ядро апсертит опцию по content
    /// (прецедент Status). Пустой content — очистка клетки (пустой mSelect).
    pub async fn db_item_set_select(
        &self,
        av_id: &str,
        key_id: &str,
        item_id: &str,
        content: &str,
    ) -> Result<()> {
        let options = if content.is_empty() {
            Vec::new()
        } else {
            vec![json!({ "content": content })]
        };
        self.mcp_tool(
            "database",
            json!({ "action": "item_update", "id": av_id, "keyID": key_id,
                    "itemID": item_id, "value": { "mSelect": options } }),
        )
        .await?;
        Ok(())
    }

    /// Значение date-ячейки: content — epoch ms, formattedContent — строка
    /// отображения (шейп kernel av.ValueDate). isNotEmpty=false — очистка
    /// клетки (kernel считает date пустой по флагу, не по content).
    pub async fn db_item_set_date(
        &self,
        av_id: &str,
        key_id: &str,
        item_id: &str,
        content_ms: i64,
        is_not_empty: bool,
        formatted: &str,
    ) -> Result<()> {
        self.mcp_tool(
            "database",
            json!({ "action": "item_update", "id": av_id, "keyID": key_id,
                    "itemID": item_id,
                    "value": { "date": { "content": content_ms,
                                          "isNotEmpty": is_not_empty,
                                          "formattedContent": formatted } } }),
        )
        .await?;
        Ok(())
    }
}

/// Голова тела ответа для сообщений об ошибках: диагностика без простыни.
/// Лимит параметром: REST-пути ядра — 200, mcp — 120 (паритет
/// lib/platform.ts mcpHttpError, задача 20261109083931-ajpznm8).
fn body_snippet(raw: &str, limit: usize) -> String {
    let mut head: String = raw.chars().take(limit).collect();
    if raw.chars().count() > limit {
        head.push('…');
    }
    head
}

/// Лимит сниппета тела для REST-конвертов ядра (post_probe и пр.).
const REST_SNIPPET_CHARS: usize = 200;
/// Лимит сниппета тела для mcp-ошибок — паритет lib/platform.ts mcpHttpError.
const MCP_SNIPPET_CHARS: usize = 120;

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

/// Первая непустая строка markdown — маркер контрольного чтения записи
/// (подстрочный матч экспорта: отступы/переносы ядра матчу не мешают).
pub(crate) fn first_nonempty_line(md: &str) -> String {
    md.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string()
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
    use super::{
        body_snippet, doc_content_is_empty, parse_block_ids, FileFetch, RetryPacing,
        MCP_SNIPPET_CHARS, REST_SNIPPET_CHARS,
    };
    use crate::siyuan::SiYuan;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
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

    /// Env-пейсинг отката (UWUWU_TEST_RESTORE_BACKOFF_MS): валидное расписание
    /// парсится дословно; неспарсившийся/пустой env отбрасывается ЦЕЛИКОМ —
    /// прод-константы, никогда не частичный график (молчаливое отбрасывание
    /// одного элемента незаметно превратило бы график 4 попыток в 1–2 —
    /// класс замечания ревью PR #189).
    #[test]
    #[serial_test::serial]
    fn restore_backoffs_env_schedule_parses_or_discarded_whole() {
        struct EnvGuard;
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                std::env::remove_var("UWUWU_TEST_RESTORE_BACKOFF_MS");
            }
        }
        std::env::remove_var("UWUWU_TEST_RESTORE_BACKOFF_MS");
        let _guard = EnvGuard;
        let prod = vec![
            Duration::from_secs(2),
            Duration::from_secs(10),
            Duration::from_secs(30),
        ];

        // env не задан — прод-константы
        assert_eq!(super::restore_backoffs(), prod);

        // валидное расписание — парсится, пробелы вокруг запятых тримятся
        std::env::set_var("UWUWU_TEST_RESTORE_BACKOFF_MS", "5, 500");
        assert_eq!(
            super::restore_backoffs(),
            vec![Duration::from_millis(5), Duration::from_millis(500)]
        );

        // мусор, частичный мусор, пустой — env отброшен целиком (прод-график)
        for schedule in ["abc", "5,x", ""] {
            std::env::set_var("UWUWU_TEST_RESTORE_BACKOFF_MS", schedule);
            assert_eq!(
                super::restore_backoffs(),
                prod,
                "env {schedule:?} не должен давать частичный график"
            );
        }
    }

    /// Попытка отката сбрасывает wave-cooldown: metadata-not-ready до overwrite
    /// армит 90s fail-fast, и без сброса delete_block первой попытки (post())
    /// падал бы «пропущено — окно волны» — откат всегда кончался бы дампом.
    #[tokio::test]
    async fn rollback_attempt_disarms_wave_cooldown_before_first_delete() {
        let server = MockServer::start().await;
        // армим окно волны: appendBlock отдаёт metadata-not-ready ровно на
        // серию арминга (3 запроса instant-пейсинга), дальше — успех
        Mock::given(method("POST"))
            .and(path("/api/block/appendBlock"))
            .respond_with(metadata_error_template())
            .up_to_n_times(3)
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());
        sy.post_with_pacing(
            "/api/block/appendBlock",
            json!({ "id": "x" }),
            RetryPacing::instant(),
        )
        .await
        .expect_err("metadata-not-ready исчерпанием армит окно волны");
        assert!(
            sy.wave_cooldown_active(),
            "окно волны армировано перед откатом"
        );

        // путь отката: get_children отвечает ребёнком на первый вызов и пусто
        // дальше (первый tools/call — notifications/initialized, его не считаем)
        let children_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&children_calls);
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(move |req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("notifications/") {
                    return ResponseTemplate::new(200);
                }
                if body.contains("\"initialize\"") {
                    return ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":1}}}"#,
                        )
                        .append_header("mcp-session-id", "sess-ok-barrier");
                }
                let n = calls.fetch_add(1, Ordering::SeqCst);
                let text = if n == 0 {
                    "- [p] старый блок (blk-1)".to_string()
                } else {
                    String::new()
                };
                ResponseTemplate::new(200).set_body_string(
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": 2,
                        "result": { "content": [{ "type": "text", "text": text }] }
                    })
                    .to_string(),
                )
            })
            .mount(&server)
            .await;
        // deleteBlock и appendBlock-успех (последний смонтированный мок
        // перекрывает metadata-error — wiremock берёт свежайший матч)
        Mock::given(method("POST"))
            .and(path("/api/block/deleteBlock"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":null})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/block/appendBlock"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":null})))
            .mount(&server)
            .await;

        let err = sy
            .restore_or_fail("doc-1", "старый контент\n", anyhow::anyhow!("boom"))
            .await
            .expect_err("restore_or_fail всегда Err");
        let text = format!("{err:#}");
        assert!(
            text.contains("восстановлен"),
            "откат прошёл при армированном окне: {text}"
        );
        assert!(
            !sy.wave_cooldown_active(),
            "окно волны сброшено попыткой отката"
        );
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
        let snippet = body_snippet(&long, REST_SNIPPET_CHARS);
        assert_eq!(snippet.chars().count(), 201);
        assert!(snippet.ends_with('…'));
        assert_eq!(body_snippet("short", REST_SNIPPET_CHARS), "short");
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

    /// Состояние сессионного мока ядра — контракт живого ядра (проба 09.11,
    /// протокол streamable-http): реестр сессий в памяти «процесса», рестарт
    /// = гашение поколения; tools/call с чужим/погасшим sid отвечает 404
    /// plain-text, и reject — не исполнение (счётчики раздельные).
    struct SessionKernel {
        inits: AtomicUsize,
        tool_oks: AtomicUsize,
        tool_404s: AtomicUsize,
        session_gen: AtomicUsize,
        /// Гасить сессию после N успешных tools/call (симулятор рестарта).
        restart_after_tools_calls: usize,
        /// «Зомби-ядро»: tools/call отклоняются даже со свежей сессией —
        /// recovery не вылечивает (второй 404 подряд).
        zombie: bool,
    }

    impl SessionKernel {
        fn new(restart_after_tools_calls: usize, zombie: bool) -> Arc<Self> {
            Arc::new(Self {
                inits: AtomicUsize::new(0),
                tool_oks: AtomicUsize::new(0),
                tool_404s: AtomicUsize::new(0),
                session_gen: AtomicUsize::new(0),
                restart_after_tools_calls,
                zombie,
            })
        }

        fn inits(&self) -> usize {
            self.inits.load(Ordering::SeqCst)
        }

        fn tool_oks(&self) -> usize {
            self.tool_oks.load(Ordering::SeqCst)
        }

        fn tool_404s(&self) -> usize {
            self.tool_404s.load(Ordering::SeqCst)
        }
    }

    /// Сессионное ядро: initialize выдаёт новое поколение sid, tools/call
    /// живёт по сессионному контракту выше.
    async fn mount_session_kernel(server: &MockServer, kernel: Arc<SessionKernel>) {
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(move |req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    let gen = kernel.inits.fetch_add(1, Ordering::SeqCst) + 1;
                    kernel.session_gen.store(gen, Ordering::SeqCst);
                    return ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":1}}}"#,
                        )
                        .append_header("mcp-session-id", format!("sess-mock-{gen}"));
                }
                if body.contains("notifications/") {
                    return ResponseTemplate::new(200);
                }
                let gen = kernel.session_gen.load(Ordering::SeqCst);
                let got = req
                    .headers
                    .get("mcp-session-id")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if kernel.zombie || gen == 0 || got != format!("sess-mock-{gen}") {
                    kernel.tool_404s.fetch_add(1, Ordering::SeqCst);
                    return ResponseTemplate::new(404).set_body_string("session not found");
                }
                let oks = kernel.tool_oks.fetch_add(1, Ordering::SeqCst) + 1;
                if oks == kernel.restart_after_tools_calls {
                    kernel.session_gen.store(0, Ordering::SeqCst);
                }
                ResponseTemplate::new(200).set_body_string(
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": 2,
                        "result": { "content": [{ "type": "text", "text": "ok" }] }
                    })
                    .to_string(),
                )
            })
            .mount(server)
            .await;
    }

    /// Мок MCP-сервера без кэша сессии (initialize не выдаёт sid): tools/call —
    /// сырой HTTP-ответ (статус+тело), репликация нестандартных отказов ядра
    /// не-сессионного класса — 404 plain-text без активной сессии (задача
    /// 20261109083931-ajpznm8), 401-конверт, kernel-конверт поверх 200.
    /// Сессионный класс 404+sid (recovery) покрывает SessionKernel выше.
    async fn mount_mcp_tool_raw_response(server: &MockServer, status: u16, body: &'static str) {
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(move |req: &wiremock::Request| {
                let req_body = String::from_utf8_lossy(&req.body);
                if req_body.contains("\"initialize\"") {
                    return ResponseTemplate::new(200).set_body_string(
                        r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":1}}}"#,
                    );
                }
                if req_body.contains("notifications/") {
                    return ResponseTemplate::new(200);
                }
                ResponseTemplate::new(status).set_body_string(body)
            })
            .mount(server)
            .await;
    }

    /// Рестарт ядра гасит сессию посреди последовательности вызовов: 404 по
    /// стейл sid → ровно один re-init → повтор исходного вызова доезжает.
    /// Reject (404) не считается исполнением — once-семантика повтора.
    #[tokio::test]
    async fn mcp_call_recovers_stale_session_with_single_reinit() {
        let server = MockServer::start().await;
        let kernel = SessionKernel::new(1, false);
        mount_session_kernel(&server, Arc::clone(&kernel)).await;
        let sy = SiYuan::for_tests(server.uri());

        sy.mcp_initialize()
            .await
            .expect("первый handshake кэширует sess-mock-1");
        sy.mcp_call("tools/call", json!({ "name": "database", "arguments": {} }))
            .await
            .expect("первый вызов с живой сессией");

        sy.mcp_call("tools/call", json!({ "name": "database", "arguments": {} }))
            .await
            .expect("recovery вытягивает стейл-сессию");

        assert_eq!(
            kernel.inits(),
            2,
            "ровно один recovery re-init: {}",
            kernel.inits()
        );
        assert_eq!(
            kernel.tool_oks(),
            2,
            "исполнения: первый вызов + повтор, reject не считается: {}",
            kernel.tool_oks()
        );
        assert_eq!(
            kernel.tool_404s(),
            1,
            "один 404 до recovery: {}",
            kernel.tool_404s()
        );
    }

    /// Ядро отклоняет tools/call даже со свежей сессией: recovery глубины 1 —
    /// ровно один re-init, затем громкая ошибка, без цикла.
    #[tokio::test]
    async fn mcp_call_gives_up_after_single_recovery_when_kernel_rejects_again() {
        let server = MockServer::start().await;
        let kernel = SessionKernel::new(usize::MAX, true);
        mount_session_kernel(&server, Arc::clone(&kernel)).await;
        let sy = SiYuan::for_tests(server.uri());

        sy.mcp_initialize()
            .await
            .expect("handshake кэширует сессию");
        let err = sy
            .mcp_call("tools/call", json!({ "name": "database", "arguments": {} }))
            .await
            .expect_err("зомби-ядро отклоняет и после re-init — громкая ошибка");

        let text = format!("{err:#}");
        assert!(text.contains("404"), "ошибка называет класс отказа: {text}");
        assert_eq!(
            kernel.inits(),
            2,
            "глубина 1 — без цикла re-init: {}",
            kernel.inits()
        );
        assert_eq!(
            kernel.tool_404s(),
            2,
            "два отклонения: исходный вызов + повтор: {}",
            kernel.tool_404s()
        );
    }

    /// 404 без активного sid в запросе — не стейл-сессия: recovery не
    /// запускается, ровно одна HTTP-попытка (иначе initialize-путь циклил бы).
    #[tokio::test]
    async fn mcp_call_without_cached_session_does_not_retry_on_404() {
        let server = MockServer::start().await;
        let http_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&http_calls);
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(move |_: &wiremock::Request| {
                calls.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(404).set_body_string("session not found")
            })
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .mcp_call("tools/call", json!({ "name": "database", "arguments": {} }))
            .await
            .expect_err("404 без sid — ошибка без recovery-цикла");

        let text = format!("{err:#}");
        assert!(
            text.contains("HTTP 404") && text.contains("session not found"),
            "не-сессионный 404 — статусный bail со статусом и телом: {text}"
        );
        assert!(
            !text.contains("mcp response parse"),
            "parse-маскировка устранена (ajpznm8): {text}"
        );
        assert_eq!(
            http_calls.load(Ordering::SeqCst),
            1,
            "ровно одна HTTP-попытка: {}",
            http_calls.load(Ordering::SeqCst)
        );
    }

    /// Транспортный класс (5xx) — не стейл-сессия: recovery не запускается,
    /// ровно одна попытка вызова с исходной ошибкой (ретраи/восстановление —
    /// вне среза, смежная закрытая линия авто-ретраев).
    #[tokio::test]
    async fn mcp_call_does_not_recover_on_transport_error() {
        let server = MockServer::start().await;
        let inits = Arc::new(AtomicUsize::new(0));
        let tool_calls = Arc::new(AtomicUsize::new(0));
        let (ini, calls) = (Arc::clone(&inits), Arc::clone(&tool_calls));
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(move |req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    ini.fetch_add(1, Ordering::SeqCst);
                    return ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":1}}}"#,
                        )
                        .append_header("mcp-session-id", "sess-mock-t");
                }
                if body.contains("notifications/") {
                    return ResponseTemplate::new(200);
                }
                calls.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(500).set_body_string("boom")
            })
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        sy.mcp_initialize()
            .await
            .expect("handshake кэширует сессию");
        let err = sy
            .mcp_call("tools/call", json!({ "name": "database", "arguments": {} }))
            .await
            .expect_err("5xx — транспортная ошибка без recovery");

        assert!(
            format!("{err:#}").contains("HTTP 500") && format!("{err:#}").contains("boom"),
            "статусный bail несёт статус и тело, recovery не запускался: {err:#}"
        );
        assert!(
            !format!("{err:#}").contains("mcp response parse"),
            "parse-маскировка устранена (ajpznm8): {err:#}"
        );
        assert_eq!(
            inits.load(Ordering::SeqCst),
            1,
            "recovery не запускался (только стартовый handshake): {}",
            inits.load(Ordering::SeqCst)
        );
        assert_eq!(
            tool_calls.load(Ordering::SeqCst),
            1,
            "ровно одна попытка вызова: {}",
            tool_calls.load(Ordering::SeqCst)
        );
    }

    /// Отказ re-init (ядро поднялось, но handshake не удался): громкая ошибка
    /// с контекстом recovery, ровно один ре-инициализирующий handshake, без
    /// циклов и без повторов исходного вызова.
    #[tokio::test]
    async fn mcp_call_loud_error_when_reinit_fails() {
        let server = MockServer::start().await;
        let inits = Arc::new(AtomicUsize::new(0));
        let tool_404s = Arc::new(AtomicUsize::new(0));
        let (ini, rejects) = (Arc::clone(&inits), Arc::clone(&tool_404s));
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(move |req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    let n = ini.fetch_add(1, Ordering::SeqCst) + 1;
                    if n == 1 {
                        return ResponseTemplate::new(200)
                            .set_body_string(
                                r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":1}}}"#,
                            )
                            .append_header("mcp-session-id", "sess-mock-r");
                    }
                    return ResponseTemplate::new(503).set_body_string("unavailable");
                }
                if body.contains("notifications/") {
                    return ResponseTemplate::new(200);
                }
                rejects.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(404).set_body_string("session not found")
            })
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        sy.mcp_initialize()
            .await
            .expect("handshake кэширует сессию");
        let err = sy
            .mcp_call("tools/call", json!({ "name": "database", "arguments": {} }))
            .await
            .expect_err("отказ re-init — громкая ошибка с контекстом");

        let text = format!("{err:#}");
        assert!(
            text.contains("ре-инициализация не удалась"),
            "контекст recovery в цепочке ошибки: {text}"
        );
        assert!(
            text.contains("HTTP 503") && text.contains("unavailable"),
            "исходная причина отказа handshake сохранена со статусом (ajpznm8): {text}"
        );
        assert_eq!(
            inits.load(Ordering::SeqCst),
            2,
            "стартовый handshake + ровно один re-init: {}",
            inits.load(Ordering::SeqCst)
        );
        assert_eq!(
            tool_404s.load(Ordering::SeqCst),
            1,
            "повтора исходного вызова нет — упало на re-init: {}",
            tool_404s.load(Ordering::SeqCst)
        );
    }

    /// Режим 1 (parse-маскировка): tools/call без активной сессии получает
    /// 404 plain-text «session not found» — ошибка несёт статус и тело,
    /// не валится в «mcp response parse» (задача 20261109083931-ajpznm8).
    #[tokio::test]
    async fn mcp_tool_404_plain_text_bails_with_status_and_body() {
        let server = MockServer::start().await;
        mount_mcp_tool_raw_response(&server, 404, "session not found").await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .mcp_tool("database", json!({ "action": "get_children" }))
            .await
            .expect_err("stale-session 404 must bail, not parse-fail");

        let text = err.to_string();
        assert!(text.contains("HTTP 404"), "статус в ошибке: {text}");
        assert!(text.contains("session not found"), "тело в ошибке: {text}");
        assert!(
            !text.contains("mcp response parse"),
            "parse-маскировка устранена: {text}"
        );
    }

    /// Режим 2 (тихий Null на не-2xx): 401 с kernel-конвертом `{}` — не-JSON
    /// конверт при JSON-парсимом теле; ошибка со статусом, не тихий Ok(Null).
    #[tokio::test]
    async fn mcp_tool_401_json_envelope_bails_with_status() {
        let server = MockServer::start().await;
        mount_mcp_tool_raw_response(&server, 401, "{}").await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .mcp_tool("database", json!({ "action": "get_children" }))
            .await
            .expect_err("401 must bail, not parse into silent Null");

        let text = err.to_string();
        assert!(text.contains("HTTP 401"), "статус в ошибке: {text}");
    }

    /// Режим 3 (тихий Null на HTTP 200 с конвертом ошибки, ADR-030):
    /// kernel-конверт code≠0 без jsonrpc поверх успешного HTTP — ошибка
    /// с кодом и msg, не тихий Ok(Null), который downstream читает как данные.
    #[tokio::test]
    async fn mcp_tool_200_kernel_envelope_code_nonzero_bails() {
        let server = MockServer::start().await;
        mount_mcp_tool_raw_response(&server, 200, r#"{"code":-1,"msg":"auth failed"}"#).await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .mcp_tool("database", json!({ "action": "get_children" }))
            .await
            .expect_err("kernel envelope with code≠0 must bail, not pass as Null");

        let text = err.to_string();
        assert!(text.contains("kernel code=-1"), "код в ошибке: {text}");
        assert!(text.contains("auth failed"), "msg в ошибке: {text}");
    }

    /// Статусный bail — только не-2xx: 202 с пустым телом (нотификация)
    /// проходит успех-путём как тихий пустой результат (задача
    /// 20261109083931-ajpznm8; `is_success()` включает 202).
    #[tokio::test]
    async fn mcp_tool_202_notification_empty_body_passes_as_null() {
        let server = MockServer::start().await;
        mount_mcp_tool_raw_response(&server, 202, "").await;
        let sy = SiYuan::for_tests(server.uri());

        let v = sy
            .mcp_tool("database", json!({ "action": "get_children" }))
            .await
            .expect("202 notification must pass, not bail");

        assert!(
            v.get("isError").is_none() && v.get("code").is_none(),
            "202 не должен нести маркеров ошибки: {v}"
        );
    }

    /// Граница конверт-детекта: code=0 — успех ядра, не ошибка (bail только
    /// на code≠0; задача 20261109083931-ajpznm8).
    #[tokio::test]
    async fn mcp_tool_200_envelope_code_zero_passes_as_data() {
        let server = MockServer::start().await;
        mount_mcp_tool_raw_response(&server, 200, r#"{"code":0,"msg":""}"#).await;
        let sy = SiYuan::for_tests(server.uri());

        sy.mcp_tool("database", json!({ "action": "get_children" }))
            .await
            .expect("code=0 envelope must pass, not bail");
    }

    /// msg отсутствует в kernel-конверте — плейсхолдер «?» в ошибке (cli-канон
    /// require_payload_ok; задача 20261109083931-ajpznm8).
    #[tokio::test]
    async fn mcp_tool_200_envelope_without_msg_bails_with_question() {
        let server = MockServer::start().await;
        mount_mcp_tool_raw_response(&server, 200, r#"{"code":-32601}"#).await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .mcp_tool("database", json!({ "action": "get_children" }))
            .await
            .expect_err("code≠0 without msg must bail with ? placeholder");

        let text = err.to_string();
        assert!(
            text.contains("kernel code=-32601 msg=?"),
            "плейсхолдер ? в ошибке: {text}"
        );
    }

    /// Канонический jsonrpc-конверт с error-веткой барьерится error-веткой
    /// («mcp {method}: {err}»), не конверт-детектом: jsonrpc присутствует —
    /// порядок проверок после parse (паритет lib/platform.ts; задача
    /// 20261109083931-ajpznm8).
    #[tokio::test]
    async fn mcp_tool_200_jsonrpc_error_envelope_bails_on_error_branch() {
        let server = MockServer::start().await;
        mount_mcp_tool_raw_response(
            &server,
            200,
            r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32000,"message":"transport rpc error"}}"#,
        )
        .await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .mcp_tool("database", json!({ "action": "get_children" }))
            .await
            .expect_err("jsonrpc error envelope must bail on error branch");

        let text = err.to_string();
        assert!(
            text.contains("transport rpc error"),
            "error-ветка в ошибке: {text}"
        );
        assert!(
            !text.contains("kernel code="),
            "конверт-детект не должен перехватывать jsonrpc-конверт: {text}"
        );
    }

    /// Сниппет тела в mcp-ошибке — лимит 120 + многоточие (паритет
    /// lib/platform.ts mcpHttpError; REST-канон 200 не задет — задача
    /// 20261109083931-ajpznm8).
    #[tokio::test]
    async fn mcp_tool_404_snippet_truncated_at_mcp_limit() {
        let server = MockServer::start().await;
        let long_body: &'static str = Box::leak("y".repeat(300).into_boxed_str());
        mount_mcp_tool_raw_response(&server, 404, long_body).await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .mcp_tool("database", json!({ "action": "get_children" }))
            .await
            .expect_err("long 404 body must bail with truncated snippet");

        let text = err.to_string();
        assert!(text.contains("HTTP 404"), "статус в ошибке: {text}");
        assert!(text.contains('…'), "обрезка обозначена: {text}");
        let ys = text.matches('y').count();
        assert_eq!(
            ys, MCP_SNIPPET_CHARS,
            "сниппет mcp = 120 символов тела: {text}"
        );
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

    // ---------- резолвер токена: env → fish vars → conf.json ----------
    // (задача 20261111013319: stable 3.8.10 пересохраняет conf.json 0600 от
    // uid контейнера — конфиг-фоллбек умирал, сессии без env токена падали)

    use super::{fish_unescape, resolve_token, token_from_conf, token_from_fish_vars};

    fn write_fish_file(dir: &std::path::Path, content: &str) -> std::path::PathBuf {
        let path = dir.join("fish_variables");
        std::fs::write(&path, content).expect("fish_variables written");
        path
    }

    fn write_conf_token(ws: &std::path::Path, token: &str) {
        std::fs::create_dir_all(ws.join("conf")).expect("conf dir");
        std::fs::write(
            ws.join("conf/conf.json"),
            json!({ "api": { "token": token } }).to_string(),
        )
        .expect("conf.json written");
    }

    #[test]
    fn resolve_token_prefers_env_over_fish_and_conf() {
        let dir = tempfile::tempdir().expect("tmp");
        let fish = write_fish_file(
            dir.path(),
            "SETUVAR --export UWUWU_SIYUAN_TOKEN:fish-token\n",
        );
        // conf.json в ws нет: env-ветка не должна до него дойти
        let got =
            resolve_token(Some("env-token".into()), &fish, dir.path()).expect("env override wins");
        assert_eq!(got, "env-token");
    }

    #[test]
    fn resolve_token_ignores_empty_env_value() {
        let dir = tempfile::tempdir().expect("tmp");
        let fish = write_fish_file(
            dir.path(),
            "SETUVAR --export UWUWU_SIYUAN_TOKEN:fish-token\n",
        );
        let got = resolve_token(Some(String::new()), &fish, dir.path())
            .expect("пустой env не стопорит цепочку");
        assert_eq!(got, "fish-token");
    }

    #[test]
    fn resolve_token_falls_back_to_fish_vars_when_env_missing() {
        let dir = tempfile::tempdir().expect("tmp");
        let fish = write_fish_file(
            dir.path(),
            "SETUVAR --export UWUWU_AFFINE_EMAIL:agent@uwuwu.net\n\
             SETUVAR --export UWUWU_SIYUAN_TOKEN:deadbeefcafe1234\n",
        );
        // conf.json в ws нет: живой класс 0600 — conf-фоллбек недоступен
        let got = resolve_token(None, &fish, dir.path()).expect("fish vars выручают");
        assert_eq!(got, "deadbeefcafe1234");
    }

    #[test]
    fn resolve_token_falls_back_to_conf_when_fish_misses() {
        let dir = tempfile::tempdir().expect("tmp");
        let fish = write_fish_file(dir.path(), "SETUVAR fish_color_normal:normal\n");
        write_conf_token(dir.path(), "conf-token");
        let got = resolve_token(None, &fish, dir.path()).expect("conf.json фоллбек жив");
        assert_eq!(got, "conf-token");
    }

    #[test]
    fn resolve_token_full_miss_reports_every_source() {
        let dir = tempfile::tempdir().expect("tmp");
        let fish = write_fish_file(dir.path(), "SETUVAR fish_color_normal:normal\n");
        let err =
            resolve_token(None, &fish, dir.path()).expect_err("полный отказ — громкая ошибка");
        let text = err.to_string();
        assert!(
            text.contains("UWUWU_SIYUAN_TOKEN не найден"),
            "названа переменная: {text}"
        );
        assert!(text.contains("fish_variables"), "назван fish-путь: {text}");
        assert!(text.contains("conf/conf.json"), "назван conf-путь: {text}");
        assert!(text.contains("set -U"), "есть подсказка носителя: {text}");
    }

    #[test]
    fn fish_vars_parses_export_flagged_entry_and_skips_foreign_lines() {
        let dir = tempfile::tempdir().expect("tmp");
        let fish = write_fish_file(
            dir.path(),
            "# comment\n\
             SETUVAR fish_color_normal:normal\n\
             SETUVAR --export UWUWU_AFFINE_PASSWORD:xx\n\
             SETUVAR --export UWUWU_SIYUAN_TOKEN:deadbeefcafe1234\n",
        );
        assert_eq!(
            token_from_fish_vars(&fish).as_deref(),
            Some("deadbeefcafe1234")
        );
    }

    #[test]
    fn fish_vars_last_entry_wins_and_empty_skipped() {
        let dir = tempfile::tempdir().expect("tmp");
        let fish = write_fish_file(
            dir.path(),
            "SETUVAR --export UWUWU_SIYUAN_TOKEN:old\n\
             SETUVAR --export UWUWU_SIYUAN_TOKEN:new\n\
             SETUVAR --export UWUWU_SIYUAN_TOKEN:\n",
        );
        assert_eq!(token_from_fish_vars(&fish).as_deref(), Some("new"));
    }

    /// Строгая граница имени: маркер `UWUWU_SIYUAN_TOKEN:` внутри значения
    /// чужой переменной или внутри чужого имени (префикс) — не матч.
    /// Регресс фильтра «имя после пробела» сматчил бы чужой секрет.
    #[test]
    fn fish_vars_strict_name_boundary_skips_marker_inside_foreign_value() {
        let dir = tempfile::tempdir().expect("tmp");
        let fish = write_fish_file(
            dir.path(),
            "SETUVAR --export OTHERVAR:prefix-UWUWU_SIYUAN_TOKEN:leak-tail\n\
             SETUVAR MY_UWUWU_SIYUAN_TOKEN:leak-prefix-name\n",
        );
        assert_eq!(token_from_fish_vars(&fish), None);
    }

    #[test]
    fn fish_vars_missing_file_is_none() {
        let dir = tempfile::tempdir().expect("tmp");
        assert_eq!(token_from_fish_vars(&dir.path().join("absent")), None);
    }

    #[test]
    fn fish_unescape_decodes_hex_escapes_in_paths_and_urls() {
        assert_eq!(
            fish_unescape("http\\x3a//127\\x2e0\\x2e0\\x2e1\\x3a6806"),
            "http://127.0.0.1:6806"
        );
        assert_eq!(
            fish_unescape("/home/yurvon/siyuan\\x2ddemo\\x2dws"),
            "/home/yurvon/siyuan-demo-ws"
        );
    }

    #[test]
    fn fish_unescape_literal_and_unknown_sequences() {
        assert_eq!(fish_unescape("a\\\\b"), "a\\b");
        assert_eq!(fish_unescape("line\\nnext"), "line\nnext");
        // \xZZ не hex — сохраняется дословно
        assert_eq!(fish_unescape("\\xZZ"), "\\xZZ");
        // хвост без двух hex-цифр — дословно
        assert_eq!(fish_unescape("a\\x"), "a\\x");
    }

    #[test]
    fn token_from_conf_reads_api_token() {
        let dir = tempfile::tempdir().expect("tmp");
        write_conf_token(dir.path(), "conf-token");
        assert_eq!(
            token_from_conf(dir.path()).expect("api/token прочитан"),
            "conf-token"
        );
    }

    #[test]
    fn token_from_conf_missing_file_named_as_absent() {
        let dir = tempfile::tempdir().expect("tmp");
        let err = token_from_conf(dir.path()).expect_err("нет файла — громко");
        let text = err.to_string();
        assert!(text.contains("файла нет"), "причина названа: {text}");
        assert!(text.contains("conf/conf.json"), "путь назван: {text}");
    }

    #[cfg(unix)]
    #[test]
    fn token_from_conf_unreadable_file_names_the_bridge() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tmp");
        write_conf_token(dir.path(), "conf-token");
        let path = dir.path().join("conf/conf.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");
        let err = token_from_conf(dir.path()).expect_err("не читается — громко");
        let text = err.to_string();
        assert!(text.contains("не читается"), "класс прав назван: {text}");
        assert!(
            text.contains("chmod 644"),
            "мост назван (класс 0600 от uid контейнера): {text}"
        );
    }

    #[test]
    fn token_from_conf_non_string_token_is_an_error() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(dir.path().join("conf")).expect("conf dir");
        std::fs::write(
            dir.path().join("conf/conf.json"),
            json!({ "api": { "token": 42 } }).to_string(),
        )
        .expect("conf.json written");
        let err = token_from_conf(dir.path()).expect_err("не-строка — громко");
        assert!(err.to_string().contains("api/token не найден"), "{err}");
    }

    // ---------- рефреш-цепочка 401: walk-past-equal + docker exec ----------
    // (задача 20261111233714: ротация api.token — самолечение без рестартов;
    // семантика пиннингнута планом по High ревью: равные провалившемуся
    // значения пропускаются, первый отличный — находка)

    use super::{api_token_from_conf_json, refresh_walk_past_equal, TokenRefresher};
    use anyhow::Result;

    fn docker_serving(token: &'static str) -> impl Fn() -> Result<String> {
        move || Ok(token.to_string())
    }

    #[test]
    fn refresh_skips_stale_equal_fish_and_takes_docker() {
        let dir = tempfile::tempdir().expect("tmp");
        // Ключевой кейс главного сценария: ротация сделана, fish-кэш ещё
        // протухший (равен провалившемуся) — цепочка доходит до docker.
        let fish = write_fish_file(
            dir.path(),
            "SETUVAR --export UWUWU_SIYUAN_TOKEN:stale-token\n",
        );
        let got = refresh_walk_past_equal(
            "stale-token",
            &fish,
            dir.path(),
            &docker_serving("fresh-token"),
        )
        .expect("протухший кэш не останавливает цепочку");
        assert_eq!(got, "fresh-token");
    }

    #[test]
    fn refresh_takes_first_distinct_source_fish_over_docker() {
        let dir = tempfile::tempdir().expect("tmp");
        // fish обновлён владельцем по ранбуку → берётся он, docker не зовётся.
        let fish = write_fish_file(
            dir.path(),
            "SETUVAR --export UWUWU_SIYUAN_TOKEN:rotated-token\n",
        );
        let got = refresh_walk_past_equal(
            "stale-token",
            &fish,
            dir.path(),
            &docker_serving("fresh-token"),
        )
        .expect("первый отличный источник — находка");
        assert_eq!(got, "rotated-token");
    }

    #[test]
    fn refresh_falls_through_missing_fish_and_missing_conf_to_docker() {
        let dir = tempfile::tempdir().expect("tmp");
        // fish-файла нет, conf.json нет — docker exec остаётся живым каналом.
        let got = refresh_walk_past_equal(
            "stale-token",
            &dir.path().join("fish_variables"),
            dir.path(),
            &docker_serving("fresh-token"),
        )
        .expect("docker exec — последнее звено");
        assert_eq!(got, "fresh-token");
    }

    #[test]
    fn refresh_full_miss_reports_every_source_and_names_the_fix() {
        let dir = tempfile::tempdir().expect("tmp");
        let fish = write_fish_file(
            dir.path(),
            "SETUVAR --export UWUWU_SIYUAN_TOKEN:stale-token\n",
        );
        let docker_also_stale = docker_serving("stale-token");
        let err = refresh_walk_past_equal("stale-token", &fish, dir.path(), &docker_also_stale)
            .expect_err("все источники равны провалившемуся — громкий отказ");
        let msg = format!("{err:#}");
        assert!(msg.contains("fish vars"), "fish упомянут: {msg}");
        assert!(msg.contains("conf.json"), "conf упомянут: {msg}");
        assert!(msg.contains("docker exec"), "docker упомянут: {msg}");
        assert!(msg.contains("равен провалившемуся"), "причина видна: {msg}");
        assert!(msg.contains("set -U"), "подсказка носителя: {msg}");
    }

    #[test]
    fn refresh_reports_docker_failure_when_it_is_the_only_channel() {
        let dir = tempfile::tempdir().expect("tmp");
        let docker_dead = || -> Result<String> {
            anyhow::bail!("docker недоступен (нет сокета)")
        };
        let err = refresh_walk_past_equal(
            "stale-token",
            &dir.path().join("fish_variables"),
            dir.path(),
            &docker_dead,
        )
        .expect_err("docker-отказ виден в перечне");
        assert!(format!("{err:#}").contains("docker недоступен"), "{err}");
    }

    #[test]
    fn api_token_from_conf_json_extracts_api_token() {
        let raw = json!({ "api": { "token": "abc123" } }).to_string();
        let got = api_token_from_conf_json(&raw, "probe").expect("токен извлечён");
        assert_eq!(got, "abc123");
    }

    #[test]
    fn api_token_from_conf_json_names_origin_on_missing_field() {
        let raw = json!({ "foo": 1 }).to_string();
        let err = api_token_from_conf_json(&raw, "docker exec probe").expect_err("нет api/token");
        assert!(err.to_string().contains("docker exec probe"), "{err}");
    }

    // docker_exec_token: прод-функция с внешним docker — без сети не зовётся;
    // её контракт (парсинг вывода) покрыт api_token_from_conf_json-тестами.

    #[tokio::test]
    async fn post_refreshes_token_once_on_401_and_repeats_with_fresh_header() {
        let server = MockServer::start().await;
        // Первый запрос — провалившийся токен; повтор — свежий: два мока
        // матчат по Authorization-заголовку, ровно по одному запросу каждый.
        Mock::given(method("POST"))
            .and(path("/api/system/version"))
            .and(wiremock::matchers::header(
                "Authorization",
                "Token test-token",
            ))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_string(r#"{"code":-1,"msg":"Auth failed [header: Authorization]"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/system/version"))
            .and(wiremock::matchers::header(
                "Authorization",
                "Token fresh-token",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"code":0,"msg":"","data":"3.8.10"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;
        let refresher: TokenRefresher = Arc::new(|stale: &str| {
            assert_eq!(stale, "test-token", "рефреш получает провалившийся токен");
            Ok("fresh-token".into())
        });
        let sy = SiYuan::for_tests_with_refresher(server.uri(), refresher);

        let v = sy.post("/api/system/version", json!({})).await;
        assert!(v.is_ok(), "повтор со свежим токеном должен пройти: {v:?}");
        assert_eq!(v.unwrap(), json!("3.8.10"));
        // Токен структуры обновлён: следующие запросы идут со свежим.
        assert_eq!(sy.current_token(), "fresh-token");
    }

    #[tokio::test]
    async fn post_bails_without_retries_when_refresh_returns_same_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/system/version"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_string(r#"{"code":-1,"msg":"Auth failed [header: Authorization]"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;
        let refresher: TokenRefresher = Arc::new(|_| anyhow::bail!("отличного токена не нашёл"));
        let sy = SiYuan::for_tests_with_refresher(server.uri(), refresher);

        let err = sy
            .post("/api/system/version", json!({}))
            .await
            .expect_err("рефреш не спас — громко, без долбления");
        assert!(
            err.to_string().contains("рефреш не спас"),
            "причина рефреша видна: {err}"
        );
        server.verify().await; // expect(1): ровно один запрос
    }

    #[tokio::test]
    async fn post_fails_fast_on_second_401_after_successful_refresh() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/system/version"))
            .and(wiremock::matchers::header(
                "Authorization",
                "Token test-token",
            ))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_string(r#"{"code":-1,"msg":"Auth failed [header: Authorization]"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/system/version"))
            .and(wiremock::matchers::header(
                "Authorization",
                "Token fresh-token",
            ))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_string(r#"{"code":-1,"msg":"Auth failed [header: Authorization]"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;
        let refresher: TokenRefresher = Arc::new(|_| Ok("fresh-token".into()));
        let sy = SiYuan::for_tests_with_refresher(server.uri(), refresher);

        let err = sy
            .post("/api/system/version", json!({}))
            .await
            .expect_err("401 после рефреша — без второго рефреша");
        assert!(
            err.to_string().contains("повторился после рефреша"),
            "{err}"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn post_fails_fast_on_http_429_without_retries() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/system/version"))
            .respond_with(ResponseTemplate::new(429).set_body_string(
                r#"{"code":-1,"msg":"Слишком много неудачных попыток аутентификации, повторите попытку позже"}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .post("/api/system/version", json!({}))
            .await
            .expect_err("429 — fail-fast, не долбить");
        let msg = err.to_string();
        assert!(msg.contains("429"), "статус виден: {msg}");
        assert!(msg.contains("Не долбить"), "подсказка: {msg}");
        server.verify().await; // ровно один запрос
    }

    #[tokio::test]
    async fn post_fails_fast_on_rate_limit_envelope_even_with_200_status() {
        let server = MockServer::start().await;
        // Вспомогательный фоллбэк: rate-limit конвертом при 200-статусе.
        Mock::given(method("POST"))
            .and(path("/api/system/version"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"{"code":-1,"msg":"Слишком много неудачных попыток аутентификации, повторите попытку позже"}"#,
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        let err = sy
            .post("/api/system/version", json!({}))
            .await
            .expect_err("rate-limit конвертом — тоже fail-fast");
        assert!(err.to_string().contains("rate-limit конвертом"), "{err}");
        server.verify().await;
    }
}
