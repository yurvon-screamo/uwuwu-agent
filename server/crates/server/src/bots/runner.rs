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
    build_prompt, chunk_reply, classify_message, BotStateStore, Incoming, MediaKind,
    TelegramGateway, TgMedia, TgMessage, TgUpdate,
};
use uwuwu_usecase::{AgentSessionGateway, Liveness, SessionSpec, TickStateStore, UsecaseResult};

pub(crate) const TG_POLL_SEC: u64 = 25;
pub(crate) const RETRY_PAUSE_MS: u64 = 10_000;
const FALLBACK: &str = "Не успел ответить — напиши ещё раз.";
/// Потолок скачивания Bot API: документированные «20 MB» — двоичные,
/// граница getFile ровно 20 971 520 B (план #1171).
const TG_MAX_FILE_BYTES: i64 = 20 * 1024 * 1024;
const REFUSE_KIND: &str = "Такой тип файлов не принимаю (видео/голос/стикер) — пришли фото \
или файлом (как документ), либо кинь файл прямо в задачу на GitHub.";
const REFUSE_OVERSIZE: &str = "Файл слишком большой (>20 МБ) — Telegram не даёт ботам его \
скачать. Приложи файл прямо в задачу на GitHub.";
const REFUSE_FAILED: &str = "Не смог забрать файл — пришли заново или кинь прямо в задачу \
на GitHub.";

/// Структурные поля спеки: фиксируются на жизнь задачи (смена = рестарт
/// задачи супервизором, для recreate — ещё и пересоздание сессии).
#[derive(Clone)]
pub(crate) struct BotStructural {
    pub title: String,
    pub model: BotModel,
    pub workspace: String,
    /// fm agent — именованный агент опенкода (agents/<name>.md в
    /// workspace): методичка в agent-md, промпты — чистый текст.
    pub agent: Option<String>,
}

pub(crate) struct BotDeps {
    pub slug: String,
    pub structural: BotStructural,
    pub live: Arc<BotLive>,
    pub tg: Arc<dyn TelegramGateway>,
    pub sessions: Arc<dyn AgentSessionGateway>,
    pub state: Arc<dyn BotStateStore>,
    /// Приёмник ассетов бота (media-файлы → URL в hub, #1171).
    pub assets: Arc<dyn uwuwu_usecase::bots::BotAssetStore>,
    /// dreamer_done-маркировка бот-сессий (не источники опыта).
    pub tick_state: Arc<dyn TickStateStore>,
    /// Потолок ожидания реплая и шаг опроса (константы — поля ради тестов).
    pub reply_cap_ms: u64,
    pub reply_poll_ms: u64,
    /// Пауза между поллами: после сбоя обработки и в poll-fail ветке.
    pub retry_pause_ms: u64,
}

/// Запустить бот-задачу; падение задачи — рестарт с луп-гвардом супервизора.
pub(crate) fn spawn_bot_task(deps: BotDeps) -> JoinHandle<()> {
    let slug = deps.slug.clone();
    tokio::spawn(async move {
        tracing::info!(event = "bot.started", slug = %deps.slug, "Бот-задача запущена");
        let mut offset = deps.state.bot_offset(&deps.slug);
        loop {
            match deps.tg.get_updates(offset, TG_POLL_SEC).await {
                Ok(updates) => {
                    for u in updates {
                        if !handle_update(&deps, &u).await {
                            // Обработка не удалась: offset НЕ двигается —
                            // апдейт и хвост батча переедут с следующим
                            // поллом (план #1150: offset персистится после
                            // отправки; дубль при ретрае лучше потери).
                            tracing::warn!(event = "bot.offset_held", slug = %deps.slug,
                                update_id = u.update_id, "Апдейт не обработан — ретрай");
                            tokio::time::sleep(Duration::from_millis(deps.retry_pause_ms)).await;
                            break;
                        }
                        offset = Some(u.update_id + 1);
                        deps.state.set_bot_offset(&deps.slug, u.update_id + 1);
                        if let Err(e) = deps.state.save() {
                            tracing::warn!(event = "bot.state_save_failed", slug = %deps.slug,
                                error = %e, "Offset не персистится");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(event = "bot.poll_failed", slug = %slug, error = %e,
                        "TG long-poll failed — пауза до следующей попытки");
                    tokio::time::sleep(Duration::from_millis(deps.retry_pause_ms)).await;
                }
            }
        }
    })
}

/// Апдейт: true — обработан/поглощён (offset можно двигать), false —
/// сбой обработки (offset держится до успешного ретрая).
async fn handle_update(deps: &BotDeps, u: &TgUpdate) -> bool {
    let Some(m) = &u.message else {
        return true; // не-message апдейт поглощается без обработки
    };
    if let Err(e) = process_message(deps, m).await {
        tracing::warn!(event = "bot.process_failed", slug = %deps.slug,
            update_id = u.update_id, error = %e, "Апдейт не обработан");
        return false;
    }
    true
}

pub(crate) async fn process_message(deps: &BotDeps, m: &TgMessage) -> UsecaseResult<()> {
    let view = deps.live.view();
    let Some(incoming) = classify_message(m, view.owner.as_deref()) else {
        tracing::info!(event = "bot.skipped", slug = %deps.slug, chat_id = m.chat_id,
            "Не private или не владелец — молча");
        return Ok(());
    };
    match incoming {
        Incoming::Text(text) => {
            if text == "/start" {
                if let Some(greeting) = view.greeting {
                    return send(deps, m.chat_id, &greeting).await;
                }
            }
            run_turn(deps, m.chat_id, &text).await
        }
        Incoming::Media { caption, media } => process_media(deps, m.chat_id, caption, media).await,
    }
}

/// Media-ветка (#1171): photo/document ≤ 20 MiB — скачать → asset store →
/// промпт с URL (обычный ход сессии); oversize/чужой тип/сбой — явный
/// ТГ-отказ. Ни одна ветка не проходит молча; отказ поглощает апдейт
/// (Ok — не ретрай: перманентные ошибки залочили бы очередь бота).
async fn process_media(
    deps: &BotDeps,
    chat_id: i64,
    caption: Option<String>,
    media: TgMedia,
) -> UsecaseResult<()> {
    if media.kind == MediaKind::Other {
        tracing::info!(event = "bot.media_refused", slug = %deps.slug,
            kind = "other", "Медиа-тип не поддержан — явный отказ");
        return send(deps, chat_id, REFUSE_KIND).await;
    }
    if media.file_size.is_some_and(|s| s > TG_MAX_FILE_BYTES) {
        tracing::info!(event = "bot.media_oversize", slug = %deps.slug,
            kind = "pre_check", "Файл больше потолка Bot API — явный отказ");
        return send(deps, chat_id, REFUSE_OVERSIZE).await;
    }
    let bytes = match deps.tg.download_file(&media.file_id).await {
        Ok(b) => b,
        Err(uwuwu_usecase::UsecaseError::TelegramTooBig(_)) => {
            tracing::info!(event = "bot.media_oversize", slug = %deps.slug,
                kind = "get_file_400", "getFile: file is too big — явный отказ");
            return send(deps, chat_id, REFUSE_OVERSIZE).await;
        }
        Err(_) => {
            tracing::info!(event = "bot.media_refused", slug = %deps.slug,
                kind = "download_failed", "Скачивание не удалось — явный отказ");
            return send(deps, chat_id, REFUSE_FAILED).await;
        }
    };
    let url = match deps
        .assets
        .put(&deps.slug, media.kind, media.file_name.as_deref(), &bytes)
        .await
    {
        Ok(u) => u,
        Err(_) => {
            tracing::info!(event = "bot.media_refused", slug = %deps.slug,
                kind = "upload_failed", "Загрузка ассета не удалась — явный отказ");
            return send(deps, chat_id, REFUSE_FAILED).await;
        }
    };
    tracing::info!(event = "bot.media_delivered", slug = %deps.slug,
        kind = ?media.kind, "Файл забран и приложен к контексту сессии");
    let caption_line = caption.unwrap_or_else(|| "Подписи нет".into());
    let media_text = format!(
        "Владелец прислал файл: {url}\n{caption_line}\n\
Если оформляешь заявку — вложи эту ссылку в тело задачи (markdown), \
чтобы контент попал в разбор."
    );
    run_turn(deps, chat_id, &media_text).await
}

/// Ход сессии (общий для текста и media): методичка при устаревании,
/// prompt вечной сессии, ожидание реплая, отправка владельцу.
async fn run_turn(deps: &BotDeps, chat_id: i64, text: &str) -> UsecaseResult<()> {
    let view = deps.live.view();
    let session = ensure_session(deps).await?;
    // Методичка НЕ префиксует каждое сообщение (владелец 16.11):
    // - agent-режим: методичка в agents/<name>.md, промпт — чистый текст;
    // - doc-режим: методичка едет одним сообщением при создании/правке
    //   (hash-детект в state) и остаётся в памяти вечной сессии.
    let manual_hash = view.manual.len() as u64 ^ view.manual.chars().count() as u64;
    let need_manual = deps.state.bot_manual_hash(&deps.slug) != Some(manual_hash);
    let prompt_text = if deps.structural.agent.is_some() {
        text.to_string()
    } else if need_manual {
        build_prompt(&view.manual, text)
    } else {
        text.to_string()
    };
    let baseline = deps
        .sessions
        .last_assistant_reply(&session)
        .await?
        .map(|r| r.created_ms)
        .unwrap_or(0);
    deps.sessions.prompt(&session, &prompt_text).await?;
    if need_manual && deps.structural.agent.is_none() {
        deps.state.set_bot_manual_hash(&deps.slug, manual_hash);
        deps.state.save().ok();
    }
    match wait_reply(deps, &session, baseline).await {
        Some(reply) if !reply.trim().is_empty() => send(deps, chat_id, &reply).await?,
        _ => {
            tracing::warn!(event = "bot.reply_timeout", slug = %deps.slug, session = %session,
                cap_ms = deps.reply_cap_ms, "Ответ не дождались — фоллбек владельцу");
            send(deps, chat_id, FALLBACK).await?;
        }
    }
    tracing::info!(event = "bot.replied", slug = %deps.slug, chat_id = chat_id,
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
        custom_agent: deps.structural.agent.clone(),
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

/// Опрос хвоста сессии: финальное сообщение завершённого хода (после
/// baseline). None — потолок ожидания исчерпан.
async fn wait_reply(deps: &BotDeps, session: &str, baseline_ms: u64) -> Option<String> {
    let deadline = Instant::now() + Duration::from_millis(deps.reply_cap_ms);
    loop {
        tokio::time::sleep(Duration::from_millis(deps.reply_poll_ms)).await;
        if let Ok(Some(r)) = deps.sessions.last_assistant_reply(session).await {
            // Ход завершён = свежее содержательное сообщение ассистента с
            // finish-маркером НЕ 'tool-calls' (и не None — ещё стримится).
            // Промежуточный шаг хода несёт 'tool-calls' и с is_latest=true
            // наблюдается поллингом в зазоре «тул исполнен — следующий
            // шаг ещё не создан» (живая проба #1209, opencode 2.3.0:
            // finish ставится после тула, зазор 0.8–1.5с при полле 2с —
            // ~40% на шаг); finished-без-текста покрывается проверкой
            // текста (инцидент 16.11: фоллбек за минуту).
            if r.created_ms > baseline_ms
                && matches!(r.finish.as_deref(), Some(f) if f != "tool-calls")
                && !r.text.trim().is_empty()
            {
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
