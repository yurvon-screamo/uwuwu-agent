//! Конфиг uwuwu-server: TOML-файл (политики/проекты) + env-секреты.
//! Нативные ошибки загрузки. Порт из TOML-структур в TickPlan — внизу.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use uwuwu_domain::{PeakWindow, SpawnPolicy};
use uwuwu_usecase::{BoardPlan, Mode, TickPlan};

pub const ENV_CONFIG: &str = "UWUWU_SERVER_CONFIG";
pub const ENV_GH_TOKEN: &str = "GH_TOKEN";
pub const ENV_OPENCODE_PASSWORD: &str = "UWUWU_OPENCODE_PASSWORD";
pub const ENV_BEARER: &str = "UWUWU_SERVER_BEARER";
pub const ENV_STATE_PATH: &str = "UWUWU_SERVER_STATE";
pub const ENV_MODE: &str = "UWUWU_SERVER_MODE";
pub const ENV_OPENCODE_GATE: &str = "UWUWU_OPENCODE_GATE";
/// Gitea-инстансы PR-гейта: «host=token,host=token» (#1269); нет/пусто —
/// Gitea-PR ждут владельца в Review (Foreign), деплой не ломается.
pub const ENV_GITEA_TOKENS: &str = "GITEA_TOKENS";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModeCfg {
    Observe,
    Live,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HubCfg {
    pub repo: String,
    /// "github" (деф.) | "memory" — мок-хаб для E2E-стенда без GitHub.
    #[serde(default = "default_hub_driver")]
    pub driver: String,
}

fn default_hub_driver() -> String {
    "github".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutorCfg {
    pub name: String,
    pub url: String,
    /// Назначение алиасов агентам: имя агента (+ "default" для bootstrap)
    /// → имя алиаса flash/common/smart — #1684. Отсутствует — канон
    /// владельца (SpawnModels::canonical). Raw-строки живут в model_aliases.
    pub models: Option<HashMap<String, String>>,
    /// Алиасы flash/common/smart → raw-модель (#1684). Отсутствует —
    /// канон владельца (ModelAliases::canonical).
    pub model_aliases: Option<HashMap<String, String>>,
}

/// Дополнительный именованный исполнитель ([[executors]]): подмножество
/// полей дефолтного — карта моделей у именованных не вводится: модель
/// кладётся сервером в тело спавна из карты `[executor.models]` /
/// `canonical()` (#1154).
#[derive(Debug, Clone, Deserialize)]
pub struct NamedExecutorCfg {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct PeakCfg {
    pub enabled: bool,
    pub start_min: u32,
    pub end_min: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PolicyCfg {
    pub cup: usize,
    pub peak: PeakCfg,
}

/// Инцидент-сигнализация сталл-детекта (#1281): массовость эскалаций за
/// окно — один ТГ-сигнал владельцу. Секция отсутствует — off (safe default).
#[derive(Debug, Clone, Deserialize)]
pub struct NotifyCfg {
    /// Имя env-переменной с токеном ТГ-бота (само значение — env, не конфиг).
    pub tg_token_env: String,
    /// Chat id получателя сигналов.
    pub tg_chat_id: i64,
}

/// Проба квоты подписки провайдера моделей (#1477): гейт лимитной тишины
/// сталл-детекта. Секция отсутствует — off (safe default). Привязка к
/// провайдеру — только конфигом (требование владельца 23.11.2026:
/// «запаковать за трейты») — ядро конвейера видит трейт QuotaProbe.
#[derive(Debug, Clone, Deserialize)]
pub struct QuotaCfg {
    /// База лимит-эндпоинта (без хвостового слэша).
    pub base_url: String,
    /// Путь лимит-эндпоинта.
    #[serde(default = "default_quota_path")]
    pub path: String,
    /// Имя env-переменной с ключом провайдера (секрет — env, не конфиг).
    pub api_key_env: String,
}

fn default_quota_path() -> String {
    "/api/monitor/usage/quota/limit".into()
}

/// Автокомпакт вечных сессий ботов (#1221, ADR-082): нативный compact
/// по idle-политике в тике супервизора. Секция отсутствует — механика
/// off (safe default, откат = снять секцию); провенанс дефолтов:
/// idle 4ч и 40% — числа владельца (постановка 05.11 в ADR-082),
/// коулдаун 1ч — реализация ADR-082 (ревью L3 #1221).
#[derive(Debug, Clone, Deserialize)]
pub struct CompactCfg {
    /// Неактивность ≥ этого числа минут (ms-порог — 60_000×).
    #[serde(default = "default_compact_idle_min")]
    pub idle_min: u64,
    /// Занятость окна контекста ≥ этого % лимита модели.
    #[serde(default = "default_compact_min_context_pct")]
    pub min_context_pct: f64,
    /// Не чаще раза в это число минут (адмиссия — метка state ДО POST).
    #[serde(default = "default_compact_cooldown_min")]
    pub cooldown_min: u64,
}

fn default_compact_idle_min() -> u64 {
    240
}

fn default_compact_min_context_pct() -> f64 {
    40.0
}

fn default_compact_cooldown_min() -> u64 {
    60
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectCfg {
    pub name: String,
    pub board: String,
    pub repo: String,
    /// Мастер-чекаут проекта на исполнителе (cwd bootstrap-сессии).
    pub checkout: String,
    /// Корень env задач на исполнителе (~/agents-env).
    pub env_root: String,
    /// Исполнитель проекта: имя из [[executors]] или дефолтного
    /// [executor]; None/пусто — дефолтный (#1203).
    pub executor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub mode: ModeCfg,
    #[serde(default = "default_poll")]
    pub poll_sec: u64,
    /// Клин холодного ящика (#1293): терминальные строки, закрытые
    /// строго старше этого числа суток, архивируются sidecar-тиком.
    /// 0 = клин выключен (дефолт кода; боевой конфиг — санкция владельца
    /// «старше недели»).
    #[serde(default)]
    pub sweep_days: u32,
    /// env-gc (#1431): снос env терминальных строк, чей issue закрыт
    /// строго старше этого числа часов (боевой 1 = «строго больше 1 ч»).
    /// 0 = выключен (дефолт кода — kill-switch).
    #[serde(default)]
    pub env_gc_hours: u32,
    /// observe-гейт env-gc (#1431, срез 4 плана): кандидаты в лог,
    /// команд нет — флип в false после пробы. Дефолт true (safe default:
    /// деструктивная механика стартует только наблюдением).
    #[serde(default = "default_true")]
    pub env_gc_observe: bool,
    #[serde(default = "default_port")]
    port: u16,
    pub hub: HubCfg,
    pub executor: ExecutorCfg,
    /// Дополнительные именованные исполнители (#1203); пусто — один
    /// дефолтный исполнитель, как раньше.
    #[serde(default)]
    pub executors: Vec<NamedExecutorCfg>,
    pub policy: PolicyCfg,
    pub project: Vec<ProjectCfg>,
    /// ТГ-сигнализация инцидентов сталл-детекта (#1281); нет секции — off.
    #[serde(default)]
    pub notify: Option<NotifyCfg>,
    /// Автокомпакт вечных сессий ботов (#1221); нет секции — off.
    #[serde(default)]
    pub compact: Option<CompactCfg>,
    /// Судья конвейера (#1364); нет секции — off (opt-in владельца).
    #[serde(default)]
    pub judge: Option<JudgeToml>,
    /// Проба квоты подписки провайдера (#1477); нет секции — off.
    #[serde(default)]
    pub quota: Option<QuotaCfg>,
    /// Операционные значения рантайма (аудит uwuwu#1726). Секции обязательны
    /// в загружаемом файле: `Config::load` отказывает старт с именем секции.
    /// `Option` здесь — шов для тестовых inline-конфигов, не второй механизм.
    #[serde(default)]
    pub search: Option<SearchCfg>,
    #[serde(default)]
    pub pipeline: Option<PipelineCfg>,
    #[serde(default)]
    pub api: Option<ApiCfg>,
    #[serde(default)]
    pub github: Option<GithubCfg>,
    #[serde(default)]
    pub blob: Option<BlobCfg>,
    #[serde(default)]
    pub bots: Option<BotsCfg>,
}

/// Поиск и эмбеддинги: единственный носитель значений — `[search]` TOML.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct SearchCfg {
    pub semantic_sync_budget: usize,
    pub embed_model: String,
    pub embed_dims: usize,
    pub embed_batch: usize,
    pub connect_secs: u64,
    pub query_total_secs: u64,
    pub batch_total_secs: u64,
    pub chunk_chars: usize,
    pub digest_chars: usize,
    pub recent_issues_cap: usize,
    pub query_overfetch_factor: usize,
    pub retention_secs: u64,
    pub sync_backoff_ms: u64,
}

/// Значения конвейера: капы, пороги, таймауты, окна.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct PipelineCfg {
    pub peak_provider: String,
    pub sweep_batch_cap: usize,
    pub sweep_days: u32,
    pub budget_floor: u64,
    pub env_gc_batch_cap: usize,
    pub env_gc_days: u32,
    pub env_gc_observe: bool,
    pub env_gc_verdict_deadline_ms: u64,
    pub env_gc_bootstrap_delete_attempts: u32,
    pub clean_batch_cap: usize,
    pub slot_session_live_ms: u64,
    pub review_round_cap: u32,
    pub clean_journal_cap: usize,
    pub spawn_cooldown_ms: u64,
    pub stall_silence_ms: u64,
    pub stall_ping_every_ms: u64,
    pub stall_max_pings: u32,
    pub stall_soft_silence_ms: u64,
    pub stall_soft_every_ms: u64,
    pub probe_degrade_min_rows: usize,
    pub session_lost_escalate_after: u32,
    pub limit_signal_threshold: usize,
    pub limit_signal_cooldown_ms: u64,
    pub analyst_wave_cooldown_ms: u64,
    pub analyst_cycle_limit: u32,
    pub analyst_review_cooldown_ms: u64,
    pub dreamer_idle_ms: u64,
    pub dreamer_day_cap: u32,
    pub cron_dir: String,
    pub cron_run_silence_ms: u64,
    pub cron_fire_cooldown_ms: u64,
    pub cron_late_fire_window_ms: u64,
    pub incident_threshold: usize,
    pub incident_window_ms: u64,
    pub incident_cooldown_ms: u64,
    pub executor_unavailable_threshold: usize,
    pub executor_unavailable_window_ms: u64,
    pub executor_unavailable_cooldown_ms: u64,
    pub task_env_command_budget_ms: u64,
    pub verify_attempts: usize,
    pub verify_pause_ms: u64,
    pub board_verify_attempts: usize,
    pub state_cap: usize,
    pub post_retention_days: u64,
    pub spawns_max_per_hour: usize,
    pub judge_restart_recent_ms: u64,
    pub spawn_budget_window_ms: u64,
    pub memory_snapshot_period_ms: u64,
    pub sidecar_timeout_ms: u64,
    pub semantic_sync_timeout_ms: u64,
    pub task_env_gc_script: String,
}

/// MCP-слой и карточки задач.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct ApiCfg {
    pub default_project: String,
    pub card_comments: usize,
    pub section_guard_floor: usize,
    pub title_max_chars: usize,
    pub goal_gist_glyphs: usize,
    pub person_slug: String,
    pub post_retention_days: u64,
}

/// REST-клиенты GitHub/Gitea.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct GithubCfg {
    pub http_timeout_secs: u64,
    pub page_size: usize,
    pub max_pages: usize,
    pub verify_gql_attempts: usize,
    pub verify_gql_pause_ms: u64,
    pub transport_retry_pause_ms: u64,
    pub transport_retry_attempts: u32,
    pub item_pages: usize,
    pub items_ttl_secs: u64,
    pub meta_ttl_secs: u64,
    pub status_ttl_secs: u64,
    pub snap_cache_ttl_secs: u64,
    pub status_cache_ttl_secs: u64,
    pub gql_backoff_secs: u64,
    pub wiki_cache_ttl_secs: u64,
    pub gql_budget_low: u64,
    pub rest_per_page: usize,
}

/// Blob-канал (Tigris/S3).
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct BlobCfg {
    pub max_bytes: usize,
    pub presign_ttl_secs: u64,
    pub presign_put_ttl_secs: u64,
    pub put_sign_ttl_secs: u64,
}

/// Боты и опенкод-клиент.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct BotsCfg {
    pub tg_poll_sec: u64,
    pub retry_pause_ms: u64,
    pub tg_http_timeout_secs: u64,
    pub guard_backoff_base_ms: u64,
    pub guard_backoff_cap_ms: u64,
    pub guard_healthy_ms: u64,
    pub reply_cap_ms: u64,
    pub reply_poll_ms: u64,
    pub asset_name_max_bytes: usize,
    pub chunk_reply_chars: usize,
    pub bot_debounce_ms: u64,
    pub user_msg_poll_ms: u64,
    pub stale_update_ms: u64,
    pub session_page_limit: usize,
    pub session_max_pages: usize,
    pub gate_http_timeout_secs: u64,
    pub poll_limit: usize,
    pub list_limit: usize,
}

/// Секция `[judge]` (#1364): порты вызовов и тримы контекста. Ключ —
/// env `GEMINI_API_KEY` (секреты не в конфиге).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct JudgeToml {
    /// Opt-in: без явного true контур судьи не работает.
    #[serde(default)]
    pub enabled: bool,
    /// Модель decision API (дефолт кода — gemini-decision-1).
    #[serde(default)]
    pub model: Option<String>,
    /// Интервал повторных вызовов на одной тишине (ms).
    #[serde(default = "default_judge_every_ms")]
    pub every_ms: u64,
    /// Гвард сверху судьи: вызовов без смены статуса до принудительного
    /// паркинга (слово владельца «порог 3»).
    #[serde(default = "default_judge_escalate_after")]
    pub escalate_after: u32,
    #[serde(default = "default_judge_comments_tail")]
    pub comments_tail: usize,
    #[serde(default = "default_judge_comment_chars")]
    pub comment_max_chars: usize,
    #[serde(default = "default_judge_body_chars")]
    pub body_max_chars: usize,
    #[serde(default = "default_judge_msg_chars")]
    pub msg_max_chars: usize,
    /// Полоса вызовов (в минуту) — квоты тира волны (#1362/#1363).
    #[serde(default = "default_judge_rpm")]
    pub rpm_limit: usize,
}

fn default_judge_every_ms() -> u64 {
    2 * 60 * 60_000
}
fn default_judge_escalate_after() -> u32 {
    3
}
fn default_judge_comments_tail() -> usize {
    4
}
fn default_judge_comment_chars() -> usize {
    1500
}
fn default_judge_body_chars() -> usize {
    6000
}
fn default_judge_msg_chars() -> usize {
    800
}
fn default_judge_rpm() -> usize {
    5
}

impl From<JudgeToml> for uwuwu_usecase::JudgeCfg {
    fn from(j: JudgeToml) -> Self {
        uwuwu_usecase::JudgeCfg {
            enabled: j.enabled,
            every_ms: j.every_ms,
            escalate_after: j.escalate_after,
            comments_tail: j.comments_tail,
            comment_max_chars: j.comment_max_chars,
            body_max_chars: j.body_max_chars,
            msg_max_chars: j.msg_max_chars,
        }
    }
}

fn default_poll() -> u64 {
    180
}

fn default_true() -> bool {
    true
}

fn default_port() -> u16 {
    8090
}

/// Секреты из env: отсутствующий секрет — честная ошибка старта.
#[derive(Debug, Clone)]
pub struct Secrets {
    pub gh_token: String,
    pub opencode_password: Option<String>,
    /// Гейт публичного опенкода: значение Cookie-хедера целиком
    /// (oc_gate=<hash>) — эндпоинт за Caddy-заглушкой.
    pub opencode_gate_cookie: Option<String>,
    /// Gitea-токены PR-гейта: host → token (#1269).
    pub gitea_tokens: HashMap<String, String>,
    pub bearer: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("чтение конфига {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("парсинг конфига {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("конфиг: {0}")]
    Validation(String),
}

impl Config {
    /// Режим: env UWUWU_SERVER_MODE (observe|live) > TOML mode.
    /// Флип конвейера в прод — переменной Railway, без пересборки образа.
    pub fn mode(&self) -> ModeCfg {
        match std::env::var(ENV_MODE)
            .ok()
            .filter(|v| !v.is_empty())
            .as_deref()
        {
            Some("live") => ModeCfg::Live,
            Some("observe") => ModeCfg::Observe,
            _ => self.mode,
        }
    }

    /// Порт: env PORT (Railway) > TOML port > дефолт.
    pub fn port(&self) -> u16 {
        std::env::var("PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(self.port)
    }

    pub fn load(path: &std::path::Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let cfg: Config = toml::from_str(&raw).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        cfg.validate()?;
        cfg.validate_runtime()?;
        Ok(cfg)
    }

    /// Обязательность секций рантайма (uwuwu#1726): отсутствие = отказ старта
    /// с именем секции, а не тихий дефолт из кода. Вызывается только на пути
    /// файловой загрузки; inline-конфиги тестов собираются через билдер.
    fn validate_runtime(&self) -> Result<(), ConfigError> {
        for (name, present) in [
            ("search", self.search.is_some()),
            ("pipeline", self.pipeline.is_some()),
            ("api", self.api.is_some()),
            ("github", self.github.is_some()),
            ("blob", self.blob.is_some()),
            ("bots", self.bots.is_some()),
        ] {
            if !present {
                return Err(ConfigError::Validation(format!(
                    "отсутствует обязательная секция [{name}]"
                )));
            }
        }
        let p = self.pipeline.as_ref().expect("validated");
        if p.semantic_sync_timeout_ms > 0 && p.sidecar_timeout_ms == 0 {
            return Err(ConfigError::Validation(
                "[pipeline] sidecar_timeout_ms = 0 при ненулевом semantic_sync_timeout_ms".into(),
            ));
        }
        let b = self.blob.as_ref().expect("validated");
        if b.presign_ttl_secs > 7 * 24 * 60 * 60 {
            return Err(ConfigError::Validation(
                "[blob] presign_ttl_secs > 7 суток: подпись SigV4 не живёт дольше".into(),
            ));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.poll_sec < 10 || self.poll_sec > 3600 {
            return Err(ConfigError::Validation("poll_sec вне 10..=3600".into()));
        }
        if self.project.is_empty() {
            return Err(ConfigError::Validation("нет ни одного [[project]]".into()));
        }
        if self.policy.cup == 0 {
            return Err(ConfigError::Validation("cup = 0 (конвейер стоит)".into()));
        }
        // [quota] (#1477): пустой base_url — включённая секция, которая
        // молча не работает (Err пробы на каждый гейт) — громче на старте
        if let Some(q) = &self.quota {
            if q.base_url.trim().is_empty() {
                return Err(ConfigError::Validation("[quota] base_url пуст".into()));
            }
        }
        // env-gc (#1431): снос обязан успевать до архивации строки в
        // холодный ящик — иначе env уходит в сироты вне досягаемости
        // доски. sweep_days = 0 (клин выключен) — инвариант не применим.
        if self.env_gc_hours > 0
            && self.sweep_days > 0
            && u64::from(self.env_gc_hours) >= u64::from(self.sweep_days) * 24
        {
            return Err(ConfigError::Validation(format!(
                "env_gc_hours ({}) обязан быть меньше sweep_days ({}) × 24: снос env должен успеть до архивации строки",
                self.env_gc_hours, self.sweep_days
            )));
        }
        if !self.executor.url.starts_with("http") {
            return Err(ConfigError::Validation("executor.url не http(s)".into()));
        }
        // [compact] (#1221, ревью L4): нулевые пороги легальны (стенды),
        // но nan/минус — конфиг-опечатка, меняющая семантику на «стрелять
        // при каждом проходе» — громкий старт вместо тихой миссемантики.
        if let Some(c) = &self.compact {
            if !c.min_context_pct.is_finite() || c.min_context_pct < 0.0 {
                return Err(ConfigError::Validation(
                    "compact.min_context_pct обязан быть конечным и ≥ 0".into(),
                ));
            }
        }
        self.validate_executors()?;
        for p in &self.project {
            if p.name.is_empty() || p.board.is_empty() || p.repo.is_empty() {
                return Err(ConfigError::Validation(format!(
                    "проект {}{{name,board,repo}} обязан быть непустым",
                    p.name
                )));
            }
            if let Some(target) = p.executor.as_deref().map(str::trim) {
                let known =
                    target == self.executor.name || self.executors.iter().any(|e| e.name == target);
                if !known {
                    return Err(ConfigError::Validation(format!(
                        "проект «{}»: executor «{}» не найден ([executor] или [[executors]])",
                        p.name, target
                    )));
                }
            }
        }
        Ok(())
    }

    /// Именованные исполнители (#1203): имена уникальны и не равны
    /// дефолтному, url http(s) — битый конфиг громко падает на старте.
    fn validate_executors(&self) -> Result<(), ConfigError> {
        let mut names = std::collections::BTreeSet::new();
        for e in &self.executors {
            if e.name.trim().is_empty() {
                return Err(ConfigError::Validation("executors[].name пуст".into()));
            }
            if !e.url.starts_with("http") {
                return Err(ConfigError::Validation(format!(
                    "executor «{}»: url не http(s)",
                    e.name
                )));
            }
            if e.name == self.executor.name {
                return Err(ConfigError::Validation(format!(
                    "executor «{}» дублирует имя дефолтного [executor]",
                    e.name
                )));
            }
            if !names.insert(e.name.as_str()) {
                return Err(ConfigError::Validation(format!(
                    "executor «{}» уже в конфиге",
                    e.name
                )));
            }
        }
        Ok(())
    }

    /// Секреты: observe обходится без пароля опенкода (спавнов нет).
    pub fn secrets(&self) -> Result<Secrets, ConfigError> {
        let gh_token = non_empty_env(ENV_GH_TOKEN).ok_or_else(|| {
            ConfigError::Validation(format!("{ENV_GH_TOKEN} обязателен (доступ к hub)"))
        })?;
        let bearer = non_empty_env(ENV_BEARER).ok_or_else(|| {
            ConfigError::Validation(format!("{ENV_BEARER} обязателен (HTTP/MCP гейт)"))
        })?;
        let opencode_password = non_empty_env(ENV_OPENCODE_PASSWORD);
        let opencode_gate_cookie = non_empty_env(ENV_OPENCODE_GATE);
        let gitea_tokens = match non_empty_env(ENV_GITEA_TOKENS) {
            Some(raw) => parse_gitea_tokens(&raw)?,
            None => HashMap::new(),
        };
        if self.mode() == ModeCfg::Live && opencode_password.is_none() {
            return Err(ConfigError::Validation(format!(
                "{ENV_OPENCODE_PASSWORD} обязателен в live-режиме (спавны)"
            )));
        }
        Ok(Secrets {
            gh_token,
            opencode_password,
            opencode_gate_cookie,
            gitea_tokens,
            bearer,
        })
    }

    /// ТГ-сигнализация инцидентов конвейера: сталл-детект (#1281),
    /// деградация executor'а (#1312). Секция не задана — off; задана,
    /// а env пуст — честная ошибка старта (канон секретов).
    pub fn tg_notify(&self) -> Result<Option<(String, i64)>, ConfigError> {
        match &self.notify {
            None => Ok(None),
            Some(n) => match non_empty_env(&n.tg_token_env) {
                Some(token) => Ok(Some((token, n.tg_chat_id))),
                None => Err(ConfigError::Validation(format!(
                    "{} обязателен (инцидент-сигнализация ТГ)",
                    n.tg_token_env
                ))),
            },
        }
    }

    /// Путь state-файла: env > рядом с конфигом.
    pub fn state_path(&self, config_path: &std::path::Path) -> PathBuf {
        std::env::var(ENV_STATE_PATH)
            .map(PathBuf::from)
            .unwrap_or_else(|_| config_path.with_file_name("state.json"))
    }

    /// Порт конфига → план тика юзкейса. Валидация конфига — здесь
    /// (деплой-данные): непустые имя/доска/чекауты, repo owner/name,
    /// дубли имён; битый конфиг — громкая ошибка старта.
    pub fn to_tick_plan(&self) -> Result<TickPlan, String> {
        let mut names = std::collections::BTreeSet::new();
        for p in &self.project {
            let name = p.name.trim();
            if name.is_empty() {
                return Err("проект без имени".into());
            }
            if !names.insert(name.to_string()) {
                return Err(format!("проект «{name}» уже в конфиге"));
            }
            if p.board.trim().is_empty() {
                return Err(format!("проект «{name}»: пустая доска"));
            }
            if p.repo.trim().split('/').count() != 2 {
                return Err(format!(
                    "проект «{name}»: repo «{}» не в форме owner/name",
                    p.repo.trim()
                ));
            }
            if p.checkout.trim().is_empty() || p.env_root.trim().is_empty() {
                return Err(format!(
                    "проект «{name}»: пустой checkout/env_root (спавн невозможен)"
                ));
            }
        }
        Ok(TickPlan {
            mode: match self.mode() {
                ModeCfg::Observe => Mode::Observe,
                ModeCfg::Live => Mode::Live,
            },
            policy: SpawnPolicy {
                cup: self.policy.cup,
                peak: PeakWindow {
                    enabled: self.policy.peak.enabled,
                    start_min: self.policy.peak.start_min,
                    end_min: self.policy.peak.end_min,
                },
            },
            boards: self
                .project
                .iter()
                .map(|p| BoardPlan {
                    project: p.name.trim().to_string(),
                    board: p.board.trim().to_string(),
                    repo: p.repo.trim().to_string(),
                    checkout: p.checkout.trim().to_string(),
                    env_root: p.env_root.trim().to_string(),
                })
                .collect(),
        })
    }
}

pub(crate) fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// GITEA_TOKENS → мапа host → token: пары через запятую, разделитель `=`
/// (#1269). Битая пара (без `=`, пустой host/token) — громкая ошибка
/// старта: тихий пропуск пары прятал бы неработающий инстанс.
fn parse_gitea_tokens(raw: &str) -> Result<HashMap<String, String>, ConfigError> {
    let mut tokens = HashMap::new();
    for pair in raw.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let Some((host, token)) = pair.split_once('=') else {
            return Err(ConfigError::Validation(format!(
                "{ENV_GITEA_TOKENS}: пара без '=' (host=token): {pair:?}"
            )));
        };
        let (host, token) = (host.trim(), token.trim());
        if host.is_empty() || token.is_empty() {
            return Err(ConfigError::Validation(format!(
                "{ENV_GITEA_TOKENS}: пустой host или token в паре {pair:?}"
            )));
        }
        if tokens.contains_key(host) {
            return Err(ConfigError::Validation(format!(
                "{ENV_GITEA_TOKENS}: дубликат хоста {host:?} — последний токен молча перезатирал бы предыдущий"
            )));
        }
        tokens.insert(host.to_string(), token.to_string());
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> &'static str {
        include_str!("../../../config.example.toml")
    }

    #[test]
    fn parses_gitea_tokens_env() {
        // #1269: мапа host=token через запятую; пробелы и пустые пары терпимы
        let tokens = parse_gitea_tokens(" git.l4a.ru = tok1 , git.true-wash.com=tok2 ,, ").unwrap();
        assert_eq!(tokens.get("git.l4a.ru").map(String::as_str), Some("tok1"));
        assert_eq!(
            tokens.get("git.true-wash.com").map(String::as_str),
            Some("tok2")
        );
        assert_eq!(parse_gitea_tokens("").unwrap().len(), 0);
        assert!(
            parse_gitea_tokens("host-without-token").is_err(),
            "пара без '=' — громкая ошибка старта"
        );
        assert!(
            parse_gitea_tokens("host=").is_err(),
            "пустой token — громкая ошибка старта"
        );
        assert!(
            parse_gitea_tokens("=tok").is_err(),
            "пустой host — громкая ошибка старта"
        );
        assert!(
            parse_gitea_tokens("git.l4a.ru=tok1,git.l4a.ru=tok2").is_err(),
            "дубликат хоста — громкая ошибка старта, не молчаливый last-wins"
        );
    }

    /// uwuwu#1726: тест-пин Values example-конфига снят. Проверяем
    /// поведение: шаблон грузится и даёт валидный план, без чисел.
    #[test]
    fn example_config_builds_tick_plan() {
        let cfg = toml::from_str::<Config>(example()).expect("example валиден");
        let plan = cfg.to_tick_plan().expect("проекты валидны");
        assert!(
            !plan.boards.is_empty(),
            "проекты из конфига доезжают в план"
        );
    }

    #[test]
    fn rejects_empty_projects() {
        let raw = r#"
            mode = "observe"
            [hub]
            repo = "a/b"
            [executor]
            name = "kisa"
            url = "http://127.0.0.1:1"
            [policy]
            cup = 7
            peak = { enabled = false, start_min = 0, end_min = 0 }
        "#;
        let err = toml::from_str::<Config>(raw).expect_err("должен упасть");
        assert!(err.to_string().contains("project"), "{err}");
    }

    #[test]
    fn broken_project_fails_loudly_in_domain() {
        // repo без owner/ — домен отклоняет при сборке плана (не в рантайме)
        let raw = r#"
            mode = "observe"
            [hub]
            repo = "a/b"
            [executor]
            name = "kisa"
            url = "http://127.0.0.1:1"
            [policy]
            cup = 7
            peak = { enabled = false, start_min = 0, end_min = 0 }
            [[project]]
            name = "p"
            board = "B"
            repo = "просто-имя"
            checkout = "/c"
            env_root = "/e"
        "#;
        let cfg = toml::from_str::<Config>(raw).expect("toml парсится");
        let err = cfg.to_tick_plan().expect_err("домен отклоняет");
        assert!(err.contains("owner/name"), "{err}");
    }

    fn base_two_projects() -> String {
        r#"
            mode = "observe"
            [hub]
            repo = "a/b"
            [executor]
            name = "kisa"
            url = "http://127.0.0.1:1"
            [policy]
            cup = 7
            peak = { enabled = false, start_min = 0, end_min = 0 }
            [[project]]
            name = "uwuwu"
            board = "B1"
            repo = "o/r"
            checkout = "/c1"
            env_root = "/e"
        "#
        .to_string()
    }

    #[test]
    fn parses_named_executors_and_project_reference() {
        let raw = format!(
            "{}\n[[executors]]\nname = \"redmi\"\nurl = \"http://127.0.0.1:2\"\n\
             [[project]]\nname = \"1xgames\"\nboard = \"B2\"\nrepo = \"o/r\"\n\
             checkout = \"D:/1x\"\nenv_root = \"D:/e\"\nexecutor = \"redmi\"\n",
            base_two_projects()
        );
        let cfg = toml::from_str::<Config>(&raw).expect("toml парсится");
        assert_eq!(cfg.executors.len(), 1);
        assert_eq!(cfg.executors[0].name, "redmi");
        assert_eq!(
            cfg.project[1].executor.as_deref(),
            Some("redmi"),
            "ссылка проекта на именованный исполнитель"
        );
        cfg.validate().expect("конфиг валиден");
    }

    #[test]
    fn project_without_executor_defaults_silently() {
        let raw = base_two_projects();
        let cfg = toml::from_str::<Config>(&raw).expect("toml парсится");
        assert!(
            cfg.project[0].executor.is_none(),
            "нет поля — дефолтный исполнитель (обратная совместимость)"
        );
        cfg.validate().expect("конфиг валиден");
    }

    #[test]
    fn rejects_duplicate_executor_names() {
        let raw = format!(
            "{}\n[[executors]]\nname = \"redmi\"\nurl = \"http://127.0.0.1:2\"\n\
             [[executors]]\nname = \"redmi\"\nurl = \"http://127.0.0.1:3\"\n",
            base_two_projects()
        );
        let cfg = toml::from_str::<Config>(&raw).expect("toml парсится");
        let err = cfg.validate().expect_err("дубль имени должен падать");
        assert!(err.to_string().contains("уже в конфиге"), "{err}");
    }

    #[test]
    fn compact_nan_and_negative_context_pct_fail_loudly() {
        // ревью L4 #1221: nan/минус — конфиг-опечатка, меняющая семантику
        // на «стрелять при каждом проходе»; нулевые пороги легальны
        for pct in ["nan", "-5.0"] {
            let raw = format!(
                "{}\n[compact]\nidle_min = 0\nmin_context_pct = {pct}\ncooldown_min = 1\n",
                base_two_projects()
            );
            let cfg = toml::from_str::<Config>(&raw).expect("toml парсится");
            let err = cfg.validate().expect_err("битый [compact] должен падать");
            assert!(err.to_string().contains("min_context_pct"), "{err}");
        }
        // легальная секция проходит
        let raw = format!(
            "{}\n[compact]\nidle_min = 0\nmin_context_pct = 0.0\ncooldown_min = 1\n",
            base_two_projects()
        );
        let cfg = toml::from_str::<Config>(&raw).expect("toml парсится");
        cfg.validate().expect("нулевые пороги легальны");
    }

    #[test]
    fn rejects_executor_named_as_default() {
        let raw = format!(
            "{}\n[[executors]]\nname = \"kisa\"\nurl = \"http://127.0.0.1:2\"\n",
            base_two_projects()
        );
        let cfg = toml::from_str::<Config>(&raw).expect("toml парсится");
        let err = cfg.validate().expect_err("имя дефолта должно падать");
        assert!(
            err.to_string().contains("дублирует имя дефолтного"),
            "{err}"
        );
    }

    #[test]
    fn rejects_non_http_named_executor_url() {
        let raw = format!(
            "{}\n[[executors]]\nname = \"redmi\"\nurl = \"127.0.0.1:2\"\n",
            base_two_projects()
        );
        let cfg = toml::from_str::<Config>(&raw).expect("toml парсится");
        let err = cfg.validate().expect_err("не-http url должен падать");
        assert!(err.to_string().contains("не http(s)"), "{err}");
    }

    fn minimal() -> String {
        // минимально валидный конфиг для notify-кейсов (tg_notify не ходит
        // в проекты/исполнителей — каркас только под serde/validate)
        r#"
mode = "observe"
[hub]
repo = "a/b"
[executor]
name = "kisa"
url = "http://127.0.0.1:1"
[policy]
cup = 7
peak = { enabled = false, start_min = 0, end_min = 0 }
[[project]]
name = "p"
board = "B"
repo = "a/c"
checkout = "/tmp/co"
env_root = "/tmp/env"
"#
        .to_string()
    }

    /// Срез 5 #1281: секция [notify] не задана — сигнализация off.
    #[test]
    fn notify_absent_section_is_off() {
        let cfg = toml::from_str::<Config>(&minimal()).expect("toml парсится");
        let notify = cfg.tg_notify().expect("off-вариант не ошибка старта");
        assert!(notify.is_none(), "нет [notify] — сигнализация off");
    }

    /// Секция задана, а env пуст — честная ошибка старта (канон секретов).
    #[test]
    fn notify_section_without_env_fails_startup() {
        let raw = format!(
            "{}\n[notify]\ntg_token_env = \"UWUWU_TEST_NOTIFY_ABSENT\"\ntg_chat_id = 42\n",
            minimal()
        );
        let cfg = toml::from_str::<Config>(&raw).expect("toml парсится");
        let err = cfg
            .tg_notify()
            .expect_err("секция без env должна валить старт");
        assert!(
            err.to_string().contains("UWUWU_TEST_NOTIFY_ABSENT"),
            "имя env в тексте ошибки: {err}"
        );
        assert!(err.to_string().contains("конфиг:"), "каноничный префикс");
    }

    /// Секция + env — пара (токен, chat) для TelegramClient.
    #[test]
    fn notify_section_with_env_returns_token_and_chat() {
        // env — глобал процесса: ключ уникален тестам модуля, чистим в конце
        const KEY: &str = "UWUWU_TEST_NOTIFY_PRESENT";
        std::env::set_var(KEY, "tok-123");
        let raw = format!(
            "{}\n[notify]\ntg_token_env = \"{KEY}\"\ntg_chat_id = 42\n",
            minimal()
        );
        let cfg = toml::from_str::<Config>(&raw).expect("toml парсится");
        let notify = cfg.tg_notify().expect("env задан — пара читается");
        assert_eq!(notify, Some(("tok-123".to_string(), 42)));
        std::env::remove_var(KEY);
    }

    /// #1273: TOML-ключ `port` задаёт порт HTTP-сервера. PORT — глобал
    /// процесса, cargo test гоняет тесты параллельно в одном процессе,
    /// поэтому все env-чувствительные фазы port() живут в одном тесте —
    /// единственном читателе/писателе PORT в наборе (изоляция структурная).
    #[test]
    fn config_toml_port_key_reaches_port() {
        // фаза 1 (красная на master): TOML-ключ port доходит до port()
        std::env::remove_var("PORT");
        let raw = format!("port = 18090\n{}", minimal());
        let cfg = toml::from_str::<Config>(&raw).expect("toml парсится");
        assert_eq!(
            cfg.port(),
            18090,
            "TOML port проигран дефолту — ключ съеден"
        );

        // фаза 2 (регресс-охрана): env PORT старше TOML
        std::env::set_var("PORT", "5555");
        assert_eq!(cfg.port(), 5555);
        std::env::remove_var("PORT"); // до фазы 3: гварду нужен чистый env

        // uwuwu#1726: фаза «пример врёт про порт» снята — проверка Values
        // шаблона заменена бесконтактной загрузкой в защите конфигов.
    }

    #[test]
    fn rejects_unknown_executor_reference() {
        let raw = format!(
            "{}\n[[project]]\nname = \"1xgames\"\nboard = \"B2\"\nrepo = \"o/r\"\n\
             checkout = \"D:/1x\"\nenv_root = \"D:/e\"\nexecutor = \"нет-такого\"\n",
            base_two_projects()
        );
        let cfg = toml::from_str::<Config>(&raw).expect("toml парсится");
        let err = cfg
            .validate()
            .expect_err("неизвестный executor должен падать");
        assert!(err.to_string().contains("не найден"), "{err}");
    }

    #[test]
    fn env_gc_defaults_are_off_and_observe() {
        // Safe defaults (#1431): kill-switch 0 + observe — деструктивная
        // механика стартует только наблюдением и по явному конфигу
        let cfg = toml::from_str::<Config>(&minimal()).expect("toml парсится");
        assert_eq!(cfg.env_gc_hours, 0, "дефолт кода — выключен");
        assert!(cfg.env_gc_observe, "дефолт кода — observe");
        cfg.validate().expect("дефолты валидны");
    }

    #[test]
    fn rejects_env_gc_hours_not_older_than_sweep_days() {
        // Инвариант порядка «снос через час ≪ архивация 7 суток» (#1431):
        // env_gc_hours ≥ sweep_days × 24 при живом клине — громкий старт.
        // Голые ключи — строго до base (после [[project]] они легли бы
        // в таблицу проекта)
        let raw = format!(
            "sweep_days = 1\nenv_gc_hours = 24\n{}\n",
            base_two_projects()
        );
        let cfg = toml::from_str::<Config>(&raw).expect("toml парсится");
        let err = cfg.validate().expect_err("инвариант порядка обязан падать");
        assert!(err.to_string().contains("env_gc_hours"), "{err}");
    }

    #[test]
    fn env_gc_hours_below_sweep_window_is_valid() {
        // Часовой порог укладывается в суточное окно архивации:
        // 23 ч < 1 сут × 24 — старт разрешён.
        let raw = format!(
            "sweep_days = 1\nenv_gc_hours = 23\n{}\n",
            base_two_projects()
        );
        let cfg = toml::from_str::<Config>(&raw).expect("toml парсится");
        cfg.validate().expect("порог меньше окна архивации");
    }

    #[test]
    fn env_gc_hours_valid_when_sweep_disabled() {
        // sweep_days = 0 (клин выключен): строки не уходят в архив,
        // сирот не бывает — инвариант порядка не применим
        let raw = format!(
            "sweep_days = 0\nenv_gc_hours = 1\n{}\n",
            base_two_projects()
        );
        let cfg = toml::from_str::<Config>(&raw).expect("toml парсится");
        cfg.validate()
            .expect("клин выключен — инвариант не применим");
    }

    // ——— носители кода конвейера (#1333) ———

    /// Живой прод-конфиг (server/config.toml коммитится в репо) — контракт
    /// носителей #1333: у не-GitHub проектов `repo` остаётся GitHub-каркасом
    /// (единственный потребитель — PR-слой автомержа prs.probe/merge, код в
    /// форже), `checkout` = мастер-чекаут кода (cwd bootstrap-сессий).
    /// Пин данных Т1: откат checkout true-washing на каркасный путь краснеет
    /// здесь, а не падением спавнов на мёртвом cwd в проде.
    #[test]
    fn pipeline_config_carries_code_carrier_contract_1333() {
        let raw = include_str!("../../../config.toml");
        let cfg = toml::from_str::<Config>(raw).expect("прод-конфиг валиден");
        let tw = cfg
            .project
            .iter()
            .find(|p| p.name == "true-washing")
            .expect("true-washing в прод-конфиге");
        // uwuwu#1726: проверяем структуру, а не значения конфига.
        assert_eq!(tw.name, "true-washing", "проект из конфига");
        for p in &cfg.project {
            assert!(!p.checkout.is_empty(), "checkout {} задан", p.name);
            assert!(!p.env_root.is_empty(), "env_root {} задан", p.name);
            assert!(!p.repo.is_empty(), "repo {} задан", p.name);
        }
    }

    /// Живая проба (гвард UWUWU_E2E=1, культура gitea_tests::live_probe_*):
    /// checkout каждого проекта прод-конфига существует и является живым
    /// git-чекаутом с origin — источником base для env-ворктри. Ловит класс
    /// «мёртвого cwd» (#1333: checkout true-washing указывал на удалённый с
    /// диска каталог — новые true-washing-спавны падали на bootstrap-этапе).
    /// Красная демонстрация против доТ1-конфига:
    /// `git show origin/master:server/config.toml > /tmp/cfg.toml &&
    /// UWUWU_E2E=1 UWUWU_CONFIG_UNDER_TEST=/tmp/cfg.toml cargo test -p
    /// uwuwu-server live_probe_config_checkouts`
    #[test]
    fn live_probe_config_checkouts_are_alive_git_worktrees() {
        if std::env::var("UWUWU_E2E").as_deref() != Ok("1") {
            return;
        }
        let default_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.toml");
        let path = std::env::var("UWUWU_CONFIG_UNDER_TEST")
            .map(std::path::PathBuf::from)
            .unwrap_or(default_path);
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("конфиг читается ({}): {e}", path.display()));
        let cfg = toml::from_str::<Config>(&raw).expect("конфиг парсится");
        assert!(!cfg.project.is_empty(), "в конфиге есть проекты");
        // Проба живёт на ноде дефолтного экзекьютора: проекты, привязанные к
        // именованному экзекьютору (пример: 1xgames → redmi за релеем,
        // checkout — путь той ноды), с этой машины непроверяемы — скип.
        let default_name = cfg.executor.name.as_str();
        let local = cfg
            .project
            .iter()
            .filter(|p| p.executor.as_deref().is_none_or(|e| e == default_name));
        for p in local {
            let meta = std::fs::metadata(&p.checkout).unwrap_or_else(|e| {
                panic!(
                    "checkout {} существует ({}): {e} — спавны проекта мертвы на cwd",
                    p.name, p.checkout
                )
            });
            assert!(meta.is_dir(), "checkout {} — каталог", p.name);
            let head = std::process::Command::new("git")
                .args(["-C", &p.checkout, "rev-parse", "--verify", "HEAD"])
                .output()
                .expect("git вызывается");
            assert!(
                head.status.success(),
                "checkout {} — живой git-чекаут (HEAD резолвится): {}",
                p.name,
                String::from_utf8_lossy(&head.stderr)
            );
            let origin = std::process::Command::new("git")
                .args(["-C", &p.checkout, "remote", "get-url", "origin"])
                .output()
                .expect("git вызывается");
            assert!(
                origin.status.success(),
                "checkout {} несёт origin (base для env-ворктри): {}",
                p.name,
                String::from_utf8_lossy(&origin.stderr)
            );
        }
    }

    /// [quota] (#1477, ревью раунд 3 L5): validate видит секцию — пустой
    /// base_url это громкий отказ старта, а не молчаливо неработающая
    /// включённая секция (Err пробы на каждый гейт).
    #[test]
    fn quota_validate_rejects_empty_base_url() {
        let good = format!(
            "{}\n[quota]\nbase_url = \"https://api.z.ai\"\npath = \"/api/monitor/usage/quota/limit\"\napi_key_env = \"UWUWU_ZAI_API_KEY\"\n",
            example()
        );
        let cfg = toml::from_str::<Config>(&good).expect("toml парсится");
        cfg.validate().expect("непустой base_url валиден");

        let bad = format!(
            "{}\n[quota]\nbase_url = \"\"\npath = \"/api/monitor/usage/quota/limit\"\napi_key_env = \"UWUWU_ZAI_API_KEY\"\n",
            example()
        );
        let cfg = toml::from_str::<Config>(&bad).expect("toml парсится");
        let err = cfg.validate().expect_err("пустой base_url должен падать");
        assert!(
            err.to_string().contains("base_url"),
            "validate называет причину: {err}"
        );
    }

    /// uwuwu#1726: бесконтактная проверка носителей — каждый коммитнутый
    /// конфиг грузится и проходит обязательные секции. Без утверждений о
    /// значениях: правка любого числа в TOML не ломает этот тест.
    #[test]
    fn committed_configs_load_with_required_sections() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for rel in [
            "config.toml",
            "config.example.toml",
            "ops/config.memory.toml",
        ] {
            let path = root.join(rel);
            Config::load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        }
    }
}
