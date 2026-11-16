//! Супервизор ботов: тик = скан `Projects/*/Боты/*.md` → диф с запущенными
//! → spawn / stop / recreate-session / re-arm / hot. Мёртвая задача —
//! рестарт с экспоненциальным бэкоффом (потолок ~30 мин, сброс после
//! ~10 мин здоровой работы). Тик быстрый (wiki-скан) — живёт обычным
//! сайдкаром; long-poll — внутри задач раннеров, тик-луп не блокирует.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uwuwu_domain::{BotAction, BotSpec};
use uwuwu_usecase::bots::{parse_bot_doc, plan_action, BotStateStore, TelegramGateway};
use uwuwu_usecase::{AgentSessionGateway, Clock, TickStateStore, UsecaseResult, WikiRepo};

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

pub struct BotSupervisor {
    wiki: Arc<dyn WikiRepo>,
    /// Raw-гейтвей: бот-спавны вне спавн-бюджета (create редкий — один на
    /// жизнь сессии; шторм гасит собственный луп-гвард).
    sessions: Arc<dyn AgentSessionGateway>,
    state: Arc<dyn BotStateStore>,
    /// dreamer_done-маркировка бот-сессий (дример их не подбирает).
    tick_state: Arc<dyn TickStateStore>,
    clock: Arc<dyn Clock>,
    make_tg: TgFactory,
    pub(crate) token_of: TokenLookup,
    /// Потолок/шаг ожидания реплая (поля ради тестов).
    pub(crate) reply_cap_ms: u64,
    pub(crate) reply_poll_ms: u64,
    pub(crate) guard: GuardTiming,
    running: Mutex<HashMap<String, Running>>,
}

impl BotSupervisor {
    pub fn new(
        wiki: Arc<dyn WikiRepo>,
        sessions: Arc<dyn AgentSessionGateway>,
        state: Arc<dyn BotStateStore>,
        tick_state: Arc<dyn TickStateStore>,
        clock: Arc<dyn Clock>,
        make_tg: TgFactory,
    ) -> Self {
        Self {
            wiki,
            sessions,
            state,
            tick_state,
            clock,
            make_tg,
            token_of: Arc::new(|env| std::env::var(env).ok().filter(|v| !v.trim().is_empty())),
            // 3600с (владелец 16.11: «15 мало — делай час»): тяжёлые
            // методички/длинные генерации; полл 2с ловит ответ раньше
            reply_cap_ms: 3_600_000,
            reply_poll_ms: 2_000,
            guard: GuardTiming::default(),
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
        let mut running = self.running.lock().expect("bots lock");
        self.apply_stops(&mut running, &scanned);
        self.revive_dead(&mut running, &scanned);
        self.apply_diffs(&mut running, &scanned);
        Ok(())
    }

    /// Скан папки Боты: один list_docs, глобально — бот не требует
    /// конфигурации [[project]] сервера.
    async fn scan(&self) -> UsecaseResult<Vec<(String, Result<BotSpec, String>)>> {
        let mut out = Vec::new();
        for slug in self.wiki.list_docs("Projects").await? {
            let segs: Vec<&str> = slug.split('/').collect();
            if segs.len() != 4 || segs[2] != "Боты" {
                continue;
            }
            if let Some(md) = self.wiki.read_doc(&slug).await? {
                let spec = parse_bot_doc(&slug, &md);
                out.push((slug, spec));
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
            let live = Arc::new(BotLive::new(
                spec.owner.clone(),
                spec.greeting.clone(),
                spec.manual.clone(),
            ));
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
            match running.get_mut(slug) {
                None => {
                    if spec.enabled {
                        self.spawn_new(running, spec.clone());
                    }
                }
                Some(entry) => {
                    if entry.live.update(
                        spec.owner.as_deref(),
                        spec.greeting.as_deref(),
                        &spec.manual,
                    ) {
                        tracing::info!(event = "bot.manual_updated", slug = %slug,
                            "Методичка обновлена — доедет с ближайшим сообщением");
                    }
                    match plan_action(&entry.spec, spec) {
                        // Stop здесь недостижим: apply_stops в этом же тике
                        // раньше гасит Err/!enabled/отсутствующие записи
                        // (сейф-нет на смену порядка тика, не мёртвый лог).
                        Some(BotAction::Stop) => {}
                        Some(BotAction::RecreateSession) => {
                            entry.task.abort();
                            self.state.set_bot_session(slug, None);
                            let _ = self.state.save();
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
        let live = Arc::new(BotLive::new(
            spec.owner.clone(),
            spec.greeting.clone(),
            spec.manual.clone(),
        ));
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
        let live = Arc::new(BotLive::new(
            spec.owner.clone(),
            spec.greeting.clone(),
            spec.manual.clone(),
        ));
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
            tick_state: self.tick_state.clone(),
            reply_cap_ms: self.reply_cap_ms,
            reply_poll_ms: self.reply_poll_ms,
            retry_pause_ms: RETRY_PAUSE_MS,
        };
        Some(spawn_bot_task(deps))
    }
}
