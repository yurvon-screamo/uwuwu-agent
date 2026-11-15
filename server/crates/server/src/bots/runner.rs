//! Бот-задача: цикл TG long-poll → входящее → prompt вечной opencode-сессии
//! (методичка префиксом) → ожидание реплая опросом хвоста → sendMessage
//! чанками. Сообщения обрабатываются последовательно — контекст сессии
//! консистентен. Спавны — вне спавн-бюджета (bootstrap-гейтвей, #1150).

use super::BotLive;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use uwuwu_domain::{AgentSlot, BotModel, ModelRef};
use uwuwu_usecase::bots::{
    build_prompt, chunk_reply, classify_message, BotStateStore, TelegramGateway, TgMessage,
    TgUpdate,
};
use uwuwu_usecase::{AgentSessionGateway, Liveness, SessionSpec, TickStateStore, UsecaseResult};

pub(crate) const TG_POLL_SEC: u64 = 25;
const POLL_RETRY_MS: u64 = 10_000;
const FALLBACK: &str = "Не успел ответить — напиши ещё раз.";

/// Структурные поля спеки: фиксируются на жизнь задачи (смена = рестарт
/// задачи супервизором, для recreate — ещё и пересоздание сессии).
#[derive(Clone)]
pub(crate) struct BotStructural {
    pub title: String,
    pub model: BotModel,
    pub workspace: String,
}

pub(crate) struct BotDeps {
    pub slug: String,
    pub structural: BotStructural,
    pub live: Arc<BotLive>,
    pub tg: Arc<dyn TelegramGateway>,
    pub sessions: Arc<dyn AgentSessionGateway>,
    pub state: Arc<dyn BotStateStore>,
    /// dreamer_done-маркировка бот-сессий (не источники опыта).
    pub tick_state: Arc<dyn TickStateStore>,
    /// Потолок ожидания реплая и шаг опроса (константы — поля ради тестов).
    pub reply_cap_ms: u64,
    pub reply_poll_ms: u64,
}

/// Запустить бот-задачу; падение задачи — рестарт с луп-гвардом супервизора.
pub(crate) fn spawn_bot_task(deps: BotDeps) -> JoinHandle<()> {
    let slug = deps.slug.clone();
    tokio::spawn(async move {
        tracing::info!(event = "bot.started", slug = %deps.slug, "Бот-задача запущена");
        loop {
            let offset = deps.state.bot_offset(&deps.slug);
            match deps.tg.get_updates(offset, TG_POLL_SEC).await {
                Ok(updates) => {
                    for u in updates {
                        handle_update(&deps, u).await;
                    }
                }
                Err(e) => {
                    tracing::warn!(event = "bot.poll_failed", slug = %slug, error = %e,
                        "TG long-poll failed — пауза до следующей попытки");
                    tokio::time::sleep(Duration::from_millis(POLL_RETRY_MS)).await;
                }
            }
        }
    })
}

/// Апдейт обрабатывается, затем offset растёт (персистится) — дубль-ответ
/// при крахе mid-processing принят осознанно (редкий класс, план #1150).
async fn handle_update(deps: &BotDeps, u: TgUpdate) {
    if let Some(m) = &u.message {
        if let Err(e) = process_message(deps, m).await {
            tracing::warn!(event = "bot.process_failed", slug = %deps.slug,
                update_id = u.update_id, error = %e, "Апдейт не обработан");
        }
    }
    deps.state.set_bot_offset(&deps.slug, u.update_id + 1);
    if let Err(e) = deps.state.save() {
        tracing::warn!(event = "bot.state_save_failed", slug = %deps.slug, error = %e,
            "Offset не персистится");
    }
}

async fn process_message(deps: &BotDeps, m: &TgMessage) -> UsecaseResult<()> {
    let view = deps.live.view();
    let Some(text) = classify_message(m, view.owner.as_deref()) else {
        tracing::debug!(event = "bot.skipped", slug = %deps.slug, chat_id = m.chat_id,
            "Не private или не владелец — молча");
        return Ok(());
    };
    if text == "/start" {
        if let Some(greeting) = view.greeting {
            return send(deps, m.chat_id, &greeting).await;
        }
    }
    let session = ensure_session(deps).await?;
    let baseline = deps
        .sessions
        .last_assistant_reply(&session)
        .await?
        .map(|r| r.created_ms)
        .unwrap_or(0);
    deps.sessions
        .prompt(&session, &build_prompt(&view.manual, &text))
        .await?;
    match wait_reply(deps, &session, baseline).await {
        Some(reply) if !reply.trim().is_empty() => send(deps, m.chat_id, &reply).await?,
        _ => {
            tracing::warn!(event = "bot.reply_timeout", slug = %deps.slug, session = %session,
                cap_ms = deps.reply_cap_ms, "Ответ не дождались — фоллбек владельцу");
            send(deps, m.chat_id, FALLBACK).await?;
        }
    }
    tracing::info!(event = "bot.replied", slug = %deps.slug, chat_id = m.chat_id,
        "Ответ отправлен");
    Ok(())
}

/// Вечная сессия бота: живая из state — переиспользуется; иначе создаётся
/// (workspace+model биндятся при создании; bootstrap — вне спавн-бюджета).
/// Бот-сессии маркируются dreamer_done — дример их не подбирает.
async fn ensure_session(deps: &BotDeps) -> UsecaseResult<String> {
    if let Some(s) = deps.state.bot_session(&deps.slug) {
        match deps.sessions.liveness(&s).await {
            // Unknown (гейт недоступен) ≠ Dead: переиспользование — рестарт
            // гейта не должен плодить дубли сессий (канон 15.11)
            Liveness::Alive | Liveness::Unknown => return Ok(s),
            Liveness::Dead => {
                tracing::warn!(event = "bot.session_dead", slug = %deps.slug, session = %s,
                    "Сессия не жива — пересоздание");
            }
        }
    }
    let m = &deps.structural.model;
    let spec = SessionSpec {
        directory: deps.structural.workspace.clone(),
        title: format!("Бот: {}", deps.structural.title),
        agent: AgentSlot::Default,
        model: Some(ModelRef::new(m.provider.clone(), m.id.clone())),
        variant: m.variant.clone(),
        bootstrap: true,
    };
    let id = deps.sessions.create_session(&spec).await?;
    deps.state.set_bot_session(&deps.slug, Some(&id));
    deps.state.save()?;
    deps.tick_state.mark_dreamer_done(&id);
    tracing::info!(event = "bot.session_created", slug = %deps.slug, session = %id,
        model_provider = %m.provider, model_id = %m.id, "Сессия бота создана");
    Ok(id)
}

/// Опрос хвоста сессии: ассистентский ответ, созданный после baseline
/// и с finish-маркером. None — потолок ожидания исчерпан.
async fn wait_reply(deps: &BotDeps, session: &str, baseline_ms: u64) -> Option<String> {
    let deadline = Instant::now() + Duration::from_millis(deps.reply_cap_ms);
    loop {
        tokio::time::sleep(Duration::from_millis(deps.reply_poll_ms)).await;
        if let Ok(Some(r)) = deps.sessions.last_assistant_reply(session).await {
            if r.created_ms > baseline_ms && r.finished {
                return Some(r.text);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
    }
}

async fn send(deps: &BotDeps, chat_id: i64, text: &str) -> UsecaseResult<()> {
    for chunk in chunk_reply(text) {
        deps.tg.send_message(chat_id, &chunk).await?;
    }
    Ok(())
}
