//! Супервизор ботов: тик = скан `bot/<имя>/README.md` (#1214) → диф с запущенными
//! → spawn / stop / recreate-session / re-arm / hot. Мёртвая задача —
//! рестарт с экспоненциальным бэкоффом (потолок ~30 мин, сброс после
//! ~10 мин здоровой работы). Тик быстрый (wiki-скан) — живёт обычным
//! сайдкаром; long-poll — внутри задач раннеров, тик-луп не блокирует.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uwuwu_domain::{BotAction, BotSpec, ModelRef};
use uwuwu_usecase::bots::{
    compact_verdict, compose_manual, normalize_lessons, parse_bot_doc, plan_action, BotAssetStore,
    BotStateStore, CompactFacts, CompactPolicy, TelegramGateway,
};
use uwuwu_usecase::{
    AgentSessionGateway, Clock, Liveness, TickStateStore, UsecaseResult, WikiRepo,
};

use super::runner::{spawn_bot_task, BotStructural, RETRY_PAUSE_MS};
use super::BotLive;

/// Фабрика ТГ-клиента: прод — TelegramClient(token), тесты/e2e — фейки.
pub(crate) type TgFactory = Arc<dyn Fn(&str) -> Arc<dyn TelegramGateway> + Send + Sync>;
/// Резолв токена по имени env-переменной (прод — std::env, тесты — map).
pub(crate) type TokenLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Ритм луп-гварда рестартов.
#[derive(Clone, Copy)]
pub(crate) struct GuardTiming {
    pub backoff_base_ms: u64,
    pub backoff_cap_ms: u64,
    /// Здоровая работа дольше этого — счётчик фейлов сбрасывается.
    pub healthy_ms: u64,
}

impl Default for GuardTiming {
    fn default() -> Self {
        Self {
            backoff_base_ms: 30_000,
            backoff_cap_ms: 1_800_000,
            healthy_ms: 600_000,
        }
    }
}

struct Running {
    /// Последняя виденная спека (структура/токен — база дифа).
    spec: BotSpec,
    live: Arc<BotLive>,
    task: tokio::task::JoinHandle<()>,
    started_ms: u64,
    fails: u32,
    /// Смерть задачи уже засчитана в fails (бэкофф растёт по смертям,
    /// не по тикам ожидания).
    dead_seen: bool,
}

/// Причина погашения по записи скана: None — запись жива и включена,
/// гасить нечего. Классификация вынесена для теста регресса диагностики.
pub(crate) fn stop_reason(parsed: Option<&Result<BotSpec, String>>) -> Option<&'static str> {
    match parsed {
        None => Some("док снесён или переименован"),
        Some(Err(_)) => Some("спека не парсится (fm сломан)"),
        Some(Ok(b)) if !b.enabled => Some("enabled:false"),
        Some(Ok(_)) => None,
    }
}

/// Живое представление из спеки (#1424): методичка — композит с уроками
/// (doc-режим, hash-гейт manual), уроки отдельно (agent-режим, префикс).
fn live_of(spec: &BotSpec, lessons: &str) -> BotLive {
    BotLive::with_lessons(
        spec.owner.clone(),
        spec.greeting.clone(),
        compose_manual(&spec.manual, lessons),
        lessons.to_string(),
    )
}

pub struct BotSupervisor {
    wiki: Arc<dyn WikiRepo>,
    /// Raw-гейтвей: бот-спавны вне спавн-бюджета (create редкий — один на
    /// жизнь сессии; шторм гасит собственный луп-гвард).
    sessions: Arc<dyn AgentSessionGateway>,
    state: Arc<dyn BotStateStore>,
    /// Приёмник ассетов ботов: media-файлы владельца → URL в hub (#1171).
    assets: Arc<dyn BotAssetStore>,
    /// бот-сессии → DreamerMark::Forever (дример их не подбирает, runner.rs).
    tick_state: Arc<dyn TickStateStore>,
    clock: Arc<dyn Clock>,
    make_tg: TgFactory,
    pub(crate) token_of: TokenLookup,
    /// Потолок/шаг ожидания реплая (поля ради тестов).
    pub(crate) reply_cap_ms: u64,
    pub(crate) reply_poll_ms: u64,
    pub(crate) guard: GuardTiming,
    /// Политика автокомпакта (#1221): None — механика off (safe default,
    /// секция [compact] конфига отсутствует).
    pub(crate) compact: Option<CompactPolicy>,
    running: Mutex<HashMap<String, Running>>,
}

impl BotSupervisor {
    pub fn new(
        wiki: Arc<dyn WikiRepo>,
        sessions: Arc<dyn AgentSessionGateway>,
        state: Arc<dyn BotStateStore>,
        assets: Arc<dyn BotAssetStore>,
        tick_state: Arc<dyn TickStateStore>,
        clock: Arc<dyn Clock>,
        make_tg: TgFactory,
    ) -> Self {
        Self {
            wiki,
            sessions,
            state,
            assets,
            tick_state,
            clock,
            make_tg,
            token_of: Arc::new(|env| std::env::var(env).ok().filter(|v| !v.trim().is_empty())),
            // 3600с (владелец 16.11: «15 мало — делай час»): тяжёлые
            // методички/длинные генерации; полл 2с ловит ответ раньше
            reply_cap_ms: 3_600_000,
            reply_poll_ms: 2_000,
            guard: GuardTiming::default(),
            compact: None,
            running: Mutex::new(HashMap::new()),
        }
    }

    /// Наблюдаемость для тестов: бот сейчас в реестре рантайма.
    #[cfg(test)]
    pub(crate) fn is_running(&self, slug: &str) -> bool {
        self.running.lock().expect("bots lock").contains_key(slug)
    }

    /// Наблюдаемость для тестов: задача бота умерла (висит в реестре
    /// мёртвой — ждёт бэкоффа/рестарта).
    #[cfg(test)]
    pub(crate) fn running_task_dead(&self, slug: &str) -> bool {
        self.running
            .lock()
            .expect("bots lock")
            .get(slug)
            .map(|r| r.task.is_finished())
            .unwrap_or(false)
    }

    /// Последние известные уроки бота (#1424, ревью раунд 2): `None`
    /// спеки — читать не удалось, не равно «носитель опустел» — живое
    /// представление сохраняет предыдущие уроки, гейт отзыва в
    /// agent-режиме не срабатывает на транзиентном сбое чтения.
    fn resolve_lessons(
        &self,
        running: &HashMap<String, Running>,
        slug: &str,
        spec: &BotSpec,
    ) -> String {
        match spec.lessons.as_deref() {
            Some(l) => l.to_string(),
            None => running
                .get(slug)
                .map(|r| r.live.view().lessons)
                .unwrap_or_default(),
        }
    }

    pub async fn tick(&self) -> UsecaseResult<()> {
        let scanned = self.scan().await?;
        tracing::info!(
            event = "bot.scan",
            found = scanned.len(),
            running = self.running.lock().expect("bots lock").len(),
            "Супервизор: скан реестра"
        );
        for (slug, spec) in &scanned {
            if let Err(e) = spec {
                tracing::warn!(event = "bot.spec_invalid", slug, error = %e,
                    "Док-спека не парсится — бот не поднимается (молчаливый пропуск прячет опечатку)");
            }
        }
        {
            let mut running = self.running.lock().expect("bots lock");
            self.apply_stops(&mut running, &scanned);
            self.revive_dead(&mut running, &scanned);
            self.apply_diffs(&mut running, &scanned);
        } // гард умер в скоупе — future остаётся Send (await ниже)
        self.compact_pass().await;
        Ok(())
    }

    /// «Сессия потеряна исполнителем» (404-класс отказа пробы):
    /// liveness-проба различает Dead от транспортного Unknown — пропуск
    /// только по Dead, Unknown завершает проход (транспорт от гейта не
    /// отличить — не долбим).
    async fn session_gone(&self, session: &str) -> bool {
        matches!(self.sessions.liveness(session).await, Liveness::Dead)
    }

    /// Компакт-проход (#1221): по живым ботам с сессией — факты (busy/
    /// idle/занятость окна) → матрица вердикта → нативный compact
    /// admission в окне неактивности. Секция конфига отсутствует —
    /// no-op. Отказ union-скана — проход пропущен; транспортный отказ
    /// пробы — проход тика завершён; потерянная сессия (404-класс) —
    /// мимо одного бота, соседи продолжаются (ревью L1 #1221).
    async fn compact_pass(&self) {
        let Some(policy) = self.compact else { return };
        let scan = match self.sessions.list_sessions().await {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(event = "bot.compact_scan_failed", error = %e,
                    "Union-скан недоступен — компакт-проход пропущен");
                return;
            }
        };
        let now = self.clock.now_ms();
        let snapshot: Vec<(String, BotSpec)> = {
            let running = self.running.lock().expect("bots lock");
            running
                .iter()
                .map(|(slug, r)| (slug.clone(), r.spec.clone()))
                .collect()
        };
        for (slug, spec) in snapshot {
            let Some(session) = self.state.bot_session(&slug) else {
                continue; // сессии ещё нет — компактировать нечего
            };
            let busy = scan
                .iter()
                .find(|s| s.id == session)
                .map(|s| s.busy)
                .unwrap_or(false);
            let idle_ms = match self.sessions.last_activity_ms(&session).await {
                Ok(ms) => ms.map(|ms| now.saturating_sub(ms)),
                Err(e) => {
                    // 404-класс (сессия потеряна исполнителем) — мимо этого
                    // бота, соседи продолжают (ревью L1 #1221); прочий
                    // отказ (транспорт/гейт) — проход тика завершён, не
                    // долбим упавший исполнитель.
                    if self.session_gone(&session).await {
                        tracing::warn!(event = "bot.compact_session_gone", slug = %slug,
                            session = %session,
                            "Сессия потеряна исполнителем — бот пропущен, соседи продолжаются");
                        continue;
                    }
                    tracing::warn!(event = "bot.compact_probe_failed", slug = %slug,
                        error = %e, "Проба активности не удалась — компакт-проход завершён");
                    return;
                }
            };
            let model = ModelRef::new(spec.model.provider.clone(), spec.model.id.clone());
            let usage_pct = match self.sessions.context_usage_pct(&session, &model).await {
                Ok(u) => u,
                Err(e) => {
                    if self.session_gone(&session).await {
                        tracing::warn!(event = "bot.compact_session_gone", slug = %slug,
                            session = %session,
                            "Сессия потеряна исполнителем — бот пропущен, соседи продолжаются");
                        continue;
                    }
                    tracing::warn!(event = "bot.compact_probe_failed", slug = %slug,
                        error = %e, "Проба занятости не удалась — компакт-проход завершён");
                    return;
                }
            };
            let facts = CompactFacts {
                busy,
                idle_ms,
                usage_pct,
                last_compact_ms: self.state.bot_compact_ms(&slug),
                now_ms: now,
            };
            let verdict = compact_verdict(&facts, &policy);
            if !verdict.fire {
                tracing::debug!(event = "bot.compact_skip", slug = %slug,
                    reason = %verdict.reason, "Компакт: не стреляем");
                continue;
            }
            // Метка ДО POST (ADR-082): коулдаун переживает рестарт
            // сервера, повторная адмиссия исключена.
            self.state.set_bot_compact_ms(&slug, now);
            if let Err(e) = self.state.save() {
                tracing::warn!(event = "bot.state_save_failed", slug = %slug, error = %e,
                    "Метка компакта не персистится — рестарт может дать повторную адмиссию");
            }
            match self.sessions.compact_session(&session).await {
                Ok(()) => {
                    // doc-режим: бутстрап-методичка ушла в суммаризацию —
                    // дозаправка следующим промптом (сброс hash; уроки едут
                    // в композите — покрываются тем же сбросом);
                    // agent-режим: уроки ехали префиксом в историю — сброс
                    // lessons-hash дозаправит их (#1424); методичка в
                    // agents/<name>.md компактом не задевается.
                    if spec.agent.is_none() {
                        self.state.set_bot_manual_hash(&slug, None);
                        if let Err(e) = self.state.save() {
                            tracing::warn!(event = "bot.state_save_failed", slug = %slug,
                                error = %e, "Сброс manual-hash не персистится — методичка не дозаправится");
                        }
                    } else {
                        self.state.set_bot_lessons_hash(&slug, None);
                        if let Err(e) = self.state.save() {
                            tracing::warn!(event = "bot.state_save_failed", slug = %slug,
                                error = %e, "Сброс lessons-hash не персистится — уроки не дозаправятся");
                        }
                    }
                    tracing::info!(event = "bot.compacted", slug = %slug,
                        session = %session, reason = %verdict.reason,
                        "Нативный compact admitted");
                }
                Err(e) => {
                    tracing::warn!(event = "bot.compact_failed", slug = %slug,
                        session = %session, error = %e,
                        "Адмиссия компакта не удалась — ретрай по коулдауну");
                }
            }
        }
    }

    async fn scan(&self) -> UsecaseResult<Vec<(String, Result<BotSpec, String>)>> {
        let mut out = Vec::new();
        for slug in self.wiki.list_docs("bot").await? {
            let Some(rest) = slug.strip_prefix("bot/") else {
                continue;
            };
            let Some(name) = rest.strip_suffix("/README") else {
                continue;
            };
            if name.is_empty() || name.contains('/') {
                continue;
            }
            if let Some(md) = self.wiki.read_doc(&slug).await? {
                let bot_slug = format!("bot/{name}");
                let mut spec = parse_bot_doc(&bot_slug, &md);
                // Уроки — соседний док персональной папки бота (#1424):
                // Ok(None) — дока нет, носитель честно пуст; Err — читать
                // не удалось: None (последние известные уроки сохраняются,
                // ложного отзыва нет); warn и бот не ломается.
                let lessons_slug = format!("bot/{name}/Уроки");
                match self.wiki.read_doc(&lessons_slug).await {
                    Ok(Some(raw)) => {
                        if let Ok(s) = spec.as_mut() {
                            s.lessons = Some(normalize_lessons(&raw));
                        }
                    }
                    Ok(None) => {
                        if let Ok(s) = spec.as_mut() {
                            s.lessons = Some(String::new());
                        }
                    }
                    Err(e) => tracing::warn!(event = "bot.lessons_read_failed", slug = %bot_slug,
                        error = %e, "Док уроков не дочитан — держим последние известные уроки"),
                }
                out.push((bot_slug, spec));
            }
        }
        Ok(out)
    }

    /// Снос/переименование дока или enabled:false → задача гасится,
    /// сессия в opencode.db сохраняется (память переживает погашение).
    fn apply_stops(
        &self,
        running: &mut HashMap<String, Running>,
        scanned: &[(String, Result<BotSpec, String>)],
    ) {
        let mut gone: Vec<(String, &'static str)> = Vec::new();
        for slug in running.keys() {
            let parsed = scanned
                .iter()
                .find(|(s, _)| s.as_str() == slug)
                .map(|(_, spec)| spec);
            if let Some(reason) = stop_reason(parsed) {
                gone.push((slug.clone(), reason));
            }
        }
        for (slug, reason) in gone {
            if let Some(entry) = running.remove(&slug) {
                entry.task.abort();
                tracing::info!(event = "bot.stopped", slug = %slug, reason,
                    "Бот погашен — сессия сохранена");
            }
        }
    }

    /// Мёртвая задача → рестарт с бэкоффом (запись висит до спавна).
    fn revive_dead(
        &self,
        running: &mut HashMap<String, Running>,
        scanned: &[(String, Result<BotSpec, String>)],
    ) {
        let dead: Vec<String> = running
            .iter()
            .filter(|(_, r)| r.task.is_finished())
            .map(|(slug, _)| slug.clone())
            .collect();
        let now = self.clock.now_ms();
        for slug in dead {
            let Some(spec) = scanned.iter().find_map(|(_s, r)| {
                r.as_ref()
                    .ok()
                    .filter(|b| b.slug == slug && b.enabled)
                    .cloned()
            }) else {
                running.remove(&slug);
                continue;
            };
            // Последние известные уроки — до мутабельного заимствования
            // entry (resolve_lessons читает running иммутабельно).
            let lessons = self.resolve_lessons(running, &slug, &spec);
            let entry = running.get_mut(&slug).expect("dead entry");
            let alive_ms = now.saturating_sub(entry.started_ms);
            if !entry.dead_seen {
                entry.dead_seen = true;
                entry.fails = if alive_ms > self.guard.healthy_ms {
                    1
                } else {
                    entry.fails + 1
                };
            }
            let shift = (entry.fails - 1).min(10);
            let delay = std::cmp::min(
                self.guard.backoff_base_ms << shift,
                self.guard.backoff_cap_ms,
            );
            if alive_ms < delay {
                tracing::info!(event = "bot.backoff_wait", slug = %slug,
                    fails = entry.fails, delay_ms = delay, "Рестарт отложен бэкоффом");
                continue;
            }
            tracing::info!(event = "bot.restarted", slug = %slug, fails = entry.fails,
                "Задача умерла — рестарт");
            let live = Arc::new(live_of(&spec, &lessons));
            let Some(task) = self.spawn_task(&spec, live.clone()) else {
                // токен пропал из env: запись гасим, warn даст следующий тик
                running.remove(&slug);
                continue;
            };
            *entry = Running {
                spec,
                live,
                task,
                started_ms: now,
                fails: 0,
                dead_seen: false,
            };
        }
    }

    /// Диф желаемого/запущенного: spawn новых, stop/rearm/recreate по
    /// ярусам fm-полей, hot-обновление метаданных/методички.
    fn apply_diffs(
        &self,
        running: &mut HashMap<String, Running>,
        scanned: &[(String, Result<BotSpec, String>)],
    ) {
        for (slug, parsed) in scanned {
            let Ok(spec) = parsed else { continue };
            // Последние известные уроки — до мутабельного заимствования
            // записи (resolve_lessons читает running иммутабельно).
            let lessons = self.resolve_lessons(running, slug, spec);
            match running.get_mut(slug) {
                None => {
                    if spec.enabled {
                        self.spawn_new(running, spec.clone());
                    }
                }
                Some(entry) => {
                    let change = entry.live.update(
                        spec.owner.as_deref(),
                        spec.greeting.as_deref(),
                        &compose_manual(&spec.manual, &lessons),
                        &lessons,
                    );
                    // методичка доезжает только в doc-режиме (agent-режим
                    // несёт её в agents/<name>.md воркспейса) — смена
                    // композита от уроков или тела дока в agent-режиме
                    // мимо сессии, лог не врёт (ревью Low 2)
                    if change.manual && spec.agent.is_none() {
                        tracing::info!(event = "bot.manual_updated", slug = %slug,
                            "Методичка обновлена — доедет с ближайшим сообщением");
                    }
                    if change.lessons {
                        tracing::info!(event = "bot.lessons_updated", slug = %slug,
                            "Уроки обновлены — доедут с ближайшим ходом (doc: композит, agent: префикс/отзыв)");
                    }
                    match plan_action(&entry.spec, spec) {
                        // Stop здесь недостижим: apply_stops в этом же тике
                        // раньше гасит Err/!enabled/отсутствующие записи
                        // (сейф-нет на смену порядка тика, не мёртвый лог).
                        Some(BotAction::Stop) => {}
                        Some(BotAction::RecreateSession) => {
                            entry.task.abort();
                            self.state.set_bot_session(slug, None);
                            // Методичка вшита в старую сессию (doc-режим) —
                            // свежая её не получит: сброс hash = дозаправка
                            // первым же промптом (#1221, тот же инвариант,
                            // что у компакта). Уроки (agent-режим) ехали
                            // префиксом в историю старой сессии — сброс
                            // lessons-hash дозаправит их тоже (#1424).
                            self.state.set_bot_manual_hash(slug, None);
                            self.state.set_bot_lessons_hash(slug, None);
                            if let Err(e) = self.state.save() {
                                tracing::warn!(event = "bot.state_save_failed", slug = %slug, error = %e,
                                    "Сброс сессии не персистится — рестарт реанимирует старую сессию без новой model/workspace");
                            }
                            tracing::info!(event = "bot.session_recreated", slug = %slug,
                                reason = "structural",
                                "Смена model/workspace — сессия пересоздаётся, память сброшена");
                            self.replace_entry(running, slug, spec);
                        }
                        Some(BotAction::ReArm) => {
                            entry.task.abort();
                            tracing::info!(event = "bot.rearmed", slug = %slug,
                                "Смена token_env — задача перезапускается, сессия живёт");
                            self.replace_entry(running, slug, spec);
                        }
                        None => entry.spec = spec.clone(),
                    }
                }
            }
        }
    }

    /// Пересоздать запись рантайма на свежей спеке (rearm/recreate).
    /// Токен пропал — запись гасится, warn даст следующий тик.
    fn replace_entry(&self, running: &mut HashMap<String, Running>, slug: &str, spec: &BotSpec) {
        let lessons = self.resolve_lessons(running, slug, spec);
        let live = Arc::new(live_of(spec, &lessons));
        let Some(task) = self.spawn_task(spec, live.clone()) else {
            running.remove(slug);
            return;
        };
        running.insert(
            slug.to_string(),
            Running {
                spec: spec.clone(),
                live,
                task,
                started_ms: self.clock.now_ms(),
                fails: 0,
                dead_seen: false,
            },
        );
    }

    fn spawn_new(&self, running: &mut HashMap<String, Running>, spec: BotSpec) {
        let slug = spec.slug.clone();
        let lessons = self.resolve_lessons(running, &slug, &spec);
        let live = Arc::new(live_of(&spec, &lessons));
        let Some(task) = self.spawn_task(&spec, live.clone()) else {
            return; // warn уже в логе (bot.token_missing)
        };
        running.insert(
            slug.clone(),
            Running {
                spec,
                live,
                task,
                started_ms: self.clock.now_ms(),
                fails: 0,
                dead_seen: false,
            },
        );
        tracing::info!(event = "bot.spawned", slug = %slug, "Бот поднят");
    }

    /// Токен из env по имени из дока; нет — бот не поднимается (warn
    /// раз на тик: деплой-долг виден в логе).
    fn spawn_task(
        &self,
        spec: &BotSpec,
        live: Arc<BotLive>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let Some(token) = (self.token_of)(&spec.token_env) else {
            tracing::warn!(event = "bot.token_missing", slug = %spec.slug,
                env = %spec.token_env, "Токена нет в env — бот-задача не поднимается");
            return None;
        };
        let deps = super::runner::BotDeps {
            slug: spec.slug.clone(),
            structural: BotStructural {
                title: spec.title.clone(),
                model: spec.model.clone(),
                workspace: spec.workspace.clone(),
                agent: spec.agent.clone(),
            },
            live,
            tg: (self.make_tg)(&token),
            sessions: self.sessions.clone(),
            state: self.state.clone(),
            assets: self.assets.clone(),
            tick_state: self.tick_state.clone(),
            reply_cap_ms: self.reply_cap_ms,
            reply_poll_ms: self.reply_poll_ms,
            retry_pause_ms: RETRY_PAUSE_MS,
        };
        Some(spawn_bot_task(deps))
    }
}
