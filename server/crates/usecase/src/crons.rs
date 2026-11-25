//! Кроны на вики-носителе (#1058, раскладка /cron — #1214): мастер =
//! docs/cron/<имя>/README.md (fm: schedule + agent + project), учёт ранов —
//! машинный, в секции `crons` state.json (#1480, снос журнала): last_fire —
//! источник is_due, незавершённый ран — детект тишины UC-02, отложение —
//! детект пропуска файра, fails — счётчик отказов (канон ADR-103
//! «runtime-факты — в state»; журнальный носитель снесён как мёртвый ящик —
//! решение владельца 21.11 #1354). Домен — uwuwu_domain::cron
//! (Schedule/is_due); здесь грамматика строк носителя (parse_schedule_text)
//! и IO.

use crate::error::UsecaseResult;
use crate::naming;
use crate::pipeline::incident::{IncidentLoudness, INCIDENT_COOLDOWN_MS};
use crate::ports::{Clock, CronRow, CronRunTrack, CronStateStore, SessionSpec, WikiRepo};
use crate::router::SessionRouter;
use crate::spawn_models::SpawnModels;
use std::sync::Arc;
use uwuwu_domain::{
    civil_from_days, AgentSlot, CronField, CronMaster, CronSchedule, IntervalMinutes, JstDate,
    JstTime, Schedule, ScheduleKind,
};

/// Верхний уровень конфигурации кронов (#1214): дети — подпапки кронов,
/// мастер — `cron/<имя>/README.md`, ассеты — рядом.
pub const CRON_DIR: &str = "cron";

/// Порог тишины рана (UC-02/ADR-103, паритет ADR-073): файр состоялся,
/// завершения нет дольше порога — счётчик отказов + ТГ владельцу. Замена
/// прежнего 2-часового журнального warn (#1480).
pub const RUN_SILENCE_MS: u64 = 6 * 3_600_000;

/// Анти-повтор кроновых сигналов: один ТГ на эпизод сбоя (IncidentLoudness
/// с порогом 1 + cooldown; ин-мемори — рестарт = свежее окно, философия
/// инцидент-контура #1281).
pub const FIRE_SIGNAL_COOLDOWN_MS: u64 = INCIDENT_COOLDOWN_MS;

pub struct CronUseCase {
    pub wiki: Arc<dyn WikiRepo>,
    /// Роутер «проект → executor»: крон-ран — гейтвей своего проекта.
    pub sessions: Arc<SessionRouter>,
    pub clock: Arc<dyn Clock>,
    pub models: Arc<SpawnModels>,
    /// Карта «project → checkout» из [[project]] конфига: крон связан
    /// с проектом fm-полем `project` (плоская раскладка /cron, #1214).
    pub projects: std::collections::HashMap<String, String>,
    /// Единый кап активных ходов исполнителя (policy.cup): файр при
    /// полном капе откладывается до свободного тика — state-запись файра
    /// не пишется, ран не считается состоявшимся (#1231).
    pub cup: usize,
    /// Машинный учёт ранов (секция `crons` state.json, #1480) — замена
    /// журнального носителя last_run.
    pub state: Arc<dyn CronStateStore>,
    /// ТГ-сигнализация сбоев («исключения демона — лог + ТГ», UC-02):
    /// None — громкий лог. MCP-инстанс не сигналит.
    pub notify: Option<(Arc<dyn crate::bots::TelegramGateway>, i64)>,
    /// Анти-повтор сигналов (сбои кронов долетают раз за cooldown).
    loudness: std::sync::Mutex<IncidentLoudness>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FireOutcome {
    pub slug: String,
    pub session: String,
}

/// Факт файра с якорем первой встречи (#1517): max(last_fire, anchor).
/// Once живёт на голом last_fire — его None доживает до планового
/// момента (якорь в формулу Once не входит).
fn effective_last(row: &CronRow, once_kind: bool) -> Option<u64> {
    if once_kind {
        return row.last_fire_ms;
    }
    match (row.last_fire_ms, row.anchor_ms) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

impl CronUseCase {
    /// Каноническая сборка: анти-повтор сигналов — IncidentLoudness
    /// «порог 1 + cooldown» (один ТГ на эпизод сбоя крона, ин-мемори —
    /// рестарт = свежее окно, философия #1281).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        wiki: Arc<dyn WikiRepo>,
        sessions: Arc<SessionRouter>,
        clock: Arc<dyn Clock>,
        models: Arc<SpawnModels>,
        projects: std::collections::HashMap<String, String>,
        cup: usize,
        state: Arc<dyn CronStateStore>,
        notify: Option<(Arc<dyn crate::bots::TelegramGateway>, i64)>,
    ) -> Self {
        Self {
            wiki,
            sessions,
            clock,
            models,
            projects,
            cup,
            state,
            notify,
            loudness: std::sync::Mutex::new(IncidentLoudness::new(
                1,
                FIRE_SIGNAL_COOLDOWN_MS,
                FIRE_SIGNAL_COOLDOWN_MS,
            )),
        }
    }

    /// Инвентарь мастеров; Some(project) — фильтр по fm `project`.
    pub async fn masters(&self, project: Option<&str>) -> UsecaseResult<Vec<CronMaster>> {
        let all = self.scan_masters().await?;
        Ok(all
            .into_iter()
            .filter(|m| match project {
                Some(p) => m.project == p,
                None => true,
            })
            .collect())
    }

    /// Один проход: мастера, завершение/тишина/пропуск ранов, due →
    /// state-запись файра + спавн рана. Проект рана — fm `project`
    /// мастера (роутинг executor + checkout).
    pub async fn tick(&self) -> UsecaseResult<Vec<FireOutcome>> {
        let mut fired = Vec::new();
        // CUP-гвард (#1231): крон-ран — запуск хода, ему нужен свободный
        // слот единого капа. Деградация по исполнителю (#1450): busy —
        // по живому union; полный отказ всех исполнителей — файр
        // откладывается (ран не считается состоявшимся, state-запись
        // файра не пишется). busy — счётчик на весь тик: файр текущего
        // тика занимает слот (инкремент после успешного запуска), иначе
        // мульти-файр при неполном капе оверселлит до cap+N-1
        // (#1244-класс, ревью #1231).
        let (list, failed) = self.sessions.list_sessions_reachable().await;
        if failed.len() == self.sessions.executors_total() {
            tracing::warn!(
                event = "cron.list_sessions_failed",
                executors = ?failed,
                "Кроны: ни один исполнитель недоступен — файр отложен"
            );
            return Ok(fired);
        }
        let mut busy = crate::router::busy_count(&list);
        let masters = self.scan_masters().await?;
        for master in masters {
            let name = cron_name(&master.slug).to_string();
            let mut row = self.state.cron_row(&name);
            // 1. Завершение рана: факт снимается с сессии исполнителя
            // (#1480 — замена журнальной строки-закрытия).
            self.settle_run(&master.slug, &mut row, &list, &failed)
                .await;
            let now = self.clock.now_ms();
            // 2. Тишина рана (UC-02): файр был, завершения нет > порога.
            if let Some(run) = row.run.take() {
                if now.saturating_sub(run.fired_ms) > RUN_SILENCE_MS {
                    row.fails += 1;
                    self.signal(&master.slug, "ран не завершился дольше 6 ч (тишина рана)")
                        .await;
                } else {
                    row.run = Some(run);
                }
            }
            // Якорь первой встречи (#1517): факта файра и якоря нет —
            // якорим и не файрим этот тик. None не файрит: крон ждёт
            // ближайшей плановой минуты — потеря state (битый файл/сброс
            // тома) больше не даёт масс-догона прошлых минут. Once —
            // исключение: его None должен дожить до планового момента.
            let once_kind = matches!(master.schedule.kind(), ScheduleKind::Once { .. });
            let effective = effective_last(&row, once_kind);
            if effective.is_none() && !once_kind {
                row.anchor_ms = Some(now);
                self.state.set_cron_row(&name, row);
                continue;
            }
            let due = master.schedule.is_due(now, effective);
            // 3. Пропущенный файр: due держится, файр отложен > порога.
            if due {
                match row.deferred_since_ms {
                    Some(since) if now.saturating_sub(since) > RUN_SILENCE_MS => {
                        row.fails += 1;
                        row.deferred_since_ms = None;
                        self.signal(
                            &master.slug,
                            "файр по расписанию не состоялся дольше 6 ч (отложение)",
                        )
                        .await;
                    }
                    None => row.deferred_since_ms = Some(now),
                    _ => {}
                }
            } else {
                row.deferred_since_ms = None;
            }
            if !due {
                self.state.set_cron_row(&name, row);
                continue;
            }
            // 4. CUP-гейт: полный кап — файр откладывается, state-запись
            // файра не пишется (ран не состоялся), метка отложения остаётся.
            if busy >= self.cup {
                tracing::info!(
                    event = "cron.held_cup",
                    cron = %master.slug,
                    active = busy,
                    cap = self.cup,
                    "Крон-файр отложен: CUP cap — ран не считается состоявшимся"
                );
                self.state.set_cron_row(&name, row);
                continue;
            }
            // 5. project вне карты конфига — skip+warn, файр не пишется.
            let Some(checkout) = self.projects.get(&master.project) else {
                tracing::warn!(event = "cron.project_unknown", cron = %master.slug,
                    project = %master.project,
                    "fm project вне карты [[project]] — крон не файрится (skip+warn)");
                self.state.set_cron_row(&name, row);
                continue;
            };
            let outcome = self
                .fire_run(&master, &name, &mut row, checkout, now)
                .await?;
            busy += 1; // файр этого тика занимает слот капа
            if let Some(outcome) = outcome {
                fired.push(outcome);
            }
        }
        self.state.save()?;
        Ok(fired)
    }

    /// Завершение рана по живому union-скану сессий (#1480). Трек без
    /// сессии (краш между записью и спавном) не сеттлится — тишина-детект
    /// закроет его по порогу. Отказные исполнители — слепой hold: их сессии
    /// невидимы скану, ложной тревоги нет (семантика #1263).
    async fn settle_run(
        &self,
        slug: &str,
        row: &mut CronRow,
        list: &[crate::ports::SessionInfo],
        failed: &[String],
    ) {
        let Some(track) = row.run.clone() else {
            return;
        };
        if track.session.is_empty() {
            return;
        }
        if !failed.is_empty() {
            return; // частичная деградация — суждение откладывается
        }
        let Some(info) = list.iter().find(|s| s.id == track.session) else {
            // Все исполнители живы, сессии нет — ран погиб (снесён/потерян).
            row.run = None;
            row.fails += 1;
            self.signal(slug, "сессия рана исчезла у исполнителя").await;
            return;
        };
        if info.busy || info.outcome.is_empty() {
            return; // ход ещё в полёте либо финала нет
        }
        row.run = None;
        if info.outcome.eq_ignore_ascii_case("failed") {
            row.fails += 1;
            self.signal(slug, "ран завершился ошибкой исполнителя")
                .await;
        }
    }

    /// Крон-сбой владельцу: анти-повтор (раз за cooldown на все кроны),
    /// [notify] не задан — громкий лог (канон UC-02/ADR-103).
    async fn signal(&self, slug: &str, reason: &str) {
        tracing::warn!(event = "cron.signal", cron = %slug, %reason,
            "Крон-сбой — сигнал владельцу");
        let Some((tg, chat)) = &self.notify else {
            tracing::warn!(event = "cron.signal_no_notify", cron = %slug,
                "[notify] не настроен — сигнала нет (лог остаётся)");
            return;
        };
        let fire = {
            let mut l = self.loudness.lock().expect("loudness lock");
            l.register(self.clock.now_ms())
        };
        if !fire {
            return;
        }
        let text = format!("⚠︎ uwuwu: крон-сбой — {slug}: {reason}.");
        if let Err(e) = tg.send_message(*chat, &text).await {
            tracing::error!(event = "cron.signal_send_failed", cron = %slug, error = %e,
                "ТГ-сигнал крон-сбоя не доставлен");
        }
    }

    /// Файр одного мастера: state-запись до спавна (crash-семантика
    /// прежней журнальной строки: краш между записью и спавном оставляет
    /// трек без сессии — тишина-детект поднимет сигнал по порогу),
    /// спавн рана на его проект-исполнителе. None — спавн не удался
    /// (create/prompt Err): last_fire остаётся (слот расписания
    /// израсходован), run-трек сбрасывается, сигнал — сразу; остальные
    /// мастера тика продолжаются.
    async fn fire_run(
        &self,
        master: &CronMaster,
        name: &str,
        row: &mut CronRow,
        checkout: &str,
        now: u64,
    ) -> UsecaseResult<Option<FireOutcome>> {
        let slug = master.slug.clone();
        let prev_fire = row.last_fire_ms;
        row.last_fire_ms = Some(now);
        row.deferred_since_ms = None;
        row.run = Some(CronRunTrack {
            session: String::new(),
            fired_ms: now,
        });
        self.state.set_cron_row(name, row.clone());
        self.state.save()?;
        let prompt = run_prompt(&master.project, master, prev_fire);
        let sm = match master.agent {
            AgentSlot::Role(agent) => self.models.for_agent(agent),
            AgentSlot::Default => self.models.for_default(),
        };
        let spec = SessionSpec {
            custom_agent: None,
            directory: checkout.to_string(),
            title: format!("⏰ {} · {}", master.title, jst_stamp(now)),
            agent: master.agent,
            model: sm.model,
            variant: sm.variant,
            bootstrap: false,
        };
        let sessions = self.sessions.project(&master.project);
        let session = match sessions.create_session(&spec).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(event = "cron.fire_failed", cron = %slug,
                    project = %master.project, error = %e,
                    "Спавн крон-рана не удался — остальные мастера тика продолжаются");
                row.run = None;
                row.fails += 1;
                self.state.set_cron_row(name, row.clone());
                self.state.save()?;
                self.signal(&slug, "спавн крон-рана не удался").await;
                return Ok(None);
            }
        };
        if let Err(e) = sessions.prompt(&session, &prompt).await {
            tracing::warn!(event = "cron.fire_failed", cron = %slug,
                session = %session, error = %e,
                "Промпт крон-рана не доставлен — остальные мастера тика продолжаются");
            row.run = None;
            row.fails += 1;
            self.state.set_cron_row(name, row.clone());
            self.state.save()?;
            self.signal(&slug, "промпт крон-рана не доставлен").await;
            return Ok(None);
        }
        row.run = Some(CronRunTrack {
            session,
            fired_ms: now,
        });
        self.state.set_cron_row(name, row.clone());
        self.state.save()?;
        tracing::info!(event = "cron.fired", cron = %slug, "Cron fired");
        let session = row
            .run
            .as_ref()
            .map(|r| r.session.clone())
            .unwrap_or_default();
        Ok(Some(FireOutcome { slug, session }))
    }

    /// Мастера: `cron/<имя>/README.md` (имя без слэша). Прочие пути
    /// папки (журналы, файлы вне README) — вне инвентаря; битый fm —
    /// warn раз на тик (молчаливый пропуск прячет опечатку владельца).
    async fn scan_masters(&self) -> UsecaseResult<Vec<CronMaster>> {
        let mut out = Vec::new();
        for slug in self.master_slugs().await? {
            let Some(md) = self.wiki.read_doc(&slug).await? else {
                continue;
            };
            match parse_master(&slug, &md) {
                Some(m) => out.push(m),
                None => tracing::warn!(event = "cron.master_invalid", slug = %slug,
                    "Мастер не парсится (fm project/schedule) — крон не файрится"),
            }
        }
        Ok(out)
    }

    /// Слаги мастеров: `cron/<имя>/README`, имя без вложенных сегментов.
    async fn master_slugs(&self) -> UsecaseResult<Vec<String>> {
        let mut out = Vec::new();
        for slug in self.wiki.list_docs(CRON_DIR).await? {
            let Some(rest) = slug.strip_prefix(&format!("{CRON_DIR}/")) else {
                continue;
            };
            let Some(name) = rest.strip_suffix("/README") else {
                continue;
            };
            if !name.is_empty() && !name.contains('/') {
                out.push(slug);
            }
        }
        Ok(out)
    }
}

/// Грамматика расписаний (строки мастера → доменные value-типы):
/// `5m|2h|1d` — интервал; `HH:MM` — ежедневно (JST); `HH:MM DD.MM` — разово;
/// `m h dom mon dow` — cron-expr (время JST). None — не расписание.
pub fn parse_schedule_text(raw: &str) -> Option<Schedule> {
    let s = raw.trim();
    let kind = if let Some(cron) = parse_cron_expr(s) {
        ScheduleKind::Cron(cron)
    } else if let Some((num, mult)) = parse_interval(s) {
        ScheduleKind::Interval(IntervalMinutes::new(num.max(1) * mult)?)
    } else if let Some((at, on)) = parse_daily_once(s) {
        match on {
            None => ScheduleKind::Daily(at),
            Some(on) => ScheduleKind::Once { at, on },
        }
    } else {
        return None;
    };
    Some(Schedule::new(kind))
}

fn parse_interval(s: &str) -> Option<(u64, u64)> {
    for (suffix, mult) in [('m', 1u64), ('h', 60), ('d', 1440)] {
        if let Some(num) = s.strip_suffix(suffix).and_then(|n| n.parse::<u64>().ok()) {
            return Some((num, mult));
        }
    }
    None
}

fn parse_daily_once(s: &str) -> Option<(JstTime, Option<JstDate>)> {
    let mut parts = s.split_whitespace();
    let time = parts.next()?;
    let day = parts.next();
    let (hh, mm) = time.split_once(':')?;
    let at = JstTime::new(hh.parse().ok()?, mm.parse().ok()?)?;
    match day {
        None => Some((at, None)),
        Some(d) => {
            let (dd, mo) = d.split_once('.')?;
            Some((at, Some(JstDate::new(dd.parse().ok()?, mo.parse().ok()?)?)))
        }
    }
}

fn parse_cron_field(raw: &str, max: u32) -> Option<CronField> {
    let raw = raw.trim();
    if raw == "*" {
        return Some(CronField::Any);
    }
    let mut out = Vec::new();
    for part in raw.split(',') {
        if let Some((a, b)) = part.split_once('-') {
            let (a, b): (u32, u32) = (a.parse().ok()?, b.parse().ok()?);
            if a > b || b > max {
                return None;
            }
            out.extend(a..=b);
        } else {
            let n: u32 = part.parse().ok()?;
            if n > max {
                return None;
            }
            out.push(n);
        }
    }
    (!out.is_empty()).then(|| CronField::list(out))
}

/// 5 полей cron-expr → CronSchedule (min hour dom month dow; JST).
fn parse_cron_expr(raw: &str) -> Option<CronSchedule> {
    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }
    Some(CronSchedule::new(
        parse_cron_field(parts[0], 59)?,
        parse_cron_field(parts[1], 23)?,
        parse_cron_field(parts[2], 31)?,
        parse_cron_field(parts[3], 12)?,
        parse_cron_field(parts[4], 7)?,
    ))
}

/// Имя крона из слага мастера `cron/<имя>/README` — средний сегмент
/// (не хвост: слаг кончается README). Публично для cron_list (#1480).
pub fn cron_name(slug: &str) -> &str {
    slug.strip_prefix(CRON_DIR)
        .and_then(|r| r.strip_prefix('/'))
        .and_then(|r| r.strip_suffix("/README"))
        .filter(|n| !n.is_empty() && !n.contains('/'))
        .unwrap_or_else(|| slug.rsplit('/').next().unwrap_or(slug))
}

/// Мастер: fm-поля title/schedule/agent/project + тело; schedule и
/// project обязательны (project связывает крон с [[project]] конфига).
pub fn parse_master(slug: &str, md: &str) -> Option<CronMaster> {
    let fm = crate::fm_lite::parse(md);
    let schedule = parse_schedule_text(fm.get("schedule")?)?;
    let project = fm.get("project")?.trim();
    if project.is_empty() {
        return None;
    }
    Some(CronMaster {
        slug: slug.to_string(),
        title: fm.get("title").cloned().unwrap_or_else(|| slug.to_string()),
        schedule,
        agent: AgentSlot::of(fm.get("agent").and_then(|a| naming::parse_agent(a))),
        project: project.to_string(),
        body: crate::fm_lite::strip(md).to_string(),
    })
}

/// JST-штамп учёта: ISO с зоной (+09:00 — читается обратно). Публично
/// для cron_list (#1480) и fm-гварда docs_write (#1444 — единый формат
/// датировок корпуса).
pub fn jst_stamp(ms: u64) -> String {
    let secs = ms / 1000 + 9 * 3600;
    let d = secs / 86400;
    let (y, mo, da) = civil_from_days(d as i64);
    let rem = secs % 86400;
    format!(
        "{y:04}-{mo:02}-{da:02}T{:02}:{:02}:00+09:00",
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// Промпт рана: тело мастера (декларация) + машинные факты учёта
/// (#1480). Журнала нет — окна аудита отсчитываются от штампа
/// последнего файра, переданного демоном.
fn run_prompt(project: &str, master: &CronMaster, last_fire_ms: Option<u64>) -> String {
    let last = match last_fire_ms {
        Some(ms) => format!("{} (JST)", jst_stamp(ms)),
        None => "none recorded (first run)".to_string(),
    };
    format!(
        "Cron run «{title}» (project {project}).\n\n\
         ## Master instruction\n\n{body}\n\n\
         ## Machine accounting\n\n\
         Last fire: {last}. Run autonomously.",
        title = master.title,
        body = master.body,
        last = last,
    )
}

/// Тестовый in-memory CronStateStore (тесты crons и spawn_model_tests):
/// файр пишет до спавна — счётчик save для ассертов crash-семантики.
#[cfg(test)]
pub(crate) struct MemCronState {
    rows: std::sync::Mutex<std::collections::HashMap<String, CronRow>>,
    saves: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl MemCronState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            rows: std::sync::Mutex::default(),
            saves: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    pub(crate) fn row(&self, name: &str) -> CronRow {
        self.rows
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn saves(&self) -> usize {
        self.saves.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
impl CronStateStore for MemCronState {
    fn cron_row(&self, name: &str) -> CronRow {
        self.row(name)
    }
    fn set_cron_row(&self, name: &str, row: CronRow) {
        self.rows.lock().unwrap().insert(name.to_string(), row);
    }
    fn save(&self) -> UsecaseResult<()> {
        self.saves.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::WikiRepo;

    use std::sync::{Arc, Mutex};

    /// Гейтвей отказного исполнителя: list_sessions всегда Err.
    struct FailingSessions;
    #[async_trait::async_trait]
    impl crate::ports::AgentSessionGateway for FailingSessions {
        async fn create_session(&self, _: &crate::ports::SessionSpec) -> UsecaseResult<String> {
            Ok("ses_x".into())
        }
        async fn prompt(&self, _: &str, _: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn run_command(&self, _: &str, _: &str, _: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn liveness(&self, _: &str) -> crate::ports::Liveness {
            crate::ports::Liveness::Unknown
        }
        async fn delete_session(&self, _: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn last_activity_ms(&self, _: &str) -> UsecaseResult<Option<u64>> {
            Ok(None)
        }
        async fn last_assistant_reply(
            &self,
            _: &str,
        ) -> UsecaseResult<Option<crate::ports::AssistantReply>> {
            Ok(None)
        }
        async fn list_sessions(&self) -> UsecaseResult<Vec<crate::ports::SessionInfo>> {
            Err(crate::error::UsecaseError::Session(
                "исполнитель недоступен".into(),
            ))
        }
        async fn export_session(&self, _: &str) -> UsecaseResult<String> {
            Ok(String::new())
        }
    }

    #[test]
    fn master_requires_valid_schedule_and_project() {
        let md = "---\ntitle: Тест\nschedule: 5m\nproject: uwuwu\n---\n\nТело";
        let m = parse_master("cron/x/README", md).unwrap();
        assert_eq!(m.title, "Тест");
        assert_eq!(m.project, "uwuwu");
        assert_eq!(m.agent, AgentSlot::Default);

        let no_project = "---\ntitle: Тест\nschedule: 5m\n---\n\nТело";
        assert!(
            parse_master("cron/x/README", no_project).is_none(),
            "нет fm project — не мастер"
        );
        // fm_lite — голые значения (без YAML-кавычек): quoted-мусор
        // пройдёт парс и отсеется в тике (project вне карты — skip+warn)
        let empty_project = "---\nschedule: 5m\nproject:   \n---\n\nТело";
        assert!(
            parse_master("cron/x/README", empty_project).is_none(),
            "пустой project — не мастер"
        );

        let bad = "---\ntitle: Битый\nschedule: не-расписание\nproject: uwuwu\n---\n\nТело";
        assert!(
            parse_master("x", bad).is_none(),
            "битое расписание — не мастер"
        );

        let nofm = "просто текст без fm";
        assert!(parse_master("x", nofm).is_none());
    }

    #[test]
    fn master_agent_parsed() {
        let md = "---\nschedule: 1h\nagent: dreamer\nproject: uwuwu\n---\n";
        let m = parse_master("cron/x/README", md).unwrap();
        assert_eq!(m.agent, AgentSlot::Role(uwuwu_domain::Agent::Dreamer));
    }

    #[test]
    fn run_prompt_carries_last_fire_and_no_journal() {
        let md = "---\ntitle: Тест\nschedule: 5m\nproject: uwuwu\n---\nТело";
        let m = parse_master("cron/тест/README", md).unwrap();
        // Штамп последнего файра в промпте — окно аудита рана (#1480);
        // ms = 2026-11-15T02:29:00+09:00 (jst_stamp туда и обратно).
        let ms = ((20771i64 * 86400) + 17 * 3600 + 29 * 60) * 1000;
        let with = run_prompt("uwuwu", &m, Some(ms as u64));
        assert!(
            with.contains("Last fire: 2026-11-15T02:29:00+09:00"),
            "{with}"
        );
        // Журнала нет ни в какой форме: ни слага, ни append-инструкции
        assert!(!with.contains("Журнал"), "{with}");
        assert!(!with.contains("journal"), "{with}");
        assert!(!with.contains("docs_append"), "{with}");
        // Первый ран — честный «нет записи»
        let first = run_prompt("uwuwu", &m, None);
        assert!(first.contains("none recorded"), "{first}");
    }

    #[test]
    fn schedule_grammar() {
        assert!(parse_schedule_text("5m").is_some());
        assert!(parse_schedule_text("2h").is_some());
        assert!(parse_schedule_text("1d").is_some());
        assert!(parse_schedule_text("06:00").is_some());
        assert!(parse_schedule_text("06:00 15.11").is_some());
        assert!(parse_schedule_text("30 1 * * *").is_some());
        assert!(parse_schedule_text("").is_none());
        assert!(parse_schedule_text("25:00").is_none());
        assert!(
            parse_schedule_text("06:00 31.04").is_none(),
            "апрель — 30 дней"
        );
        assert!(parse_schedule_text("* * *").is_none());
    }

    #[test]
    fn jst_stamp_iso() {
        let ms = {
            // 2026-11-14 16:30 UTC = 2026-11-15 01:30 JST
            let secs = 20771i64 * 86400 + 16 * 3600 + 30 * 60;
            (secs * 1000) as u64
        };
        assert_eq!(jst_stamp(ms), "2026-11-15T01:30:00+09:00");
    }

    /// Тик-харнесс: ин-мемори вики + сессии (CUP-гвард файра, #1231).
    struct MemWiki(Mutex<std::collections::HashMap<String, String>>);
    #[async_trait::async_trait]
    impl WikiRepo for MemWiki {
        async fn read_doc(&self, slug: &str) -> UsecaseResult<Option<String>> {
            Ok(self.0.lock().unwrap().get(slug).cloned())
        }
        async fn list_docs(&self, prefix: &str) -> UsecaseResult<Vec<String>> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .keys()
                .filter(|s| s.starts_with(prefix))
                .cloned()
                .collect())
        }
        async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
            Ok(Vec::new())
        }
        async fn write_doc(&self, slug: &str, content: &str, _m: &str) -> UsecaseResult<()> {
            self.0.lock().unwrap().insert(slug.into(), content.into());
            Ok(())
        }
        async fn append_doc(&self, slug: &str, line: &str, _m: &str) -> UsecaseResult<usize> {
            self.0
                .lock()
                .unwrap()
                .entry(slug.into())
                .or_default()
                .push_str(&format!("{line}\n"));
            Ok(0)
        }
    }

    struct TicksClock;
    impl Clock for TicksClock {
        fn utc_minute(&self) -> uwuwu_domain::UtcMinute {
            uwuwu_domain::UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            2000
        }
    }

    /// Настраиваемые часы: якорная семантика требует движения времени
    /// между тиками (якорь → плановая минута).
    struct SettableClock(std::sync::Mutex<u64>);
    impl SettableClock {
        fn at(ms: u64) -> Arc<Self> {
            Arc::new(Self(std::sync::Mutex::new(ms)))
        }
        fn set(&self, ms: u64) {
            *self.0.lock().unwrap() = ms;
        }
    }
    impl Clock for SettableClock {
        fn utc_minute(&self) -> uwuwu_domain::UtcMinute {
            uwuwu_domain::UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            *self.0.lock().unwrap()
        }
    }

    /// Epoch ms из UTC-полей (минуты JST-окна считаются тестами явно).
    fn epoch_ms(day: i64, hour: i64, minute: i64, extra_ms: i64) -> u64 {
        ((day * 86400 + hour * 3600 + minute * 60) * 1000 + extra_ms) as u64
    }

    /// Фиксированные часы (тишина/отложение: пороги от произвольного now).
    struct FixedClock(u64);
    impl Clock for FixedClock {
        fn utc_minute(&self) -> uwuwu_domain::UtcMinute {
            uwuwu_domain::UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            self.0
        }
    }

    struct MemSessions {
        busy: std::sync::atomic::AtomicUsize,
        spawned: Mutex<Vec<String>>,
        /// true — create_session всегда Err (изоляция файра, ревью #1214).
        fail_create: std::sync::atomic::AtomicBool,
    }
    #[async_trait::async_trait]
    impl crate::ports::AgentSessionGateway for MemSessions {
        async fn create_session(&self, _spec: &crate::ports::SessionSpec) -> UsecaseResult<String> {
            if self.fail_create.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(crate::error::UsecaseError::Session(
                    "исполнитель недоступен".into(),
                ));
            }
            let id = format!("ses_{}", self.spawned.lock().unwrap().len() + 1);
            self.spawned.lock().unwrap().push(id.clone());
            Ok(id)
        }
        async fn prompt(&self, _s: &str, _t: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn run_command(&self, _s: &str, _n: &str, _a: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn liveness(&self, _s: &str) -> crate::ports::Liveness {
            crate::ports::Liveness::Alive
        }
        async fn delete_session(&self, _s: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn last_activity_ms(&self, _s: &str) -> UsecaseResult<Option<u64>> {
            Ok(None)
        }
        async fn last_assistant_reply(
            &self,
            _s: &str,
        ) -> UsecaseResult<Option<crate::ports::AssistantReply>> {
            Ok(None)
        }
        async fn list_sessions(&self) -> UsecaseResult<Vec<crate::ports::SessionInfo>> {
            Ok((0..self.busy.load(std::sync::atomic::Ordering::SeqCst))
                .map(|n| crate::ports::SessionInfo {
                    id: format!("ses_busy_{n}"),
                    agent: String::new(),
                    directory: String::new(),
                    updated_ms: 0,
                    outcome: String::new(),
                    total_tokens: 0,
                    busy: true,
                })
                .collect())
        }
        async fn export_session(&self, _s: &str) -> UsecaseResult<String> {
            Ok(String::new())
        }
    }

    /// Сканируемый исполнитель для сеттлмента (#1480): список сессий
    /// задаёт тест — сессии ранов с их outcome/busy.
    struct ScanSessions(Mutex<Vec<crate::ports::SessionInfo>>);
    #[async_trait::async_trait]
    impl crate::ports::AgentSessionGateway for ScanSessions {
        async fn create_session(&self, _spec: &crate::ports::SessionSpec) -> UsecaseResult<String> {
            let id = format!("ses_new_{}", self.0.lock().unwrap().len() + 1);
            self.0.lock().unwrap().push(crate::ports::SessionInfo {
                id: id.clone(),
                agent: String::new(),
                directory: String::new(),
                updated_ms: 0,
                outcome: String::new(),
                total_tokens: 0,
                busy: true,
            });
            Ok(id)
        }
        async fn prompt(&self, _s: &str, _t: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn run_command(&self, _s: &str, _n: &str, _a: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn liveness(&self, _s: &str) -> crate::ports::Liveness {
            crate::ports::Liveness::Alive
        }
        async fn delete_session(&self, _s: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn last_activity_ms(&self, _s: &str) -> UsecaseResult<Option<u64>> {
            Ok(None)
        }
        async fn last_assistant_reply(
            &self,
            _s: &str,
        ) -> UsecaseResult<Option<crate::ports::AssistantReply>> {
            Ok(None)
        }
        async fn list_sessions(&self) -> UsecaseResult<Vec<crate::ports::SessionInfo>> {
            Ok(self.0.lock().unwrap().clone())
        }
        async fn export_session(&self, _s: &str) -> UsecaseResult<String> {
            Ok(String::new())
        }
    }

    /// ТГ-фейк: собранные тексты сигналов.
    struct MemTg(Mutex<Vec<String>>);
    #[async_trait::async_trait]
    impl crate::bots::TelegramGateway for MemTg {
        async fn send_message(&self, _chat: i64, text: &str) -> UsecaseResult<()> {
            self.0.lock().unwrap().push(text.to_string());
            Ok(())
        }
        async fn get_updates(
            &self,
            _offset: Option<u64>,
            _timeout_sec: u64,
        ) -> UsecaseResult<Vec<crate::bots::TgUpdate>> {
            Ok(Vec::new())
        }
        async fn download_file(&self, _file_id: &str) -> UsecaseResult<Vec<u8>> {
            Ok(Vec::new())
        }
    }

    fn cron_uc(
        wiki: Arc<MemWiki>,
        sessions: Arc<MemSessions>,
        cup: usize,
    ) -> (CronUseCase, Arc<MemCronState>) {
        let state = MemCronState::new();
        let u = CronUseCase::new(
            wiki,
            Arc::new(SessionRouter::uniform(sessions)),
            Arc::new(TicksClock),
            Arc::new(crate::spawn_models::SpawnModels::canonical()),
            std::collections::HashMap::from([("uwuwu".to_string(), "/tmp/checkout".to_string())]),
            cup,
            state.clone(),
            None,
        );
        (u, state)
    }

    fn master_wiki() -> Arc<MemWiki> {
        Arc::new(MemWiki(Mutex::new(std::collections::HashMap::from([(
            "cron/тест/README".into(),
            "---\ntitle: Тест\nschedule: 5m\nproject: uwuwu\n---\nТело".into(),
        )]))))
    }

    #[tokio::test]
    async fn cron_fire_held_when_cup_full() {
        // Единый кап (#1231): крон-файр — запуск хода; при полном капе
        // файр откладывается, state-запись файра (last_fire/run-трек) НЕ
        // пишется — ран не считается состоявшимся, расписание перезапустит
        // файр. Метка отложения пишется — детект пропуска файра (#1480).
        // Первая встреча (#1517): тик якорит, файр — на следующем интервале.
        let wiki = master_wiki();
        let sessions = Arc::new(MemSessions {
            busy: 1.into(),
            spawned: Mutex::default(),
            fail_create: false.into(),
        });
        let clock = SettableClock::at(epoch_ms(20780, 1, 0, 0));
        let state = MemCronState::new();
        let u = cron_uc_clock(wiki, sessions.clone(), 1, clock.clone(), state.clone());
        let fired = u.tick().await.unwrap();
        assert!(fired.is_empty(), "первая встреча — якорь, не файр");
        clock.set(epoch_ms(20780, 1, 5, 0)); // +5 минут — интервал 5m due
        let fired = u.tick().await.unwrap();
        assert!(fired.is_empty(), "файр при полном капе отложен");
        assert!(sessions.spawned.lock().unwrap().is_empty(), "спавна нет");
        let row = state.row("тест");
        assert!(row.last_fire_ms.is_none(), "state-запись файра не пишется");
        assert!(row.run.is_none(), "run-трек не заводится");
        assert!(
            row.deferred_since_ms.is_some(),
            "метка отложения стоит — детект пропуска файра ведётся"
        );
    }

    #[tokio::test]
    async fn cron_fire_deferred_when_scan_fails() {
        // Отказная ветка (#1450): ВСЕ исполнители недоступны — файр
        // откладывается до свободного тика, ран не считается
        // состоявшимся (консистентно с конвейером). Частичный отказ
        // файр не останавливает — busy по живому union (тест ниже).
        let wiki = master_wiki();
        let sessions = Arc::new(FailingSessions);
        let state = MemCronState::new();
        let u = CronUseCase::new(
            wiki,
            Arc::new(SessionRouter::uniform(sessions)),
            Arc::new(TicksClock),
            Arc::new(crate::spawn_models::SpawnModels::canonical()),
            std::collections::HashMap::from([("uwuwu".to_string(), "/tmp/checkout".to_string())]),
            7,
            state.clone(),
            None,
        );
        let fired = u.tick().await.unwrap();
        assert!(fired.is_empty(), "отказ скана — файр отложен");
    }

    #[tokio::test]
    async fn cron_fire_partial_degradation_fires() {
        // Смешанное здоровье (ревью #1450): один исполнитель отказной,
        // живой отвечает — файр идёт, busy считается по живому union.
        // Спавн уходит живому исполнителю (дефолт роутера). Первая
        // встреча якорится (#1517), файр — на следующем интервале.
        let wiki = master_wiki();
        let live_mem = Arc::new(MemSessions {
            busy: 0.into(),
            spawned: Mutex::default(),
            fail_create: false.into(),
        });
        let live: crate::router::Gateway = live_mem.clone();
        let failing: crate::router::Gateway = Arc::new(FailingSessions);
        let clock = SettableClock::at(epoch_ms(20780, 1, 0, 0));
        let state = MemCronState::new();
        let u = CronUseCase::new(
            wiki,
            Arc::new(SessionRouter::new(
                "kisa",
                live.clone(),
                vec![("redmi".into(), failing)],
                std::collections::HashMap::new(),
            )),
            clock.clone(),
            Arc::new(crate::spawn_models::SpawnModels::canonical()),
            std::collections::HashMap::from([("uwuwu".to_string(), "/tmp/checkout".to_string())]),
            7,
            state.clone(),
            None,
        );
        u.tick().await.unwrap();
        clock.set(epoch_ms(20780, 1, 5, 0)); // +5 минут — интервал 5m due
        let fired = u.tick().await.unwrap();
        assert_eq!(
            fired.len(),
            1,
            "частичный отказ файр не морозит — busy по живому union"
        );
        assert_eq!(
            live_mem.spawned.lock().unwrap().len(),
            1,
            "спавн ушёл живому исполнителю"
        );
    }

    #[tokio::test]
    async fn cron_fire_passes_with_free_slot() {
        // Свободный слот: файр состоявшийся — state-запись (last_fire +
        // run-трек с сессией) + спавн. Первая встреча якорится (#1517),
        // файр — на следующем интервале.
        let wiki = master_wiki();
        let sessions = Arc::new(MemSessions {
            busy: 0.into(),
            spawned: Mutex::default(),
            fail_create: false.into(),
        });
        let clock = SettableClock::at(epoch_ms(20780, 1, 0, 0));
        let state = MemCronState::new();
        let u = cron_uc_clock(wiki, sessions.clone(), 1, clock.clone(), state.clone());
        let fired = u.tick().await.unwrap();
        assert!(fired.is_empty(), "первая встреча — якорь");
        clock.set(epoch_ms(20780, 1, 5, 0)); // +5 минут — интервал 5m due
        let fired = u.tick().await.unwrap();
        assert_eq!(fired.len(), 1, "файр при свободном слоте");
        assert_eq!(sessions.spawned.lock().unwrap().len(), 1);
        let row = state.row("тест");
        assert_eq!(
            row.last_fire_ms,
            Some(epoch_ms(20780, 1, 5, 0)),
            "факт файра в state"
        );
        let track = row.run.expect("run-трек заведён");
        assert_eq!(track.session, "ses_1", "сессия рана в учёте");
        assert!(state.saves() > 0, "save был");
    }

    /// CronUseCase поверх настраиваемых часов и готового state (тесты
    /// якорной семантики: потеря state, первая встреча крона).
    fn cron_uc_clock(
        wiki: Arc<MemWiki>,
        sessions: Arc<MemSessions>,
        cup: usize,
        clock: Arc<SettableClock>,
        state: Arc<MemCronState>,
    ) -> CronUseCase {
        CronUseCase::new(
            wiki,
            Arc::new(SessionRouter::uniform(sessions)),
            clock,
            Arc::new(crate::spawn_models::SpawnModels::canonical()),
            std::collections::HashMap::from([("uwuwu".to_string(), "/tmp/checkout".to_string())]),
            cup,
            state,
            None,
        )
    }

    fn two_cron_masters() -> Arc<MemWiki> {
        Arc::new(MemWiki(Mutex::new(std::collections::HashMap::from([
            (
                "cron/sentry/README".into(),
                "---\ntitle: Sentry\nschedule: 0 5 * * *\nproject: uwuwu\n---\nТело".into(),
            ),
            (
                "cron/qa/README".into(),
                "---\ntitle: QA\nschedule: 30 1 * * *\nproject: uwuwu\n---\nТело".into(),
            ),
        ]))))
    }

    #[tokio::test]
    async fn state_loss_anchors_instead_of_mass_catchup() {
        // R2 (#1517, Д2): потеря state (битый файл/сброс) — пустой сторе.
        // Ни одного файра: кроны якорятся и ждут своих плановых минут.
        // Прежнее поведение — масс-догон прошлых плановых минут разом
        // (всплеск пяти кронов 23.11 05:37–05:41).
        let sessions = Arc::new(MemSessions {
            busy: 0.into(),
            spawned: Mutex::default(),
            fail_create: false.into(),
        });
        let clock = SettableClock::at(epoch_ms(20780, 1, 0, 0)); // 23.11 10:00 JST
        let lost = MemCronState::new();
        let u = cron_uc_clock(
            two_cron_masters(),
            sessions.clone(),
            7,
            clock.clone(),
            lost.clone(),
        );
        let fired = u.tick().await.unwrap();
        assert!(fired.is_empty(), "потеря state — догона нет");
        assert!(sessions.spawned.lock().unwrap().is_empty(), "спавнов нет");
        assert_eq!(lost.row("sentry").last_fire_ms, None, "факта файра нет");
        assert_eq!(
            lost.row("sentry").anchor_ms,
            Some(epoch_ms(20780, 1, 0, 0)),
            "якорь первой встречи записан"
        );
        assert_eq!(lost.row("qa").anchor_ms, Some(epoch_ms(20780, 1, 0, 0)));
    }

    #[tokio::test]
    async fn new_cron_anchors_then_fires_next_scheduled_minute() {
        // R3 (#1517): крон без факта (первая встреча после деплоя/завода)
        // не файрит «сразу» — якорится и файрит ближайшую плановую минуту.
        let wiki = Arc::new(MemWiki(Mutex::new(std::collections::HashMap::from([(
            "cron/daily/README".into(),
            "---\ntitle: Daily\nschedule: 01:30\nproject: uwuwu\n---\nТело".into(),
        )]))));
        let sessions = Arc::new(MemSessions {
            busy: 0.into(),
            spawned: Mutex::default(),
            fail_create: false.into(),
        });
        let clock = SettableClock::at(epoch_ms(20780, 5, 0, 0)); // 23.11 14:00 JST
        let state = MemCronState::new();
        let u = cron_uc_clock(wiki, sessions.clone(), 7, clock.clone(), state.clone());
        let fired = u.tick().await.unwrap();
        assert!(fired.is_empty(), "первая встреча — файра нет");
        assert!(
            sessions.spawned.lock().unwrap().is_empty(),
            "спавна при якоре нет"
        );
        assert_eq!(state.row("daily").anchor_ms, Some(epoch_ms(20780, 5, 0, 0)));
        // Ближайшая плановая минута: 24.11 01:31 JST — файр
        clock.set(epoch_ms(20780, 16, 31, 0));
        let fired = u.tick().await.unwrap();
        assert_eq!(fired.len(), 1, "файр в плановую минуту после якоря");
        assert_eq!(
            state.row("daily").last_fire_ms,
            Some(epoch_ms(20780, 16, 31, 0))
        );
        assert_eq!(
            state.row("daily").anchor_ms,
            Some(epoch_ms(20780, 5, 0, 0)),
            "якорь остаётся, погашен фактом"
        );
    }

    /// CronUseCase поверх готового роутера и фиксированных часов (тесты
    /// сеттлмента/тишины/пропуска: state сеется напрямую, файр не нужен).
    fn cron_uc_router(
        router: SessionRouter,
        now_ms: u64,
        cup: usize,
    ) -> (CronUseCase, Arc<MemCronState>) {
        let state = MemCronState::new();
        let u = CronUseCase::new(
            master_wiki(),
            Arc::new(router),
            Arc::new(FixedClock(now_ms)),
            Arc::new(crate::spawn_models::SpawnModels::canonical()),
            std::collections::HashMap::from([("uwuwu".to_string(), "/tmp/checkout".to_string())]),
            cup,
            state.clone(),
            None,
        );
        (u, state)
    }

    fn cron_uc_scan(
        listed: Vec<crate::ports::SessionInfo>,
        now_ms: u64,
    ) -> (CronUseCase, Arc<MemCronState>) {
        let scan: crate::router::Gateway = Arc::new(ScanSessions(Mutex::new(listed)));
        cron_uc_router(SessionRouter::uniform(scan), now_ms, 7)
    }

    /// Трек живого рана в state (после файра).
    /// Трек живого рана в state: fired_ms — момент файра (тишина),
    /// last_fire_ms — независим (свежий last_fire гасит повторный файр
    /// 5m-интервала в тестах сеттлмента).
    fn run_track(session: &str, fired_ms: u64, last_fire_ms: u64) -> CronRow {
        CronRow {
            last_fire_ms: Some(last_fire_ms),
            anchor_ms: None,
            deferred_since_ms: None,
            run: Some(CronRunTrack {
                session: session.into(),
                fired_ms,
            }),
            fails: 0,
        }
    }

    fn sess(id: &str, outcome: &str, busy: bool) -> crate::ports::SessionInfo {
        crate::ports::SessionInfo {
            id: id.into(),
            agent: String::new(),
            directory: String::new(),
            updated_ms: 0,
            outcome: outcome.into(),
            total_tokens: 100,
            busy,
        }
    }

    #[tokio::test]
    async fn settlement_success_closes_run_quietly() {
        // Завершение рана снимается с сессии исполнителя (#1480): ход
        // завершён (не busy, outcome выставлен) → трек закрыт, отказов нет.
        // last_fire свежий — интервал не рефайрится поверх сеттлмента.
        let (u, state) = cron_uc_scan(vec![sess("ses_1", "succeeded", false)], 1_000_000_000);
        state.set_cron_row("тест", run_track("ses_1", 999_940_000, 999_940_000));
        u.tick().await.unwrap();
        let row = state.row("тест");
        assert!(row.run.is_none(), "успешный ран закрыт");
        assert_eq!(row.fails, 0, "успех — отказов нет");
    }

    #[tokio::test]
    async fn settlement_failed_outcome_signals() {
        let (u, state) = cron_uc_scan(vec![sess("ses_1", "failed", false)], 1_000_000_000);
        state.set_cron_row("тест", run_track("ses_1", 999_940_000, 999_940_000));
        u.tick().await.unwrap();
        let row = state.row("тест");
        assert!(row.run.is_none(), "закрытый ран не висит");
        assert_eq!(row.fails, 1, "ошибка исполнителя — отказ");
    }

    #[tokio::test]
    async fn settlement_missing_session_signals() {
        // Все исполнители живы, сессии рана нет — ран погиб (снесён).
        let (u, state) = cron_uc_scan(vec![sess("ses_other", "succeeded", false)], 1_000_000_000);
        state.set_cron_row("тест", run_track("ses_1", 999_940_000, 999_940_000));
        u.tick().await.unwrap();
        let row = state.row("тест");
        assert!(row.run.is_none());
        assert_eq!(row.fails, 1, "исчезнувшая сессия — отказ");
    }

    #[tokio::test]
    async fn settlement_holds_busy_and_failed_executor() {
        // busy — ход в полёте: трек жив. Отказной исполнитель — слепой
        // hold: сессии невидимы скану, суждение откладывается без ложной
        // тревоги (семантика #1263).
        let (u, state) = cron_uc_scan(vec![sess("ses_1", "", true)], 1_000_000_000);
        state.set_cron_row("тест", run_track("ses_1", 999_940_000, 999_940_000));
        u.tick().await.unwrap();
        assert!(state.row("тест").run.is_some(), "busy — ран жив");

        let scan: crate::router::Gateway = Arc::new(ScanSessions(Mutex::new(vec![sess(
            "ses_x",
            "succeeded",
            false,
        )])));
        let failing: crate::router::Gateway = Arc::new(FailingSessions);
        let (u2, state2) = cron_uc_router(
            SessionRouter::new(
                "kisa",
                scan,
                vec![("redmi".into(), failing)],
                std::collections::HashMap::new(),
            ),
            1_000_000_000,
            7,
        );
        state2.set_cron_row("тест", run_track("ses_1", 999_940_000, 999_940_000));
        u2.tick().await.unwrap();
        let row = state2.row("тест");
        assert!(row.run.is_some(), "отказной исполнитель — hold, трек жив");
        assert_eq!(row.fails, 0, "ложной тревоги нет");
    }

    #[tokio::test]
    async fn settlement_holds_crash_track_without_session() {
        // Crash-семантика файра (#1480): state-запись до спавна, краш
        // между записью и спавном оставляет трек без сессии. Сеттлмент
        // не судит вслепую (ветка пустой сессии), тишина-детект закроет
        // эпизод по порогу — свежий crash-трек не отказ и не рефайр.
        let now = 1_000_000_000_000u64;
        let (u, state) = cron_uc_scan(vec![sess("ses_1", "succeeded", false)], now);
        state.set_cron_row("тест", run_track("", now - 60_000, now - 60_000));
        u.tick().await.unwrap();
        let row = state.row("тест");
        assert!(row.run.is_some(), "crash-трек без сессии не сеттлится");
        assert_eq!(row.fails, 0, "ложной тревоги нет");
    }

    #[tokio::test]
    async fn silence_closes_crash_track_over_threshold() {
        // Реалистичный crash-таймлайн (#1480): файр умер между записью
        // state и спавном (last_fire = fired_ms = момент краша). Через
        // порог тишины эпизод закрывается отказом (UC-02), интервал к
        // этому моменту снова due — тот же тик перезапускает ран
        // (восстановление без ручного вмешательства).
        let now = 1_000_000_000_000u64;
        let (u, state) = cron_uc_scan(vec![sess("ses_1", "succeeded", false)], now);
        state.set_cron_row(
            "тест",
            run_track("", now - RUN_SILENCE_MS - 1, now - RUN_SILENCE_MS - 1),
        );
        u.tick().await.unwrap();
        let row = state.row("тест");
        assert_eq!(row.fails, 1, "тишина закрыла crash-эпизод отказом");
        assert_eq!(
            row.last_fire_ms,
            Some(now),
            "интервал снова due — тот же тик перезапустил ран"
        );
        assert!(row.run.is_some(), "новый ран живёт в треке");
    }

    #[tokio::test]
    async fn silence_over_threshold_signals_and_closes() {
        // Тишина рана (UC-02): файр был, завершения нет > 6 ч → отказ +
        // эпизод закрыт. Сессия busy — сеттлмент держит, тишина закрывает;
        // last_fire свежий — интервал не рефайрится.
        let now = 1_000_000_000_000u64;
        let (u, state) = cron_uc_scan(vec![sess("ses_1", "", true)], now);
        state.set_cron_row(
            "тест",
            run_track("ses_1", now - RUN_SILENCE_MS - 1, now - 60_000),
        );
        u.tick().await.unwrap();
        let row = state.row("тест");
        assert!(row.run.is_none(), "тишина закрывает эпизод");
        assert_eq!(row.fails, 1);
    }

    #[tokio::test]
    async fn silence_below_threshold_holds() {
        let now = 1_000_000_000_000u64;
        let (u, state) = cron_uc_scan(vec![sess("ses_1", "", true)], now);
        state.set_cron_row(
            "тест",
            run_track("ses_1", now - RUN_SILENCE_MS + 60_000, now - 60_000),
        );
        u.tick().await.unwrap();
        assert!(state.row("тест").run.is_some(), "до порога ран жив");
        assert_eq!(state.row("тест").fails, 0);
    }

    #[tokio::test]
    async fn missed_fire_over_threshold_signals() {
        // Пропущенный файр: крон с якорем (факт первой встречи) держится
        // due (CUP полон), отложение > 6 ч → отказ; метка эпизода
        // сбрасывается, state-запись файра не пишется.
        let now = 1_000_000_000_000u64;
        let sessions: crate::router::Gateway = Arc::new(MemSessions {
            busy: 1.into(),
            spawned: Mutex::default(),
            fail_create: false.into(),
        });
        let (u, state) = cron_uc_router(SessionRouter::uniform(sessions), now, 1);
        state.set_cron_row(
            "тест",
            CronRow {
                last_fire_ms: None,
                anchor_ms: Some(now - RUN_SILENCE_MS - 60_000),
                deferred_since_ms: Some(now - RUN_SILENCE_MS - 1),
                run: None,
                fails: 0,
            },
        );
        let fired = u.tick().await.unwrap();
        assert!(fired.is_empty(), "CUP полон — файра нет");
        let row = state.row("тест");
        assert_eq!(row.fails, 1, "пропущенный файр — отказ");
        assert!(row.last_fire_ms.is_none(), "файр не состоялся — записи нет");
        assert!(row.deferred_since_ms.is_none(), "эпизод закрыт");
    }

    #[tokio::test]
    async fn missed_fire_deferred_below_threshold_no_signal() {
        let now = 1_000_000_000_000u64;
        let sessions: crate::router::Gateway = Arc::new(MemSessions {
            busy: 1.into(),
            spawned: Mutex::default(),
            fail_create: false.into(),
        });
        let (u, state) = cron_uc_router(SessionRouter::uniform(sessions), now, 1);
        // Якорь 5 минут назад: интервал 5m — файр due, CUP полон держит.
        state.set_cron_row(
            "тест",
            CronRow {
                last_fire_ms: None,
                anchor_ms: Some(now - 300_000),
                deferred_since_ms: None,
                run: None,
                fails: 0,
            },
        );
        let fired = u.tick().await.unwrap();
        assert!(fired.is_empty());
        let row = state.row("тест");
        assert_eq!(row.fails, 0, "свежее отложение — не отказ");
        assert!(row.deferred_since_ms.is_some(), "метка отложения стоит");
    }

    #[tokio::test]
    async fn signals_anti_repeat_within_cooldown() {
        // Анти-повтор (#1281-философия): два сбоя разных кронов в одном
        // тике (порог 1, cooldown) — один ТГ на эпизод, счётчики у обоих.
        let mut docs = std::collections::HashMap::new();
        for (name, title) in [("тест-1", "Тест 1"), ("тест-2", "Тест 2")] {
            docs.insert(
                format!("cron/{name}/README"),
                format!("---\ntitle: {title}\nschedule: 5m\nproject: uwuwu\n---\nТело"),
            );
        }
        let wiki = Arc::new(MemWiki(Mutex::new(docs)));
        let sessions = Arc::new(MemSessions {
            busy: 0.into(),
            spawned: Mutex::default(),
            fail_create: true.into(),
        });
        let clock = SettableClock::at(epoch_ms(20780, 1, 0, 0));
        let state = MemCronState::new();
        let mut u = cron_uc_clock(wiki, sessions, 7, clock.clone(), state.clone());
        let tg = Arc::new(MemTg(Mutex::default()));
        u.notify = Some((tg.clone() as Arc<dyn crate::bots::TelegramGateway>, 42));
        u.tick().await.unwrap(); // якорь первой встречи (#1517)
        clock.set(epoch_ms(20780, 1, 5, 0)); // +5 минут — оба due, спавны падают
        let fired = u.tick().await.unwrap();
        assert!(fired.is_empty(), "спавны не удались");
        assert_eq!(
            tg.0.lock().unwrap().len(),
            1,
            "один сигнал на эпизод, не шторм"
        );
        assert_eq!(state.row("тест-1").fails, 1);
        assert_eq!(state.row("тест-2").fails, 1);
    }

    #[tokio::test]
    async fn notify_off_counts_failure_without_send() {
        // [notify] не задан — громкий лог, счётчик отказов ведётся.
        let wiki = master_wiki();
        let sessions = Arc::new(MemSessions {
            busy: 0.into(),
            spawned: Mutex::default(),
            fail_create: true.into(),
        });
        let clock = SettableClock::at(epoch_ms(20780, 1, 0, 0));
        let state = MemCronState::new();
        let u = cron_uc_clock(wiki, sessions, 7, clock.clone(), state.clone());
        u.tick().await.unwrap(); // якорь первой встречи (#1517)
        clock.set(epoch_ms(20780, 1, 5, 0)); // +5 минут — файр, спавн падает
        let fired = u.tick().await.unwrap();
        assert!(fired.is_empty());
        assert_eq!(state.row("тест").fails, 1, "отказ посчитан без ТГ");
    }

    #[tokio::test]
    async fn cron_multi_fire_no_oversell_within_tick() {
        // Оверселл внутри прохода (#1244-класс, ревью #1231): два due-мастера
        // при cup=1 — первый файр занимает слот, второй откладывается;
        // без инкремента busy между файрами оба улетают (cap+1 ход).
        let mut docs = std::collections::HashMap::new();
        for (name, title) in [("тест-1", "Тест 1"), ("тест-2", "Тест 2")] {
            let slug = format!("cron/{name}/README");
            docs.insert(
                slug,
                format!("---\ntitle: {title}\nschedule: 5m\nproject: uwuwu\n---\nТело"),
            );
        }
        let wiki = Arc::new(MemWiki(Mutex::new(docs)));
        let sessions = Arc::new(MemSessions {
            busy: 0.into(),
            spawned: Mutex::default(),
            fail_create: false.into(),
        });
        let clock = SettableClock::at(epoch_ms(20780, 1, 0, 0));
        let state = MemCronState::new();
        let u = cron_uc_clock(wiki, sessions.clone(), 1, clock.clone(), state);
        u.tick().await.unwrap(); // первая встреча — якорь обоих (#1517)
        clock.set(epoch_ms(20780, 1, 5, 0)); // +5 минут — оба due
        let fired = u.tick().await.unwrap();
        assert_eq!(fired.len(), 1, "второй файр отложен занятым первым слотом");
        assert_eq!(sessions.spawned.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn spawn_failure_isolates_single_cron() {
        // Ревью #1214: сбой спавна одного мастера не обрывает тик —
        // остальные мастера файрятся при свободных слотах. last_fire у
        // несостоявшегося рана стоит (state до спавна), run-трек сброшен,
        // отказ посчитан; Outcome в тик не попадает (#1480).
        let mut docs = std::collections::HashMap::new();
        for (name, title) in [("тест-1", "Тест 1"), ("тест-2", "Тест 2")] {
            let slug = format!("cron/{name}/README");
            docs.insert(
                slug,
                format!("---\ntitle: {title}\nschedule: 5m\nproject: uwuwu\n---\nТело"),
            );
        }
        let wiki = Arc::new(MemWiki(Mutex::new(docs)));
        let sessions = Arc::new(MemSessions {
            busy: 0.into(),
            spawned: Mutex::default(),
            fail_create: true.into(),
        });
        let clock = SettableClock::at(epoch_ms(20780, 1, 0, 0));
        let state = MemCronState::new();
        let u = cron_uc_clock(wiki, sessions, 7, clock.clone(), state.clone());
        u.tick().await.unwrap(); // первая встреча — якорь обоих (#1517)
        clock.set(epoch_ms(20780, 1, 5, 0)); // +5 минут — оба due
        let fired = u.tick().await.unwrap();
        assert!(fired.is_empty(), "спавны не удались — outcome нет");
        for name in ["тест-1", "тест-2"] {
            let row = state.row(name);
            assert!(row.last_fire_ms.is_some(), "{name}: файр записан до спавна");
            assert!(row.run.is_none(), "{name}: run-трек сброшен");
            assert_eq!(row.fails, 1, "{name}: отказ посчитан");
        }
    }

    #[tokio::test]
    async fn masters_inventory_filter_and_journal_exclusion() {
        // Инвентарь /cron: не-README пути (легаси-журналы замороженных
        // кронов, ассеты) — не мастера; project — опциональный фильтр
        // (cron_list без проекта — все кроны).
        let mut docs = std::collections::HashMap::new();
        for (name, project) in [("крон-а", "uwuwu"), ("крон-б", "origa")] {
            docs.insert(
                format!("cron/{name}/README"),
                format!("---\ntitle: {name}\nschedule: 1h\nproject: {project}\n---\nТело"),
            );
        }
        docs.insert("cron/крон-а/Журнал".into(), "- замороженная история".into());
        docs.insert("cron/крон-а/скрин.png.txt".into(), "не-мастер".into());
        // Мастер — только README внутри подпапки (#1214, план): loose-файл
        // на верхнем уровне cron/ и вложенный сегмент — вне инвентаря.
        docs.insert("cron/заметка.md".into(), "не подпапка".into());
        docs.insert(
            "cron/а/б/README".into(),
            "---\nschedule: 1h\nproject: uwuwu\n---".into(),
        );
        let wiki = Arc::new(MemWiki(Mutex::new(docs)));
        let sessions = Arc::new(MemSessions {
            busy: 0.into(),
            spawned: Mutex::default(),
            fail_create: false.into(),
        });
        let (u, _state) = cron_uc(wiki, sessions, 1);
        let all = u.masters(None).await.unwrap();
        assert_eq!(
            all.len(),
            2,
            "журнал, не-README и вложенные пути — вне инвентаря"
        );
        let uwuwu_only = u.masters(Some("uwuwu")).await.unwrap();
        assert_eq!(uwuwu_only.len(), 1);
        assert_eq!(uwuwu_only[0].project, "uwuwu");
    }
}
