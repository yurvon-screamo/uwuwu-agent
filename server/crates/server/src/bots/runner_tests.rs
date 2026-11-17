//! Интеграционные тесты бот-контура (#1150) на фейках: поведение раннера
//! (private-only, greeting, реплай, чанки, сессии) и матрица жизненного
//! цикла супервизора (spawn/stop/recreate/re-arm/hot/state/бэкофф).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;
use uwuwu_domain::{BotModel, Slot, Status};
use uwuwu_usecase::bots::{
    BotAssetStore, BotStateStore, MediaKind, TelegramGateway, TgMedia, TgMessage, TgUpdate,
};
use uwuwu_usecase::{
    AgentSessionGateway, AssistantReply, Clock, Liveness, SessionInfo, SessionSpec, TickStateStore,
    UsecaseError, UsecaseResult, WikiRepo,
};

use super::runner::{process_message, spawn_bot_task, BotDeps, BotStructural};
use super::supervisor::{BotSupervisor, GuardTiming};
use super::BotLive;

// ---------- фейки ----------

#[derive(Default)]
struct FakeWiki {
    docs: Mutex<HashMap<String, String>>,
}

impl FakeWiki {
    fn with_doc(slug: &str, md: &str) -> Arc<Self> {
        let wiki = Arc::new(Self::default());
        wiki.set_doc(slug, md);
        wiki
    }

    fn set_doc(&self, slug: &str, md: &str) {
        self.docs
            .lock()
            .expect("wiki lock")
            .insert(slug.to_string(), md.to_string());
    }
}

#[async_trait::async_trait]
impl WikiRepo for FakeWiki {
    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
        Ok(vec![])
    }
    async fn read_doc(&self, slug: &str) -> UsecaseResult<Option<String>> {
        Ok(self.docs.lock().expect("wiki lock").get(slug).cloned())
    }
    async fn list_docs(&self, prefix: &str) -> UsecaseResult<Vec<String>> {
        let norm = prefix.trim_matches('/');
        let mut out: Vec<String> = self
            .docs
            .lock()
            .expect("wiki lock")
            .keys()
            .filter(|p| norm.is_empty() || p.starts_with(&format!("{norm}/")))
            .cloned()
            .collect();
        out.sort();
        Ok(out)
    }
    async fn write_doc(&self, slug: &str, content: &str, _m: &str) -> UsecaseResult<()> {
        self.docs
            .lock()
            .expect("wiki lock")
            .insert(slug.into(), content.into());
        Ok(())
    }
    async fn append_doc(&self, slug: &str, line: &str, _m: &str) -> UsecaseResult<()> {
        let mut docs = self.docs.lock().expect("wiki lock");
        let e = docs.entry(slug.into()).or_default();
        e.push('\n');
        e.push_str(line);
        Ok(())
    }
}

/// FakeTg: async-поллинг очереди (прод — long-poll HTTP); panic_mode —
/// смерть задачи для теста луп-гварда. Апдейты остаются в очереди до
/// сдвига offset (семантика TG), fail_send — сбой sendMessage.
/// download_file: байты по file_id из fixture; fail_download/oversize —
/// сценарии отказов для media-веток (#1171).
struct FakeTg {
    queue: Mutex<Vec<TgUpdate>>,
    sent: Mutex<Vec<(i64, String)>>,
    fail: AtomicBool,
    panic: AtomicBool,
    fail_send: AtomicBool,
    polls: AtomicU32,
    files: Mutex<HashMap<String, Vec<u8>>>,
    fail_download: Mutex<Option<UsecaseError>>,
    downloads: AtomicU32,
}

impl FakeTg {
    fn new(updates: Vec<TgUpdate>) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(updates),
            sent: Mutex::new(vec![]),
            fail: AtomicBool::new(false),
            panic: AtomicBool::new(false),
            fail_send: AtomicBool::new(false),
            polls: AtomicU32::new(0),
            files: Mutex::new(HashMap::new()),
            fail_download: Mutex::new(None),
            downloads: AtomicU32::new(0),
        })
    }
    fn inject(&self, update: TgUpdate) {
        self.queue.lock().expect("tg lock").push(update);
    }
    fn sent_snapshot(&self) -> Vec<(i64, String)> {
        self.sent.lock().expect("tg lock").clone()
    }
    fn with_file(&self, file_id: &str, bytes: &[u8]) {
        self.files
            .lock()
            .expect("tg lock")
            .insert(file_id.into(), bytes.to_vec());
    }
    fn fail_download_with(&self, e: UsecaseError) {
        *self.fail_download.lock().expect("tg lock") = Some(e);
    }
}

#[async_trait::async_trait]
impl TelegramGateway for FakeTg {
    async fn get_updates(
        &self,
        offset: Option<u64>,
        timeout_sec: u64,
    ) -> UsecaseResult<Vec<TgUpdate>> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        if self.panic.load(Ordering::SeqCst) {
            panic!("fake tg: смерть задачи для луп-гварда");
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(UsecaseError::Telegram("fake tg down".into()));
        }
        // async-поллинг очереди до timeout; апдейты НЕ удаляются —
        // подтверждение происходит сдвигом offset у клиента (как в TG)
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_sec);
        loop {
            {
                let q = self.queue.lock().expect("tg lock");
                let pending: Vec<TgUpdate> = q
                    .iter()
                    .filter(|u| Some(u.update_id) >= offset)
                    .cloned()
                    .collect();
                if !pending.is_empty() {
                    return Ok(pending);
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(vec![]);
            }
            sleep(Duration::from_millis(10)).await;
        }
    }
    async fn send_message(&self, chat_id: i64, text: &str) -> UsecaseResult<()> {
        if self.fail_send.load(Ordering::SeqCst) {
            return Err(UsecaseError::Telegram(
                "fake tg: sendMessage refused".into(),
            ));
        }
        self.sent
            .lock()
            .expect("tg lock")
            .push((chat_id, text.to_string()));
        Ok(())
    }
    async fn download_file(&self, file_id: &str) -> UsecaseResult<Vec<u8>> {
        self.downloads.fetch_add(1, Ordering::SeqCst);
        if let Some(e) = self.fail_download.lock().expect("tg lock").take() {
            return Err(e);
        }
        self.files
            .lock()
            .expect("tg lock")
            .get(file_id)
            .cloned()
            .ok_or_else(|| UsecaseError::Telegram("fake tg: неизвестный file_id".into()))
    }
}

/// FakeAssets: url фиксируется, байты — для проверок содержимого.
type AssetPut = (String, MediaKind, Option<String>, Vec<u8>);

struct FakeAssets {
    puts: Mutex<Vec<AssetPut>>,
    fail: AtomicBool,
}

impl FakeAssets {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            puts: Mutex::new(vec![]),
            fail: AtomicBool::new(false),
        })
    }
    fn snapshot(&self) -> Vec<AssetPut> {
        self.puts.lock().expect("assets lock").clone()
    }
}

#[async_trait::async_trait]
impl BotAssetStore for FakeAssets {
    async fn put(
        &self,
        bot: &str,
        kind: MediaKind,
        filename: Option<&str>,
        bytes: &[u8],
    ) -> UsecaseResult<String> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(UsecaseError::Board("fake assets: put refused".into()));
        }
        let url = format!("memory://assets/bots/{bot}/asset");
        self.puts.lock().expect("assets lock").push((
            url.clone(),
            kind,
            filename.map(String::from),
            bytes.to_vec(),
        ));
        Ok(url)
    }
}

/// FakeSessions: реплай появляется на prompt (симуляция сессии) —
/// baseline читается до промпта, ответ создаётся после.
struct FakeSessions {
    counter: AtomicU32,
    seq: AtomicU64,
    created: Mutex<Vec<SessionSpec>>,
    prompts: Mutex<Vec<(String, String)>>,
    liveness: Mutex<HashMap<String, Liveness>>,
    reply: Mutex<Option<AssistantReply>>,
    /// Реплай, «создаваемый сессией» в момент prompt (baseline-семантика
    /// раннера: ответ должен появиться после промпта).
    on_prompt: Mutex<Option<AssistantReply>>,
    /// Сценарий многосообщного хода (наблюдённая модель пробы #1209):
    /// шаги выдаются только после первого prompt (baseline-вызов раннера
    /// идёт до prompt и видит прежнюю логику), по одному на вызов,
    /// последний залипает (ход завершился — сессия не меняется).
    script: Mutex<Vec<AssistantReply>>,
    script_pos: AtomicUsize,
    auto_reply: AtomicBool,
}

impl FakeSessions {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            counter: AtomicU32::new(1),
            seq: AtomicU64::new(100),
            created: Mutex::new(vec![]),
            prompts: Mutex::new(vec![]),
            liveness: Mutex::new(HashMap::new()),
            reply: Mutex::new(None),
            on_prompt: Mutex::new(None),
            script: Mutex::new(Vec::new()),
            script_pos: AtomicUsize::new(0),
            auto_reply: AtomicBool::new(true),
        })
    }
    fn set_liveness(&self, session: &str, l: Liveness) {
        self.liveness
            .lock()
            .expect("sessions lock")
            .insert(session.to_string(), l);
    }
    fn prompts_snapshot(&self) -> Vec<(String, String)> {
        self.prompts.lock().expect("sessions lock").clone()
    }
}

#[async_trait::async_trait]
impl AgentSessionGateway for FakeSessions {
    async fn create_session(&self, spec: &SessionSpec) -> UsecaseResult<String> {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        self.created
            .lock()
            .expect("sessions lock")
            .push(spec.clone());
        Ok(format!("ses_fake{n}"))
    }
    async fn prompt(&self, session: &str, text: &str) -> UsecaseResult<()> {
        self.prompts
            .lock()
            .expect("sessions lock")
            .push((session.to_string(), text.to_string()));
        {
            let mut on_prompt = self.on_prompt.lock().expect("sessions lock");
            if on_prompt.is_some() {
                *self.reply.lock().expect("sessions lock") = on_prompt.take();
                return Ok(());
            }
        }
        if self.auto_reply.load(Ordering::SeqCst) {
            let tail = text
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or_default();
            let created = self.seq.fetch_add(1, Ordering::SeqCst);
            *self.reply.lock().expect("sessions lock") = Some(AssistantReply {
                created_ms: created,
                text: format!("эхо: {tail}"),
                finished: true,
                is_latest: true,
                finish: Some("stop".into()),
            });
        }
        Ok(())
    }
    async fn run_command(&self, _s: &str, _n: &str, _a: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn liveness(&self, session: &str) -> Liveness {
        self.liveness
            .lock()
            .expect("sessions lock")
            .get(session)
            .copied()
            .unwrap_or(Liveness::Alive)
    }
    async fn delete_session(&self, _s: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn last_activity_ms(&self, _s: &str) -> UsecaseResult<Option<u64>> {
        Ok(None)
    }
    async fn last_assistant_reply(&self, _s: &str) -> UsecaseResult<Option<AssistantReply>> {
        {
            let script = self.script.lock().expect("sessions lock");
            if !script.is_empty() && !self.prompts.lock().expect("sessions lock").is_empty() {
                let pos = self.script_pos.fetch_add(1, Ordering::SeqCst);
                let idx = pos.min(script.len() - 1);
                return Ok(Some(script[idx].clone()));
            }
        }
        Ok(self.reply.lock().expect("sessions lock").clone())
    }
    async fn list_sessions(&self) -> UsecaseResult<Vec<SessionInfo>> {
        Ok(vec![])
    }
    async fn export_session(&self, _s: &str) -> UsecaseResult<String> {
        Ok(String::new())
    }
}

/// slug → (session, offset) — пара состояния бота.
type BotRow = (Option<String>, Option<u64>, Option<u64>);

#[derive(Default)]
struct FakeState {
    rows: Mutex<HashMap<String, BotRow>>,
}

impl BotStateStore for FakeState {
    fn bot_session(&self, slug: &str) -> Option<String> {
        self.rows
            .lock()
            .expect("state lock")
            .get(slug)
            .and_then(|r| r.0.clone())
    }
    fn set_bot_session(&self, slug: &str, session: Option<&str>) {
        let mut rows = self.rows.lock().expect("state lock");
        let e = rows.entry(slug.to_string()).or_default();
        e.0 = session.map(String::from);
    }
    fn bot_offset(&self, slug: &str) -> Option<u64> {
        self.rows
            .lock()
            .expect("state lock")
            .get(slug)
            .and_then(|r| r.1)
    }
    fn set_bot_offset(&self, slug: &str, offset: u64) {
        let mut rows = self.rows.lock().expect("state lock");
        let e = rows.entry(slug.to_string()).or_default();
        e.1 = Some(offset);
    }
    fn bot_manual_hash(&self, slug: &str) -> Option<u64> {
        self.rows
            .lock()
            .expect("state lock")
            .get(slug)
            .and_then(|r| r.2)
    }
    fn set_bot_manual_hash(&self, slug: &str, hash: u64) {
        let mut rows = self.rows.lock().expect("state lock");
        let e = rows.entry(slug.to_string()).or_default();
        e.2 = Some(hash);
    }
    fn save(&self) -> UsecaseResult<()> {
        Ok(())
    }
}

/// Минимальный TickStateStore: живы только dreamer-маркеры (их зовёт
/// раннер), остальное — no-op.
#[derive(Default)]
struct MemTickState {
    dreamer_done: Mutex<Vec<String>>,
}

impl TickStateStore for MemTickState {
    fn last_comment_id(&self, _b: &str, _n: u64) -> Option<String> {
        None
    }
    fn last_seen_status(&self, _b: &str, _n: u64) -> Option<Status> {
        None
    }
    fn set_last_seen_status(&self, _b: &str, _n: u64, _s: Status) {}
    fn set_last_comment_id(&self, _b: &str, _n: u64, _id: &str) {}
    fn dreamer_done(&self, session: &str) -> bool {
        self.dreamer_done
            .lock()
            .expect("lock")
            .iter()
            .any(|s| s == session)
    }
    fn mark_dreamer_done(&self, session: &str) {
        self.dreamer_done
            .lock()
            .expect("lock")
            .push(session.to_string());
    }
    fn dreamer_day_spawns(&self, _day: &str) -> u32 {
        0
    }
    fn mark_dreamer_spawn(&self, _day: &str) {}
    fn last_spawn_ms(&self, _b: &str, _n: u64, _s: Slot) -> Option<u64> {
        None
    }
    fn clear_spawn_ms(&self, _b: &str, _n: u64, _s: Slot) {}
    fn mark_spawn(&self, _b: &str, _n: u64, _s: Slot, _at: u64) {}
    fn last_analys_ms(&self, _b: &str, _n: u64) -> Option<u64> {
        None
    }
    fn stall_ping(&self, _b: &str, _n: u64) -> Option<(uwuwu_domain::Status, u64, u32)> {
        None
    }
    fn mark_stall_ping(&self, _b: &str, _n: u64, _s: uwuwu_domain::Status, _at: u64) {}
    fn mark_analys_spawn(&self, _b: &str, _n: u64, _at: u64) {}
    fn last_analys_review_ms(&self, _b: &str, _n: u64) -> Option<u64> {
        None
    }
    fn mark_analys_review_spawn(&self, _b: &str, _n: u64, _at: u64) {}
    fn analys_loop(&self, _b: &str, _n: u64) -> Option<(Status, u32)> {
        None
    }
    fn set_analys_loop(&self, _b: &str, _n: u64, _st: Status, _r: u32) {}
    fn clear_analys_loop(&self, _b: &str, _n: u64) {}
    fn analys_review_loop(&self, _b: &str, _n: u64) -> Option<(Status, u32)> {
        None
    }
    fn set_analys_review_loop(&self, _b: &str, _n: u64, _st: Status, _r: u32) {}
    fn clear_analys_review_loop(&self, _b: &str, _n: u64) {}
    fn prune_analys_guards(&self, _live: &std::collections::HashSet<String>) {}
    fn spawn_loop(&self, _b: &str, _n: u64, _s: Slot) -> Option<(Status, u32)> {
        None
    }
    fn set_spawn_loop(&self, _b: &str, _n: u64, _s: Slot, _st: Status, _r: u32) {}
    fn reset_stale_spawn_loops(&self, _b: &str, _n: u64, _c: Status) {}
    fn own_comment(&self, _id: &str) -> bool {
        false
    }
    fn mark_own_comment(&self, _id: &str, _posted_ms: u64) {}
    fn save(&self) -> UsecaseResult<()> {
        Ok(())
    }
}

#[derive(Default)]
struct FakeClock {
    now: AtomicU64,
}

impl FakeClock {
    fn at(ms: u64) -> Arc<Self> {
        Arc::new(Self {
            now: AtomicU64::new(ms),
        })
    }
}

impl Clock for FakeClock {
    fn utc_minute(&self) -> uwuwu_domain::UtcMinute {
        uwuwu_domain::UtcMinute {
            minute_of_day: 0,
            weekday: 1,
        }
    }
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}

// ---------- хелперы ----------

const SLUG: &str = "Projects/uwuwu/Боты/проба";

fn doc_md(model: &str, token_env: &str, manual: &str) -> String {
    format!(
        "---\ntitle: Проба\nmodel: {model}\ntoken_env: {token_env}\n\
         owner: yurvon\nworkspace: /tmp/opencode\ngreeting: привет-привет\n---\n\n{manual}"
    )
}

fn private_update(id: u64, username: &str, text: &str) -> TgUpdate {
    TgUpdate {
        update_id: id,
        message: Some(TgMessage {
            chat_type: "private".into(),
            chat_id: 42,
            from_username: Some(username.into()),
            text: Some(text.into()),
            caption: None,
            media: None,
        }),
    }
}

/// Media-апдейт от владельца (#1171): photo/file_id + caption.
fn media_update(
    id: u64,
    username: &str,
    kind: MediaKind,
    file_id: &str,
    caption: Option<&str>,
    size: Option<i64>,
) -> TgUpdate {
    TgUpdate {
        update_id: id,
        message: Some(TgMessage {
            chat_type: "private".into(),
            chat_id: 42,
            from_username: Some(username.into()),
            text: None,
            caption: caption.map(String::from),
            media: Some(TgMedia {
                kind,
                file_id: file_id.into(),
                file_name: if kind == MediaKind::Document {
                    Some("скрин.png".into())
                } else {
                    None
                },
                file_size: size,
                mime: None,
            }),
        }),
    }
}

fn group_update(id: u64) -> TgUpdate {
    TgUpdate {
        update_id: id,
        message: Some(TgMessage {
            chat_type: "group".into(),
            chat_id: -100,
            from_username: Some("yurvon".into()),
            text: Some("из группы".into()),
            caption: None,
            media: None,
        }),
    }
}

async fn wait_until(mut cond: impl FnMut() -> bool, max_ms: u64) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(max_ms);
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        sleep(Duration::from_millis(10)).await;
    }
    cond()
}

fn deps(
    tg: Arc<FakeTg>,
    sessions: Arc<FakeSessions>,
    state: Arc<FakeState>,
    reply_cap_ms: u64,
    reply_poll_ms: u64,
    live: BotLive,
) -> BotDeps {
    BotDeps {
        slug: SLUG.into(),
        structural: BotStructural {
            title: "Проба".into(),
            model: BotModel {
                provider: "p".into(),
                id: "m".into(),
                variant: None,
            },
            workspace: "/tmp/opencode".into(),
            agent: None,
        },
        live: Arc::new(live),
        tg,
        sessions,
        state: state.clone(),
        assets: FakeAssets::new(),
        tick_state: Arc::new(MemTickState::default()),
        reply_cap_ms,
        reply_poll_ms,
        retry_pause_ms: 10,
    }
}

fn supervisor(
    wiki: Arc<FakeWiki>,
    sessions: Arc<FakeSessions>,
    state: Arc<FakeState>,
    clock: Arc<FakeClock>,
    tg: Arc<FakeTg>,
    tokens: Vec<(&'static str, &'static str)>,
) -> BotSupervisor {
    let tokens: Arc<HashMap<String, String>> = Arc::new(
        tokens
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    );
    let mut sup = BotSupervisor::new(
        wiki,
        sessions,
        state,
        FakeAssets::new(),
        Arc::new(MemTickState::default()),
        clock,
        Arc::new(move |_| {
            let handle = tg.clone();
            Arc::new(TgHandle(handle)) as Arc<dyn TelegramGateway>
        }),
    );
    sup.token_of = Arc::new(move |env| tokens.get(env).cloned());
    sup.reply_cap_ms = 300;
    sup.reply_poll_ms = 10;
    sup.guard = GuardTiming {
        backoff_base_ms: 200,
        backoff_cap_ms: 1_000,
        healthy_ms: 5_000,
    };
    sup
}

/// Мостик к общему FakeTg: фабрика супервизора возвращает один инстанс.
struct TgHandle(Arc<FakeTg>);

#[async_trait::async_trait]
impl TelegramGateway for TgHandle {
    async fn get_updates(
        &self,
        offset: Option<u64>,
        timeout_sec: u64,
    ) -> UsecaseResult<Vec<TgUpdate>> {
        self.0.get_updates(offset, timeout_sec).await
    }
    async fn send_message(&self, chat_id: i64, text: &str) -> UsecaseResult<()> {
        self.0.send_message(chat_id, text).await
    }
    async fn download_file(&self, file_id: &str) -> UsecaseResult<Vec<u8>> {
        self.0.download_file(file_id).await
    }
}

// ---------- раннер ----------

/// Шаг сценария хода (наблюдённая модель пробы #1209).
fn step_reply(created_ms: u64, text: &str, finish: &str) -> AssistantReply {
    AssistantReply {
        created_ms,
        text: text.into(),
        finished: true,
        is_latest: true,
        finish: Some(finish.into()),
    }
}

/// Ход из двух сообщений (живая проба 17.11): промежуточный шаг с
/// finish='tool-calls' и is_latest=true — НЕ ответ (зазор после тула
/// ловится поллингом), доставляется только финальный finish='stop'.
#[tokio::test]
async fn intermediate_toolcall_step_not_delivered() {
    let tg = FakeTg::new(vec![private_update(6, "yurvon", "многошаговый ход")]);
    let sessions = FakeSessions::new();
    sessions.auto_reply.store(false, Ordering::SeqCst);
    sessions.script.lock().expect("lock").push(step_reply(
        1_000,
        "Смотрю конфиг сервера и поля досок:",
        "tool-calls",
    ));
    sessions
        .script
        .lock()
        .expect("lock")
        .push(step_reply(2_000, "полный финальный ответ", "stop"));
    let state = Arc::new(FakeState::default());
    let task = spawn_bot_task(deps(
        tg.clone(),
        sessions.clone(),
        state.clone(),
        1_000,
        10,
        BotLive::new(Some("yurvon".into()), None, "м".into()),
    ));
    let ok = wait_until(
        || {
            tg.sent_snapshot()
                .iter()
                .any(|(_, t)| t.contains("финальный ответ"))
        },
        2000,
    )
    .await;
    task.abort();
    assert!(ok, "финальный шаг не доставлен");
    let sent = tg.sent_snapshot();
    assert_eq!(sent.len(), 1, "промежуточный шаг ушёл в ТГ: {sent:?}");
    assert_eq!(sent[0].1, "полный финальный ответ");
}

/// Ход в стриме (проба #1209): в окне стрима finish=None — сообщение
/// не доставляется даже с is_latest=true; доставляется только финал
/// после появления finish='stop'.
#[tokio::test]
async fn streaming_step_not_delivered_until_stop() {
    let tg = FakeTg::new(vec![private_update(9, "yurvon", "ход в стриме")]);
    let sessions = FakeSessions::new();
    sessions.auto_reply.store(false, Ordering::SeqCst);
    sessions.script.lock().expect("lock").push(AssistantReply {
        created_ms: 1_000,
        text: "недостримленный кусок".into(),
        finished: false,
        is_latest: true,
        finish: None,
    });
    sessions
        .script
        .lock()
        .expect("lock")
        .push(step_reply(2_000, "стрим дошёл до финала", "stop"));
    let state = Arc::new(FakeState::default());
    let task = spawn_bot_task(deps(
        tg.clone(),
        sessions.clone(),
        state.clone(),
        1_000,
        10,
        BotLive::new(Some("yurvon".into()), None, "м".into()),
    ));
    let ok = wait_until(
        || {
            tg.sent_snapshot()
                .iter()
                .any(|(_, t)| t.contains("стрим дошёл до финала"))
        },
        2000,
    )
    .await;
    task.abort();
    assert!(ok, "финал стрима не доставлен");
    let sent = tg.sent_snapshot();
    assert_eq!(sent.len(), 1, "полустрим ушёл в ТГ: {sent:?}");
    assert_eq!(sent[0].1, "стрим дошёл до финала");
}

/// Финал без текста (инцидент 16.11): finish='stop' при пустом тексте —
/// не ответ, в ТГ уходит только честный фоллбек по cap, пустота не
/// доставляется.
#[tokio::test]
async fn finished_empty_text_not_answered() {
    let tg = FakeTg::new(vec![private_update(10, "yurvon", "вопрос без ответа")]);
    let sessions = FakeSessions::new();
    sessions.auto_reply.store(false, Ordering::SeqCst);
    // залипающий шаг: тула отработали (finish='stop'), текста нет
    sessions
        .script
        .lock()
        .expect("lock")
        .push(step_reply(1_000, "   ", "stop"));
    let state = Arc::new(FakeState::default());
    let task = spawn_bot_task(deps(
        tg.clone(),
        sessions.clone(),
        state.clone(),
        80,
        10,
        BotLive::new(Some("yurvon".into()), None, "м".into()),
    ));
    let ok = wait_until(
        || {
            tg.sent_snapshot()
                .iter()
                .any(|(_, t)| t.contains("напиши ещё раз"))
        },
        2000,
    )
    .await;
    task.abort();
    assert!(ok, "фоллбек на безтекстовом финале не отправлен");
    let sent = tg.sent_snapshot();
    assert_eq!(sent.len(), 1, "кроме фоллбека ушло лишнее: {sent:?}");
    assert_eq!(sent[0].1, "Не успел ответить — напиши ещё раз.");
}

#[tokio::test]
async fn private_owner_message_prompts_and_replies() {
    let tg = FakeTg::new(vec![private_update(7, "yurvon", "заявка из ТГ")]);
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    let task = spawn_bot_task(deps(
        tg.clone(),
        sessions.clone(),
        state.clone(),
        1_000,
        10,
        BotLive::new(Some("yurvon".into()), None, "создавай Draft".into()),
    ));
    let ok = wait_until(
        || !tg.sent_snapshot().is_empty() && !sessions.prompts_snapshot().is_empty(),
        2000,
    )
    .await;
    task.abort();
    assert!(ok, "реплай не отправлен");
    let (_, prompt) = &sessions.prompts_snapshot()[0];
    assert!(
        prompt.contains("Методичка бота"),
        "методичка — префикс промпта"
    );
    assert!(prompt.contains("создавай Draft"));
    assert!(prompt.ends_with("заявка из ТГ"));
    assert!(
        tg.sent_snapshot()[0].1.starts_with("эхо:"),
        "текст реплая ушёл в ТГ"
    );
    assert_eq!(
        state.bot_offset(SLUG),
        Some(8),
        "offset персистится после обработки"
    );
}

#[tokio::test]
async fn group_message_absorbed_without_prompt() {
    let tg = FakeTg::new(vec![group_update(3)]);
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    let task = spawn_bot_task(deps(
        tg.clone(),
        sessions.clone(),
        state.clone(),
        1_000,
        10,
        BotLive::new(None, None, "м".into()),
    ));
    let ok = wait_until(|| state.bot_offset(SLUG) == Some(4), 2000).await;
    task.abort();
    assert!(ok, "offset не продвинулся");
    assert!(
        sessions.prompts_snapshot().is_empty(),
        "группа не доходит до сессии"
    );
    assert!(tg.sent_snapshot().is_empty());
}

#[tokio::test]
async fn start_answers_greeting_without_llm() {
    let tg = FakeTg::new(vec![private_update(1, "yurvon", "/start")]);
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    let task = spawn_bot_task(deps(
        tg.clone(),
        sessions.clone(),
        state.clone(),
        1_000,
        10,
        BotLive::new(
            Some("yurvon".into()),
            Some("привет-привет".into()),
            "м".into(),
        ),
    ));
    let ok = wait_until(|| !tg.sent_snapshot().is_empty(), 2000).await;
    task.abort();
    assert!(ok);
    assert_eq!(tg.sent_snapshot(), vec![(42, "привет-привет".into())]);
    assert!(sessions.prompts_snapshot().is_empty(), "/start без LLM");
}

#[tokio::test]
async fn reply_timeout_sends_fallback() {
    let tg = FakeTg::new(vec![private_update(2, "yurvon", "медленный вопрос")]);
    let sessions = FakeSessions::new();
    sessions.auto_reply.store(false, Ordering::SeqCst); // сессия молчит
    let state = Arc::new(FakeState::default());
    let task = spawn_bot_task(deps(
        tg.clone(),
        sessions.clone(),
        state.clone(),
        80,
        10,
        BotLive::new(Some("yurvon".into()), None, "м".into()),
    ));
    let ok = wait_until(
        || {
            tg.sent_snapshot()
                .iter()
                .any(|(_, t)| t.contains("напиши ещё раз"))
        },
        2000,
    )
    .await;
    task.abort();
    assert!(ok, "фоллбек на таймауте не отправлен");
}

#[tokio::test]
async fn long_reply_chunked() {
    let tg = FakeTg::new(vec![private_update(5, "yurvon", "дай длинно")]);
    let sessions = FakeSessions::new();
    sessions.auto_reply.store(false, Ordering::SeqCst);
    *sessions.on_prompt.lock().expect("lock") = Some(AssistantReply {
        created_ms: 1_000_000,
        text: "а".repeat(9500),
        finished: true,
        is_latest: true,
        finish: Some("stop".into()),
    });
    let state = Arc::new(FakeState::default());
    let task = spawn_bot_task(deps(
        tg.clone(),
        sessions.clone(),
        state.clone(),
        1_000,
        10,
        BotLive::new(Some("yurvon".into()), None, "м".into()),
    ));
    let ok = wait_until(|| tg.sent_snapshot().len() >= 3, 2000).await;
    task.abort();
    assert!(ok, "ждём 3 чанки");
    let total: usize = tg
        .sent_snapshot()
        .iter()
        .map(|(_, t)| t.chars().count())
        .sum();
    assert_eq!(total, 9500, "чанки покрывают ответ целиком");
}

#[tokio::test]
async fn send_failure_holds_offset_and_retries() {
    let tg = FakeTg::new(vec![private_update(
        4,
        "yurvon",
        "не дойдёт с первого раза",
    )]);
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    tg.fail_send.store(true, Ordering::SeqCst);
    let task = spawn_bot_task(deps(
        tg.clone(),
        sessions.clone(),
        state.clone(),
        1_000,
        10,
        BotLive::new(Some("yurvon".into()), None, "м".into()),
    ));
    // сбой отправки: offset держится, апдейт не подтверждён
    let held = wait_until(|| tg.polls.load(Ordering::SeqCst) >= 2, 3000).await;
    assert!(held, "раннер сделал повторный полл после сбоя");
    assert_eq!(
        state.bot_offset(SLUG),
        None,
        "offset не двигается при неудачной отправке (план: персист после отправки)"
    );
    // ТГ ожил → апдейт переедет с очереди и ответ уйдёт
    tg.fail_send.store(false, Ordering::SeqCst);
    let ok = wait_until(
        || {
            !tg.sent_snapshot().is_empty()
                && tg.sent_snapshot()[0].1.contains("эхо:")
                && state.bot_offset(SLUG) == Some(5)
        },
        3000,
    )
    .await;
    task.abort();
    assert!(
        ok,
        "после восстановления ТГ сообщение обработано и offset сдвинут"
    );
}

#[tokio::test]
async fn alive_session_reused_dead_recreated() {
    // живая сессия в state — реюз без create
    let tg = FakeTg::new(vec![private_update(9, "yurvon", "раз")]);
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    state.set_bot_session(SLUG, Some("ses_old"));
    let task = spawn_bot_task(deps(
        tg.clone(),
        sessions.clone(),
        state.clone(),
        1_000,
        10,
        BotLive::new(Some("yurvon".into()), None, "м".into()),
    ));
    wait_until(|| !tg.sent_snapshot().is_empty(), 2000).await;
    task.abort();
    assert!(
        sessions.created.lock().expect("lock").is_empty(),
        "живая сессия не пересоздаётся"
    );
    assert_eq!(state.bot_session(SLUG).as_deref(), Some("ses_old"));

    // unknown (гейт недоступен) — тоже реюз: рестарт гейта не плодит дубли
    let tg2 = FakeTg::new(vec![private_update(11, "yurvon", "три")]);
    let sessions2 = FakeSessions::new();
    sessions2.set_liveness("ses_old", Liveness::Unknown);
    let state2 = Arc::new(FakeState::default());
    state2.set_bot_session(SLUG, Some("ses_old"));
    let task2 = spawn_bot_task(deps(
        tg2.clone(),
        sessions2.clone(),
        state2.clone(),
        1_000,
        10,
        BotLive::new(Some("yurvon".into()), None, "м".into()),
    ));
    wait_until(|| !tg2.sent_snapshot().is_empty(), 2000).await;
    task2.abort();
    assert!(
        sessions2.created.lock().expect("lock").is_empty(),
        "unknown liveness не пересоздаёт сессию"
    );

    // мёртвая — пересоздание
    let tg3 = FakeTg::new(vec![private_update(10, "yurvon", "два")]);
    let sessions3 = FakeSessions::new();
    sessions3.set_liveness("ses_old", Liveness::Dead);
    let state3 = Arc::new(FakeState::default());
    state3.set_bot_session(SLUG, Some("ses_old"));
    let task3 = spawn_bot_task(deps(
        tg3.clone(),
        sessions3.clone(),
        state3.clone(),
        1_000,
        10,
        BotLive::new(Some("yurvon".into()), None, "м".into()),
    ));
    let ok = wait_until(
        || {
            state3.bot_session(SLUG).as_deref() == Some("ses_fake1")
                && !tg3.sent_snapshot().is_empty()
        },
        2000,
    )
    .await;
    task3.abort();
    assert!(ok, "мёртвая сессия пересоздана");
    assert!(
        tg3.sent_snapshot()[0].1.starts_with("эхо:"),
        "реплай по новой сессии"
    );
}

// ---------- супервизор ----------

#[tokio::test]
async fn doc_created_spawns_bot_and_survives_ticks() {
    let wiki = FakeWiki::with_doc(SLUG, &doc_md("p/m", "UWUWU_BOT_TOKEN_T1", "м"));
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    let clock = FakeClock::at(10_000);
    let tg = FakeTg::new(vec![private_update(1, "yurvon", "привет")]);
    let sup = Arc::new(supervisor(
        wiki.clone(),
        sessions.clone(),
        state.clone(),
        clock.clone(),
        tg.clone(),
        vec![("UWUWU_BOT_TOKEN_T1", "tok")],
    ));
    sup.tick().await.unwrap();
    let ok = wait_until(|| !sessions.created.lock().expect("lock").is_empty(), 2000).await;
    assert!(ok, "спавн не поднял сессию");
    assert!(sup.is_running(SLUG));
    let created_after_first = sessions.created.lock().expect("lock").len();
    sup.tick().await.unwrap();
    sleep(Duration::from_millis(50)).await;
    assert_eq!(
        sessions.created.lock().expect("lock").len(),
        created_after_first,
        "повторный тик не пересоздаёт сессию"
    );
}

#[tokio::test]
async fn doc_removed_stops_bot_keeps_session() {
    let wiki = FakeWiki::with_doc(SLUG, &doc_md("p/m", "UWUWU_BOT_TOKEN_T2", "м"));
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    let clock = FakeClock::at(10_000);
    let tg = FakeTg::new(vec![private_update(1, "yurvon", "привет")]);
    let sup = Arc::new(supervisor(
        wiki.clone(),
        sessions.clone(),
        state.clone(),
        clock.clone(),
        tg.clone(),
        vec![("UWUWU_BOT_TOKEN_T2", "tok")],
    ));
    sup.tick().await.unwrap();
    wait_until(
        || sup.is_running(SLUG) && !sessions.created.lock().expect("lock").is_empty(),
        2000,
    )
    .await;
    wiki.docs.lock().expect("lock").remove(SLUG); // снос дока
    sup.tick().await.unwrap();
    assert!(!sup.is_running(SLUG), "бот погашен");
    assert!(
        state.bot_session(SLUG).is_some(),
        "сессия сохраняется после сноса дока"
    );
}

#[tokio::test]
async fn model_change_recreates_session_memory_reset() {
    let wiki = FakeWiki::with_doc(SLUG, &doc_md("p/m1", "UWUWU_BOT_TOKEN_T3", "м"));
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    let clock = FakeClock::at(10_000);
    let tg = FakeTg::new(vec![private_update(1, "yurvon", "привет")]);
    let sup = Arc::new(supervisor(
        wiki.clone(),
        sessions.clone(),
        state.clone(),
        clock.clone(),
        tg.clone(),
        vec![("UWUWU_BOT_TOKEN_T3", "tok")],
    ));
    sup.tick().await.unwrap();
    wait_until(|| state.bot_session(SLUG).is_some(), 2000).await;
    let old_session = state.bot_session(SLUG).unwrap();
    wiki.set_doc(SLUG, &doc_md("p/m2", "UWUWU_BOT_TOKEN_T3", "м"));
    sup.tick().await.unwrap();
    tg.inject(private_update(2, "yurvon", "второе")); // сессия ленивая — создаётся на сообщении
    let ok = wait_until(
        || {
            state.bot_session(SLUG).is_some()
                && state.bot_session(SLUG).as_deref() != Some(old_session.as_str())
        },
        2000,
    )
    .await;
    assert!(ok, "структурная правка пересоздаёт сессию");
    assert_eq!(
        sessions.created.lock().expect("lock").len(),
        2,
        "ровно одно пересоздание"
    );
}

#[tokio::test]
async fn token_env_change_rearms_keeps_session() {
    let wiki = FakeWiki::with_doc(SLUG, &doc_md("p/m", "UWUWU_BOT_TOKEN_T4A", "м"));
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    let clock = FakeClock::at(10_000);
    let tg = FakeTg::new(vec![private_update(1, "yurvon", "привет")]);
    let sup = Arc::new(supervisor(
        wiki.clone(),
        sessions.clone(),
        state.clone(),
        clock.clone(),
        tg.clone(),
        vec![
            ("UWUWU_BOT_TOKEN_T4A", "tok"),
            ("UWUWU_BOT_TOKEN_T4B", "tok"),
        ],
    ));
    sup.tick().await.unwrap();
    wait_until(|| state.bot_session(SLUG).is_some(), 2000).await;
    let session = state.bot_session(SLUG).unwrap();
    wiki.set_doc(SLUG, &doc_md("p/m", "UWUWU_BOT_TOKEN_T4B", "м"));
    sup.tick().await.unwrap();
    tg.inject(private_update(2, "yurvon", "второе")); // новая задача получит сообщение
    let ok = wait_until(
        || {
            sessions
                .prompts_snapshot()
                .iter()
                .any(|(s, _)| s.as_str() == session)
        },
        2000,
    )
    .await;
    assert!(ok, "перезапущенная задача работает с той же сессией");
    assert_eq!(
        state.bot_session(SLUG).as_deref(),
        Some(session.as_str()),
        "re-arm не трогает сессию"
    );
    assert!(sup.is_running(SLUG), "задача перезапущена на новом токене");
    assert_eq!(
        sessions.created.lock().expect("lock").len(),
        1,
        "re-arm не создаёт сессию"
    );
}

#[tokio::test]
async fn manual_edit_is_hot_without_restart() {
    let wiki = FakeWiki::with_doc(
        SLUG,
        &doc_md("p/m", "UWUWU_BOT_TOKEN_T5", "старая методичка"),
    );
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    let clock = FakeClock::at(10_000);
    let tg = FakeTg::new(vec![private_update(1, "yurvon", "вопрос")]);
    let sup = Arc::new(supervisor(
        wiki.clone(),
        sessions.clone(),
        state.clone(),
        clock.clone(),
        tg.clone(),
        vec![("UWUWU_BOT_TOKEN_T5", "tok")],
    ));
    sup.tick().await.unwrap();
    wait_until(|| sup.is_running(SLUG), 2000).await;
    // первое сообщение создаёт ленивую сессию — база для «без рестарта»
    wait_until(|| !sessions.created.lock().expect("lock").is_empty(), 2000).await;
    let created_before = sessions.created.lock().expect("lock").len();
    wiki.set_doc(
        SLUG,
        &doc_md("p/m", "UWUWU_BOT_TOKEN_T5", "новая методичка"),
    );
    sup.tick().await.unwrap();
    tg.inject(private_update(2, "yurvon", "второй вопрос"));
    // сообщение после горячего обновления — промпт несёт новую методичку
    let ok = wait_until(
        || {
            sessions
                .prompts_snapshot()
                .iter()
                .any(|(_, p)| p.contains("новая методичка"))
        },
        3000,
    )
    .await;
    assert!(ok, "горячая методичка доезжает в промпт");
    assert_eq!(
        sessions.created.lock().expect("lock").len(),
        created_before,
        "правка методички без рестарта"
    );
}

#[tokio::test]
async fn supervisor_restart_restores_bots_with_memory() {
    let wiki = FakeWiki::with_doc(SLUG, &doc_md("p/m", "UWUWU_BOT_TOKEN_T6", "м"));
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    let clock = FakeClock::at(10_000);
    let tg = FakeTg::new(vec![]);
    let sup = Arc::new(supervisor(
        wiki.clone(),
        sessions.clone(),
        state.clone(),
        clock.clone(),
        tg.clone(),
        vec![("UWUWU_BOT_TOKEN_T6", "tok")],
    ));
    sup.tick().await.unwrap();
    wait_until(|| state.bot_session(SLUG).is_some(), 2000).await;
    // «рестарт сервера»: новый супервизор на тех же фейках
    let sup2_tg = FakeTg::new(vec![]);
    let sup2 = Arc::new(supervisor(
        wiki.clone(),
        sessions.clone(),
        state.clone(),
        clock.clone(),
        sup2_tg.clone(),
        vec![("UWUWU_BOT_TOKEN_T6", "tok")],
    ));
    sup2.tick().await.unwrap();
    sup2_tg.inject(private_update(2, "yurvon", "после рестарта"));
    let ok = wait_until(|| !sup2_tg.sent_snapshot().is_empty(), 2000).await;
    assert!(ok, "восстановленный бот отвечает");
    assert!(sup2.is_running(SLUG), "бот восстановлен");
    assert_eq!(
        sessions.created.lock().expect("lock").len(),
        1,
        "живая сессия переиспользуется — память сохранена"
    );
}

#[tokio::test]
async fn missing_token_does_not_spawn() {
    let wiki = FakeWiki::with_doc(SLUG, &doc_md("p/m", "UWUWU_BOT_TOKEN_T7_MISSING", "м"));
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    let clock = FakeClock::at(10_000);
    let tg = FakeTg::new(vec![]);
    let sup = Arc::new(supervisor(
        wiki.clone(),
        sessions.clone(),
        state.clone(),
        clock.clone(),
        tg.clone(),
        vec![("UWUWU_BOT_TOKEN_T7", "tok")],
    ));
    sup.tick().await.unwrap();
    sup.tick().await.unwrap();
    assert!(!sup.is_running(SLUG), "без токена бот не поднимается");
    assert!(sessions.created.lock().expect("lock").is_empty());
    assert!(state.bot_session(SLUG).is_none());
}

#[tokio::test]
async fn dead_task_restarts_with_backoff() {
    let wiki = FakeWiki::with_doc(SLUG, &doc_md("p/m", "UWUWU_BOT_TOKEN_T8", "м"));
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    let clock = FakeClock::at(10_000);
    let tg = FakeTg::new(vec![]);
    tg.panic.store(true, Ordering::SeqCst); // задача умрёт на первом полле
    let sup = Arc::new(supervisor(
        wiki.clone(),
        sessions.clone(),
        state.clone(),
        clock.clone(),
        tg.clone(),
        vec![("UWUWU_BOT_TOKEN_T8", "tok")],
    ));
    sup.tick().await.unwrap();
    let ok = wait_until(|| sup.is_running(SLUG) && sup.running_task_dead(SLUG), 2000).await;
    assert!(ok, "задача с паникующим ТГ погасла");
    // тик в окне бэкоффа: рестарт отложен
    sup.tick().await.unwrap();
    sleep(Duration::from_millis(30)).await;
    let polls_after_wait = tg.polls.load(Ordering::SeqCst);
    // часы вперёд за потолок бэкоффа (cap 1с в тесте) → рестарт
    clock.now.store(10_000 + 2_000, Ordering::SeqCst);
    sup.tick().await.unwrap();
    let ok = wait_until(|| tg.polls.load(Ordering::SeqCst) > polls_after_wait, 2000).await;
    assert!(
        ok,
        "после бэкоффа задача перезапущена (поллы возобновились)"
    );
}

#[tokio::test]
async fn broken_spec_fails_loudly_without_spawn() {
    let wiki = FakeWiki::with_doc(SLUG, "---\ntitle: битый\n---\nтело без model");
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    let clock = FakeClock::at(10_000);
    let tg = FakeTg::new(vec![]);
    let sup = Arc::new(supervisor(
        wiki.clone(),
        sessions.clone(),
        state.clone(),
        clock.clone(),
        tg.clone(),
        vec![],
    ));
    sup.tick().await.unwrap();
    assert!(!sup.is_running(SLUG), "битая спека не поднимается");
    assert!(sessions.created.lock().expect("lock").is_empty());
}

#[test]
fn stop_reason_discriminates_the_three_cases() {
    use super::supervisor::stop_reason;
    use uwuwu_domain::BotSpec;
    use uwuwu_usecase::bots::parse_bot_doc;
    // регресс диагностики: одна причина на случай, не одна строка на три
    assert_eq!(stop_reason(None), Some("док снесён или переименован"));
    let broken: Result<BotSpec, String> = Err("нет fm model".into());
    assert_eq!(
        stop_reason(Some(&broken)),
        Some("спека не парсится (fm сломан)")
    );
    let md = |enabled: &str| {
        format!("---\nmodel: p/m\ntoken_env: T\nworkspace: /w\nenabled: {enabled}\n---")
    };
    let off = parse_bot_doc("s", &md("false")).unwrap();
    assert_eq!(stop_reason(Some(&Ok(off))), Some("enabled:false"));
    let on = parse_bot_doc("s", &md("true")).unwrap();
    assert_eq!(
        stop_reason(Some(&Ok(on))),
        None,
        "живая запись — гасить нечего"
    );
}

#[tokio::test]
async fn manual_rides_once_not_every_message() {
    // Владелец 16.11: методичка не префиксует каждый промпт — едет
    // одним сообщением при создании (hash-детект), дальше чистый текст.
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    let live = BotLive::new(
        Some("yurvon".into()),
        Some("привет".into()),
        "методичка-тело".into(),
    );
    let deps0 = deps(
        FakeTg::new(vec![]),
        sessions.clone(),
        state.clone(),
        50,
        5,
        live,
    );
    // сообщение 1: промпт несёт методичку (бутстрап)
    let u1 = private_update(1, "yurvon", "вопрос один");
    process_message(&deps0, u1.message.as_ref().unwrap())
        .await
        .unwrap();
    {
        let ps = sessions.prompts_snapshot();
        assert!(
            ps.iter().any(|(_, p)| p.contains("методичка-тело")),
            "первый промпт вшивает методичку: {ps:?}"
        );
    }
    // сообщение 2: тот же hash — промпт чистый
    let u2 = private_update(2, "yurvon", "вопрос два");
    process_message(&deps0, u2.message.as_ref().unwrap())
        .await
        .unwrap();
    {
        let ps = sessions.prompts_snapshot();
        let second: Vec<&String> = ps
            .iter()
            .filter(|(_, p)| p.contains("вопрос два"))
            .map(|(_, p)| p)
            .collect();
        assert!(second.len() == 1, "второй промпт есть: {ps:?}");
        assert!(
            second.iter().all(|p| !p.contains("методичка-тело")),
            "повторной методички нет: {ps:?}"
        );
    }
    // правка методички (горячая): следующий промпт несёт новую — один раз
    let deps1 = deps(
        FakeTg::new(vec![]),
        sessions.clone(),
        state.clone(),
        50,
        5,
        BotLive::new(
            Some("yurvon".into()),
            Some("привет".into()),
            "новая-методичка".into(),
        ),
    );
    let u3 = private_update(3, "yurvon", "вопрос три");
    process_message(&deps1, u3.message.as_ref().unwrap())
        .await
        .unwrap();
    {
        let ps = sessions.prompts_snapshot();
        assert!(
            ps.iter()
                .any(|(_, p)| p.contains("новая-методичка") && p.contains("вопрос три")),
            "правка методички доезжает одним промптом: {ps:?}"
        );
    }
}

#[tokio::test]
async fn agent_mode_prompt_is_raw_text() {
    // fm agent: методичка в agents/<name>.md воркспейса — промпт
    // пользователю несёт ТОЛЬКО текст сообщения, тело дока не едет.
    let sessions = FakeSessions::new();
    let state = Arc::new(FakeState::default());
    let live = BotLive::new(
        Some("yurvon".into()),
        None,
        "методичка-дока-игнорируется".into(),
    );
    let mut d = deps(FakeTg::new(vec![]), sessions.clone(), state, 50, 5, live);
    d.structural.agent = Some("marketing".into());
    let u = private_update(1, "yurvon", "чистый вопрос");
    process_message(&d, u.message.as_ref().unwrap())
        .await
        .unwrap();
    let ps = sessions.prompts_snapshot();
    assert!(
        ps.iter().any(|(_, p)| p == "чистый вопрос"),
        "agent-режим: промпт без методички, только текст: {ps:?}"
    );
}

// ---------- media (#1171) ----------

fn deps_with_assets(
    tg: Arc<FakeTg>,
    sessions: Arc<FakeSessions>,
    assets: Arc<FakeAssets>,
) -> BotDeps {
    let mut d = deps(
        tg,
        sessions,
        Arc::new(FakeState::default()),
        1_000,
        10,
        BotLive::new(Some("yurvon".into()), None, "м".into()),
    );
    d.assets = assets;
    d
}

/// Фото владельца: скачан → залит в asset store → URL и caption в промпте
/// сессии; ни одно сообщение не потерялось молча.
#[tokio::test]
async fn owner_photo_reaches_session_prompt_with_url() {
    let tg = FakeTg::new(vec![media_update(
        21,
        "yurvon",
        MediaKind::Photo,
        "agg1",
        Some("от МПНЧ"),
        Some(10_000),
    )]);
    tg.with_file("agg1", b"\x89PNG-data");
    let sessions = FakeSessions::new();
    let assets = FakeAssets::new();
    let d = deps_with_assets(tg.clone(), sessions.clone(), assets.clone());
    let m = tg.queue.lock().expect("l")[0].message.clone().unwrap();
    process_message(&d, &m).await.unwrap();

    assert_eq!(tg.downloads.load(Ordering::SeqCst), 1, "файл скачан");
    let puts = assets.snapshot();
    let [(url, kind, filename, bytes)] = puts.as_slice() else {
        panic!("asset put не выполнен: {puts:?}");
    };
    assert!(matches!(kind, MediaKind::Photo));
    assert!(filename.is_none(), "photo без file_name");
    assert_eq!(bytes, b"\x89PNG-data");
    let ps = sessions.prompts_snapshot();
    let [(s, p)] = ps.as_slice() else {
        panic!("промпт не отправлен: {ps:?}")
    };
    assert!(p.contains(url), "URL в промпте: {p}");
    assert!(p.contains("от МПНЧ"), "caption в промпте: {p}");
    assert!(p.contains("вложи эту ссылку"), "инструкция в промпте: {p}");
    // reply владельцу уходит (эхо последней строки промпта)
    let sent = tg.sent_snapshot();
    assert!(
        sent.iter().any(|(_, t)| t.contains("эхо:")),
        "реплай не доставлен: {sent:?}"
    );
    let _ = s;
}

/// Oversize-гард: file_size > 20 MiB в апдейте — явный отказ без
/// скачивания и без prompt; апдейт поглощён (Ok).
#[tokio::test]
async fn oversized_document_refused_without_download() {
    let big: i64 = 30 * 1024 * 1024;
    let tg = FakeTg::new(vec![
        media_update(22, "yurvon", MediaKind::Document, "doc1", None, Some(big)),
        private_update(23, "yurvon", "следующее сообщение"),
    ]);
    let sessions = FakeSessions::new();
    let assets = FakeAssets::new();
    let d = deps_with_assets(tg.clone(), sessions.clone(), assets.clone());
    let m = tg.queue.lock().expect("l")[0].message.clone().unwrap();
    process_message(&d, &m).await.expect("отказ — не Err");

    let sent = tg.sent_snapshot();
    assert!(
        sent.iter().any(|(_, t)| t.contains("слишком большой")),
        "oversize-отказ владельцу не отправлен: {sent:?}"
    );
    assert_eq!(tg.downloads.load(Ordering::SeqCst), 0, "оверсайз качается");
    assert!(assets.snapshot().is_empty(), "оверсайз залит в store");
    // апдейт поглощён: следующее сообщение доходит до сессии
    let m2 = tg.queue.lock().expect("l")[1].message.clone().unwrap();
    process_message(&d, &m2).await.unwrap();
    assert_eq!(sessions.prompts_snapshot().len(), 1);
}

/// Второй гард: file_size не заполнен — 400 «file is too big» от getFile
/// уходит в тот же oversize-отказ, не в общий сбой.
#[tokio::test]
async fn get_file_too_big_refused_as_oversize() {
    let tg = FakeTg::new(vec![media_update(
        24,
        "yurvon",
        MediaKind::Document,
        "doc2",
        None,
        None,
    )]);
    tg.fail_download_with(UsecaseError::TelegramTooBig("400 file is too big".into()));
    let sessions = FakeSessions::new();
    let assets = FakeAssets::new();
    let d = deps_with_assets(tg.clone(), sessions.clone(), assets.clone());
    let m = tg.queue.lock().expect("l")[0].message.clone().unwrap();
    process_message(&d, &m)
        .await
        .expect("too-big — отказ, не Err");

    assert!(
        tg.sent_snapshot()
            .iter()
            .any(|(_, t)| t.contains("слишком большой")),
        "oversize-отказ не доставлен"
    );
    assert!(assets.snapshot().is_empty());
    assert!(sessions.prompts_snapshot().is_empty());
}

/// Сбой скачивания (транспорт) — явный отказ «не смог забрать», поглощён.
#[tokio::test]
async fn download_failure_refused_not_silent() {
    let tg = FakeTg::new(vec![media_update(
        25,
        "yurvon",
        MediaKind::Document,
        "doc3",
        None,
        Some(100),
    )]);
    tg.fail_download_with(UsecaseError::Telegram("fake transport down".into()));
    let sessions = FakeSessions::new();
    let assets = FakeAssets::new();
    let d = deps_with_assets(tg.clone(), sessions.clone(), assets.clone());
    let m = tg.queue.lock().expect("l")[0].message.clone().unwrap();
    process_message(&d, &m)
        .await
        .expect("сбой скачивания — отказ, не Err");

    assert!(
        tg.sent_snapshot()
            .iter()
            .any(|(_, t)| t.contains("Не смог забрать файл")),
        "отказ о сбое не доставлен"
    );
    assert!(assets.snapshot().is_empty());
    assert!(sessions.prompts_snapshot().is_empty());
}

/// Сбой upload в asset store — тот же явный отказ (ff-коллизия и пр.).
#[tokio::test]
async fn asset_upload_failure_refused_not_silent() {
    let tg = FakeTg::new(vec![media_update(
        26,
        "yurvon",
        MediaKind::Photo,
        "agg3",
        None,
        Some(10),
    )]);
    tg.with_file("agg3", b"bytes");
    let sessions = FakeSessions::new();
    let assets = FakeAssets::new();
    assets.fail.store(true, Ordering::SeqCst);
    let d = deps_with_assets(tg.clone(), sessions.clone(), assets.clone());
    let m = tg.queue.lock().expect("l")[0].message.clone().unwrap();
    process_message(&d, &m)
        .await
        .expect("сбой upload — отказ, не Err");

    assert!(
        tg.sent_snapshot()
            .iter()
            .any(|(_, t)| t.contains("Не смог забрать файл")),
        "отказ о сбое upload не доставлен"
    );
    assert!(sessions.prompts_snapshot().is_empty());
}

/// Не-текстовые типы вне photo/document (видео/войс/стикер) — явный
/// отказ «не принимаю», без скачивания; документом тот же контент доехал бы.
#[tokio::test]
async fn unsupported_media_kind_refused_explicitly() {
    let tg = FakeTg::new(vec![media_update(
        27,
        "yurvon",
        MediaKind::Other,
        "vid1",
        None,
        Some(1000),
    )]);
    let sessions = FakeSessions::new();
    let assets = FakeAssets::new();
    let d = deps_with_assets(tg.clone(), sessions.clone(), assets.clone());
    let m = tg.queue.lock().expect("l")[0].message.clone().unwrap();
    process_message(&d, &m).await.unwrap();

    assert!(
        tg.sent_snapshot()
            .iter()
            .any(|(_, t)| t.contains("не принимаю")),
        "отказ о типе не доставлен"
    );
    assert_eq!(tg.downloads.load(Ordering::SeqCst), 0);
    assert!(assets.snapshot().is_empty());
    assert!(sessions.prompts_snapshot().is_empty());
}

/// У граничного размера (ровно 20 MiB) файл качается — decimal-гард
/// 20 000 000 отбрасывал бы валидные файлы (план #1171).
#[tokio::test]
async fn file_at_exact_binary_limit_is_accepted() {
    let edge: i64 = 20 * 1024 * 1024;
    let tg = FakeTg::new(vec![media_update(
        28,
        "yurvon",
        MediaKind::Document,
        "doc4",
        None,
        Some(edge),
    )]);
    tg.with_file("doc4", b"edge");
    let sessions = FakeSessions::new();
    let assets = FakeAssets::new();
    let d = deps_with_assets(tg.clone(), sessions.clone(), assets.clone());
    let m = tg.queue.lock().expect("l")[0].message.clone().unwrap();
    process_message(&d, &m).await.unwrap();

    assert_eq!(
        tg.downloads.load(Ordering::SeqCst),
        1,
        "граничный файл скачан"
    );
    assert_eq!(assets.snapshot().len(), 1);
    assert!(!sessions.prompts_snapshot().is_empty());
}
