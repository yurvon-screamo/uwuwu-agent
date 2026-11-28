//! Бот-задача: цикл TG long-poll → буфер ввода (#1596) → один prompt
//! вечной opencode-сессии на батч → ожидание реплая → sendMessage чанками.
//! Сообщения обрабатываются последовательно — контекст сессии консистентен.
//! Спавны — вне спавн-бюджета (bootstrap-гейтвей, #1150).

use super::{inbox, BotLive};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use uwuwu_domain::{AgentSlot, BotModel, ModelRef};
#[cfg(test)]
use uwuwu_usecase::bots::TgMessage;
use uwuwu_usecase::bots::{
    classify_message, BotStateStore, Incoming, InputBuffer, TelegramGateway, TgUpdate,
};
use uwuwu_usecase::{
    AgentSessionGateway, DreamerMark, Liveness, SessionSpec, TickStateStore, UsecaseResult,
};

pub(crate) fn tg_poll_sec() -> u64 {
    uwuwu_usecase::runtime_cfg::bots::tg_poll_sec()
}
pub(crate) fn retry_pause_ms() -> u64 {
    uwuwu_usecase::runtime_cfg::bots::retry_pause_ms()
}
const FALLBACK: &str = "Не успел ответить — напиши ещё раз.";
/// Потолок скачивания Bot API: документированные «20 MB» — двоичные,
/// граница getFile ровно 20 971 520 B (план #1171).
pub(crate) const TG_MAX_FILE_BYTES: i64 = 20 * 1024 * 1024;
pub(crate) const REFUSE_KIND: &str =
    "Такой тип файлов не принимаю (видео/голос/стикер) — пришли фото \
или файлом (как документ), либо кинь файл прямо в задачу на GitHub.";
pub(crate) const REFUSE_OVERSIZE: &str =
    "Файл слишком большой (>20 МБ) — Telegram не даёт ботам его \
скачать. Приложи файл прямо в задачу на GitHub.";
pub(crate) const REFUSE_FAILED: &str =
    "Не смог забрать файл — пришли заново или кинь прямо в задачу \
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
    /// dreamer-метка Forever бот-сессий (не источники опыта).
    pub tick_state: Arc<dyn TickStateStore>,
    /// Потолок ожидания реплая и шаг опроса (константы — поля ради тестов).
    pub reply_cap_ms: u64,
    pub reply_poll_ms: u64,
    /// Окно буфера ввода (#1596): минута в проде, короткое в тестах.
    pub debounce_ms: u64,
    /// Пауза между поллами: после сбоя обработки и в poll-fail ветке.
    pub retry_pause_ms: u64,
}

enum Absorb {
    /// Ввод принят в буфер — окно перезапускается.
    Done,
    /// Апдейт поглощён без ввода — таймер не трогается.
    Noop,
    /// Сбой отправки greeting — offset держится до ретрая.
    Held,
}

/// Запустить бот-задача; падение задачи — рестарт с луп-гвардом супервизора.
pub(crate) fn spawn_bot_task(deps: BotDeps) -> JoinHandle<()> {
    let slug = deps.slug.clone();
    tokio::spawn(async move {
        tracing::info!(event = "bot.started", slug = %deps.slug, "Бот-задача запущена");
        let mut buffer = InputBuffer::default();
        let mut deadline: Option<Instant> = None;
        // Запросный offset двигается при приёме; в state он пишется только
        // после успешного сброса (#1596) — рестарт перечитает бандл.
        let mut poll_offset = deps.state.bot_offset(&deps.slug);
        loop {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                flush_pending(&deps, &buffer).await;
                buffer = InputBuffer::default();
                deadline = None;
                continue;
            }
            // 25-секундный long-poll не перескакивает через дедлайн окна.
            let wait_secs = deadline
                .map(|d| {
                    d.saturating_duration_since(Instant::now())
                        .as_secs()
                        .clamp(1, tg_poll_sec())
                })
                .unwrap_or(tg_poll_sec());
            match deps.tg.get_updates(poll_offset, wait_secs).await {
                Ok(updates) => {
                    let mut held = false;
                    for u in &updates {
                        match absorb_update(&deps, u, &mut buffer).await {
                            Absorb::Done => {
                                poll_offset = Some(u.update_id + 1);
                                deadline =
                                    Some(Instant::now() + Duration::from_millis(deps.debounce_ms));
                            }
                            Absorb::Noop => {
                                poll_offset = Some(u.update_id + 1);
                                // Поглощённые апдейты подтверждаются сразу,
                                // но не раньше сброса ждущего батча:
                                // иначе падение потеряло бы ждущие вводы.
                                if buffer.is_empty() {
                                    confirm_offset(&deps, u.update_id + 1, "поглощённого апдейта");
                                }
                            }
                            Absorb::Held => {
                                held = true;
                                break;
                            }
                        }
                    }
                    if held {
                        tracing::warn!(event = "bot.offset_held", slug = %slug,
                            "greeting не отправлен — offset держится до ретрая");
                        tokio::time::sleep(Duration::from_millis(deps.retry_pause_ms)).await;
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

/// Сброс с ретраем и подтверждение offset батча (#1596): offset пишется
/// только после успешной доставки, при сбое бандл остаётся в буфере.
async fn flush_pending(deps: &BotDeps, buffer: &InputBuffer) {
    while let Err(e) = inbox::flush(deps, buffer).await {
        tracing::warn!(event = "bot.flush_failed", slug = %deps.slug, error = %e,
            "Сброс буфера не удался — повтор");
        tokio::time::sleep(Duration::from_millis(deps.retry_pause_ms)).await;
    }
    let max_id = buffer.max_update_id();
    if max_id > 0 {
        confirm_offset(deps, max_id + 1, "батча");
    }
}

fn confirm_offset(deps: &BotDeps, offset: u64, source: &str) {
    deps.state.set_bot_offset(&deps.slug, offset);
    if let Err(e) = deps.state.save() {
        tracing::warn!(event = "bot.state_save_failed", slug = %deps.slug, error = %e,
            "Offset {source} не персистится");
    }
}

/// Апдейт → буфер. message и edited_message идут одним путём (#1596);
/// не-private/чужой/не-сообщение поглощаются без перезапуска окна.
async fn absorb_update(deps: &BotDeps, u: &TgUpdate, buffer: &mut InputBuffer) -> Absorb {
    let (message, is_edit) = match (&u.message, &u.edited_message) {
        (Some(m), _) => (m, false),
        (None, Some(m)) => (m, true),
        (None, None) => return Absorb::Noop,
    };
    let view = deps.live.view();
    let Some(incoming) = classify_message(message, view.owner.as_deref()) else {
        tracing::info!(event = "bot.skipped", slug = %deps.slug, chat_id = message.chat_id,
            "Не private или не владелец — молча");
        return Absorb::Noop;
    };
    if matches!(&incoming, Incoming::Text(text) if text == "/start") {
        if let Some(greeting) = view.greeting {
            if let Err(e) = send(deps, message.chat_id, &greeting).await {
                tracing::warn!(event = "bot.greeting_failed", slug = %deps.slug, error = %e,
                    "Greeting не отправлен — offset держится");
                return Absorb::Held;
            }
        }
        return Absorb::Noop;
    }
    buffer.push(u.update_id, message.clone(), is_edit);
    Absorb::Done
}

/// Адаптер для прямых вызовов одного сообщения из тестов: тот же сброс,
/// что и в раннере, без накопления окна. В проде не используется.
#[cfg(test)]
pub(crate) async fn process_message(deps: &BotDeps, m: &TgMessage) -> UsecaseResult<()> {
    let mut buffer = InputBuffer::default();
    buffer.push(0, m.clone(), false);
    inbox::flush(deps, &buffer).await
}

/// Форвард-маркер промпта (#1416): строка-префикс с источником; для
/// обычного сообщения — пустая строка, промпт байт-в-байт прежний.
/// Единственная точка формата на обе ветки доставки — контракт маркера
/// носится тремя носителями (сервер ⇄ методичка ⇄ grep e2e-стенда),
/// дрейф формулировки между ветками исключён (ревью #1416, common).
pub(crate) fn forward_line(forward: &Option<String>) -> String {
    match forward {
        Some(origin) => format!("[Пересланный пост — {origin}]\n"),
        None => String::new(),
    }
}

/// Ход сессии (общий для текста и media): методичка при устаревании,
/// prompt вечной сессии, ожидание реплая, отправка владельцу.
pub(crate) async fn run_turn(deps: &BotDeps, chat_id: i64, text: &str) -> UsecaseResult<()> {
    let view = deps.live.view();
    let session = ensure_session(deps).await?;
    // Методичка НЕ префиксует каждое сообщение (владелец 16.11):
    // - agent-режим: методичка в agents/<name>.md, промпт — чистый текст;
    // - doc-режим: методичка едет одним сообщением при создании/правке
    //   (hash-детект в state) и остаётся в памяти вечной сессии.
    let manual_hash = view.manual.len() as u64 ^ view.manual.chars().count() as u64;
    let need_manual = deps.state.bot_manual_hash(&deps.slug) != Some(manual_hash);
    // Уроки (agent-режим, #1424): префикс одним сообщением при смене
    // носителя; doc-режим везёт уроки в композите методички. Пустой
    // носитель при доставлявшихся ранее уроках — тоже смена: бот обязан
    // узнать об отзыве (гейт «!empty» не режет), а бот без уроков от
    // рождения — без промпта (регресс-гейт «дока нет»).
    let lessons_hash = view.lessons.len() as u64 ^ view.lessons.chars().count() as u64;
    let stored_lessons = deps.state.bot_lessons_hash(&deps.slug);
    let lessons_empty = view.lessons.trim().is_empty();
    let need_lessons = deps.structural.agent.is_some()
        && stored_lessons != Some(lessons_hash)
        && (!lessons_empty || stored_lessons.is_some());
    let prompt_text = if deps.structural.agent.is_some() {
        if need_lessons && lessons_empty {
            uwuwu_usecase::bots::build_lessons_revoked_prompt(text)
        } else if need_lessons {
            uwuwu_usecase::bots::build_lessons_prompt(&view.lessons, text)
        } else {
            text.to_string()
        }
    } else if need_manual {
        uwuwu_usecase::bots::build_prompt(&view.manual, text)
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
        deps.state
            .set_bot_manual_hash(&deps.slug, Some(manual_hash));
        if let Err(e) = deps.state.save() {
            tracing::warn!(event = "bot.state_save_failed", slug = %deps.slug, error = %e,
                "Manual-hash не персистится — методичка уедет повторно после рестарта");
        }
    }
    if need_lessons {
        deps.state
            .set_bot_lessons_hash(&deps.slug, Some(lessons_hash));
        if let Err(e) = deps.state.save() {
            tracing::warn!(event = "bot.state_save_failed", slug = %deps.slug, error = %e,
                "Lessons-hash не персистится — уроки уедут повторно после рестарта");
        }
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
/// Бот-сессии маркируются DreamerMark::Forever — дример их не подбирает.
async fn ensure_session(deps: &BotDeps) -> UsecaseResult<String> {
    if let Some(s) = deps.state.bot_session(&deps.slug) {
        match deps.sessions.liveness(&s).await {
            // Unknown (гейт недоступен) ≠ Dead: переиспользование — рестарт
            // гейта не должен плодить дубли сессий (канон 15.11)
            Liveness::Alive | Liveness::Unknown => return Ok(s),
            Liveness::Dead => {
                tracing::warn!(event = "bot.session_dead", slug = %deps.slug, session = %s,
                    "Сессия не жива — пересоздание");
                // Методичка вшита в мёртвую сессию (doc-режим) — свежая её
                // не получит: сброс hash = дозаправка первым же промптом
                // (#1221 тест-круг 1, тот же инвариант, что у компакта и
                // recreate-ветки супервизора; персист — save() пути
                // пересоздания ниже). Уроки (agent-режим) ехали префиксом
                // в историю мёртвой сессии — сброс lessons-hash тоже.
                deps.state.set_bot_manual_hash(&deps.slug, None);
                deps.state.set_bot_lessons_hash(&deps.slug, None);
            }
        }
    }
    let m = &deps.structural.model;
    let spec = SessionSpec {
        custom_agent: deps.structural.agent.clone(),
        directory: deps.structural.workspace.clone(),
        title: format!("Бот: {}", deps.structural.title),
        agent: AgentSlot::Default,
        model: ModelRef::new(m.provider.clone(), m.id.clone()),
        variant: m.variant.clone(),
        bootstrap: true,
    };
    let id = deps.sessions.create_session(&spec).await?;
    deps.state.set_bot_session(&deps.slug, Some(&id));
    deps.state.save()?;
    deps.tick_state.mark_dreamer(&id, DreamerMark::Forever);
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

pub(crate) async fn send(deps: &BotDeps, chat_id: i64, text: &str) -> UsecaseResult<()> {
    for chunk in uwuwu_usecase::bots::chunk_reply(text) {
        deps.tg.send_message(chat_id, &chunk).await?;
    }
    Ok(())
}
