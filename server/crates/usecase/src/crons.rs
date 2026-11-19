//! Кроны на вики-носителе (#1058, раскладка /cron — #1214): мастер =
//! docs/cron/<имя>/README.md (fm: schedule + agent + project), журнал =
//! cron/<имя>/Журнал.md. Демон дописывает строку запуска, ран-агент —
//! сводку (MCP docs_append). Домен — uwuwu_domain::cron (Schedule/is_due);
//! здесь грамматика строк носителя (parse_schedule_text) и IO.

use crate::error::UsecaseResult;
use crate::naming;
use crate::ports::{Clock, SessionSpec, WikiRepo};
use crate::router::SessionRouter;
use crate::spawn_models::SpawnModels;
use std::sync::Arc;
use uwuwu_domain::{
    civil_from_days, AgentSlot, CronField, CronMaster, CronSchedule, IntervalMinutes, JstDate,
    JstTime, Schedule, ScheduleKind,
};

/// Верхний уровень конфигурации кронов (#1214): дети — подпапки кронов,
/// мастер — `cron/<имя>/README.md`, журнал — `cron/<имя>/Журнал`.
pub const CRON_DIR: &str = "cron";

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
    /// полном капе откладывается до свободного тика — журнальная строка
    /// не пишется, ран не считается состоявшимся (#1231).
    pub cup: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FireOutcome {
    pub slug: String,
    pub session: String,
}

impl CronUseCase {
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

    /// Один проход: мастера, due → журнальная строка + спавн рана.
    /// Проект рана — fm `project` мастера (роутинг executor + checkout).
    pub async fn tick(&self) -> UsecaseResult<Vec<FireOutcome>> {
        let mut fired = Vec::new();
        // CUP-гвард (#1231): крон-ран — запуск хода, ему нужен свободный
        // слот единого капа. Strict union (#1203): отказ скана — файр
        // откладывается (ран не считается состоявшимся, журнал не пишется).
        // busy — счётчик на весь тик: файр текущего тика занимает слот
        // (инкремент после успешного запуска), иначе мульти-файр при
        // неполном капе оверселлит до cap+N-1 (#1244-класс, ревью #1231).
        let mut busy = match self.sessions.list_sessions_strict().await {
            Ok(list) => crate::router::busy_count(&list),
            Err(e) => {
                tracing::warn!(
                    event = "cron.list_sessions_failed",
                    error = %e,
                    "Кроны: union-скан недоступен — файр отложен"
                );
                return Ok(fired);
            }
        };
        let masters = self.scan_masters().await?;
        for master in masters {
            let name = cron_name(&master.slug).to_string();
            let journal = format!("{CRON_DIR}/{name}/Журнал");
            let journal_md = self.wiki.read_doc(&journal).await?.unwrap_or_default();
            warn_stale_run(&master.slug, &journal_md, self.clock.now_ms());
            let now = self.clock.now_ms();
            if !master.schedule.is_due(now, last_run_ms(&journal_md)) {
                continue;
            }
            if busy >= self.cup {
                tracing::info!(
                    event = "cron.held_cup",
                    cron = %master.slug,
                    active = busy,
                    cap = self.cup,
                    "Крон-файр отложен: CUP cap — ран не считается состоявшимся"
                );
                continue;
            }
            let Some(checkout) = self.projects.get(&master.project) else {
                tracing::warn!(event = "cron.project_unknown", cron = %master.slug,
                    project = %master.project,
                    "fm project вне карты [[project]] — крон не файрится (skip+warn)");
                continue;
            };
            let outcome = self
                .fire_run(&master, &journal, &journal_md, checkout, now)
                .await?;
            busy += 1; // файр этого тика занимает слот капа
            if let Some(outcome) = outcome {
                fired.push(outcome);
            }
        }
        Ok(fired)
    }

    /// Файр одного мастера: журнальная строка + спавн рана на его
    /// проект-исполнителе. None — спавн не удался (create/prompt Err):
    /// журнал уже пометил запуск, stale-детект поднимет сигнал через 2ч,
    /// остальные мастера тика продолжаются.
    async fn fire_run(
        &self,
        master: &CronMaster,
        journal: &str,
        journal_md: &str,
        checkout: &str,
        now: u64,
    ) -> UsecaseResult<Option<FireOutcome>> {
        let slug = master.slug.clone();
        let stamp = jst_stamp(now);
        self.wiki
            .append_doc(
                journal,
                &format!("- {stamp} запуск (демон)"),
                &format!("cron: запуск {slug}"),
            )
            .await?;
        let prompt = run_prompt(&master.project, master, journal_md);
        let sm = match master.agent {
            AgentSlot::Role(agent) => self.models.for_agent(agent),
            AgentSlot::Default => self.models.for_default(),
        };
        let spec = SessionSpec {
            custom_agent: None,
            directory: checkout.to_string(),
            title: format!("⏰ {} · {}", master.title, stamp),
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
                return Ok(None);
            }
        };
        if let Err(e) = sessions.prompt(&session, &prompt).await {
            tracing::warn!(event = "cron.fire_failed", cron = %slug,
                session = %session, error = %e,
                "Промпт крон-рана не доставлен — остальные мастера тика продолжаются");
            return Ok(None);
        }
        tracing::info!(event = "cron.fired", cron = %slug, session = %session, "Cron fired");
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
/// (не хвост: слаг кончается README).
fn cron_name(slug: &str) -> &str {
    slug.strip_prefix(CRON_DIR)
        .and_then(|r| r.strip_prefix('/'))
        .and_then(|r| r.strip_suffix("/README"))
        .filter(|n| !n.is_empty() && !n.contains('/'))
        .unwrap_or_else(|| slug.rsplit('/').next().unwrap_or(slug))
}

/// Инвариант DS: ран не закрылся = сигнал сбоя — последняя строка
/// журнала «запуск» без сводки старше 2 часов → warn (наблюдаемость).
fn warn_stale_run(slug: &str, journal_md: &str, now: u64) {
    let Some(last) = journal_md.lines().rev().find(|l| !l.trim().is_empty()) else {
        return;
    };
    if !last.contains("запуск (демон)") {
        return;
    }
    let stale = parse_iso_ms(last.split_whitespace().nth(1).unwrap_or_default())
        .map(|ts| now.saturating_sub(ts) > 2 * 3_600_000)
        .unwrap_or(false);
    if stale {
        tracing::warn!(event = "cron.run_stale", cron = %slug,
            last = %last.chars().take(60).collect::<String>(),
            "Крон-ран не закрылся >2ч — сигнал сбоя");
    }
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

/// Последний запуск из журнала: epoch ms строки «- <ISO-штамп> запуск …».
fn last_run_ms(journal: &str) -> Option<u64> {
    journal
        .lines()
        .rev()
        .find(|l| l.contains("запуск"))
        .and_then(|l| parse_iso_ms(l.split_whitespace().nth(1)?))
}

/// ISO-штамп журнала (парсится parse_iso_ms).
fn parse_iso_ms(stamp: &str) -> Option<u64> {
    crate::fm_lite::parse_iso_ms(stamp)
}

/// JST-штамп журнала: ISO с зоной (+09:00 — читается обратно).
fn jst_stamp(ms: u64) -> String {
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

fn run_prompt(project: &str, master: &CronMaster, journal: &str) -> String {
    let tail: Vec<&str> = journal.lines().rev().take(5).collect();
    // имя крона — средний сегмент слага мастера cron/<имя>/README
    let name = cron_name(&master.slug);
    format!(
        "Крон-ран «{title}» (проект {project}).\n\n\
         ## Инструкция мастера\n\n{body}\n\n\
         ## Журнал (последние записи, сверху новые)\n\n{tail}\n\n\
         Выполни ран автономно. Сводку допиши в журнал:\n\
         uwuwu_docs_append {{slug: \"cron/{name}/Журнал\", line: \"- <итог одной строкой>\"}}.\n\
         Блокер — коммент в задачу-хвост не нужен; просто зафиксируй в журнале.",
        title = master.title,
        body = master.body,
        tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};

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
    fn run_prompt_points_to_flat_journal() {
        let md = "---\ntitle: Тест\nschedule: 5m\nproject: uwuwu\n---\nТело";
        let m = parse_master("cron/тест/README", md).unwrap();
        let prompt = run_prompt("uwuwu", &m, "- журнал");
        assert!(
            prompt.contains(r#"slug: "cron/тест/Журнал""#),
            "промпт рана ведёт на журнальный слаг новой раскладки: {prompt}"
        );
        assert!(
            !prompt.contains("Кроны"),
            "старый путь из промпта изгнан: {prompt}"
        );
    }

    #[test]
    fn last_run_reads_iso_stamp() {
        let journal = "- мусор\n- 2026-11-15T02:29:00+09:00 запуск (демон)\n- 2026-11-15T03:00:00+09:00 сводка";
        assert_eq!(
            last_run_ms(journal),
            fm_lite_parse("2026-11-15T02:29:00+09:00")
        );
        assert_eq!(last_run_ms("пусто"), None);
    }

    fn fm_lite_parse(s: &str) -> Option<u64> {
        crate::fm_lite::parse_iso_ms(s)
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
        async fn append_doc(&self, slug: &str, line: &str, _m: &str) -> UsecaseResult<()> {
            self.0
                .lock()
                .unwrap()
                .entry(slug.into())
                .or_default()
                .push_str(&format!("{line}\n"));
            Ok(())
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

    fn cron_uc(wiki: Arc<MemWiki>, sessions: Arc<MemSessions>, cup: usize) -> CronUseCase {
        CronUseCase {
            wiki,
            sessions: Arc::new(SessionRouter::uniform(sessions)),
            clock: Arc::new(TicksClock),
            models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
            projects: std::collections::HashMap::from([(
                "uwuwu".to_string(),
                "/tmp/checkout".to_string(),
            )]),
            cup,
        }
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
        // файр откладывается, журнальная строка НЕ пишется — ран не
        // считается состоявшимся, расписание перезапустит файр.
        let wiki = master_wiki();
        let sessions = Arc::new(MemSessions {
            busy: 1.into(),
            spawned: Mutex::default(),
            fail_create: false.into(),
        });
        let u = cron_uc(wiki.clone(), sessions.clone(), 1);
        let fired = u.tick().await.unwrap();
        assert!(fired.is_empty(), "файр при полном капе отложен");
        assert!(sessions.spawned.lock().unwrap().is_empty(), "спавна нет");
        assert!(
            !wiki.0.lock().unwrap().contains_key("cron/тест/Журнал"),
            "журнальная строка не пишется — ран не состоялся"
        );
    }

    #[tokio::test]
    async fn cron_fire_deferred_when_scan_fails() {
        // Отказная ветка: union-скан strict-Err (исполнитель недоступен) —
        // файр откладывается до свободного тика, ран не считается
        // состоявшимся (консистентно с конвейером).
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
        let wiki = master_wiki();
        let sessions = Arc::new(FailingSessions);
        let u = CronUseCase {
            wiki,
            sessions: Arc::new(SessionRouter::uniform(sessions)),
            clock: Arc::new(TicksClock),
            models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
            projects: std::collections::HashMap::from([(
                "uwuwu".to_string(),
                "/tmp/checkout".to_string(),
            )]),
            cup: 7,
        };
        let fired = u.tick().await.unwrap();
        assert!(fired.is_empty(), "отказ скана — файр отложен");
    }

    #[tokio::test]
    async fn cron_fire_passes_with_free_slot() {
        // Свободный слот: файр состоявшийся — журнальная строка + спавн.
        let wiki = master_wiki();
        let sessions = Arc::new(MemSessions {
            busy: 0.into(),
            spawned: Mutex::default(),
            fail_create: false.into(),
        });
        let u = cron_uc(wiki.clone(), sessions.clone(), 1);
        let fired = u.tick().await.unwrap();
        assert_eq!(fired.len(), 1, "файр при свободном слоте");
        assert_eq!(sessions.spawned.lock().unwrap().len(), 1);
        let journal = wiki.0.lock().unwrap()["cron/тест/Журнал"].clone();
        assert!(journal.contains("запуск (демон)"), "{journal}");
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
        let u = cron_uc(wiki, sessions.clone(), 1);
        let fired = u.tick().await.unwrap();
        assert_eq!(fired.len(), 1, "второй файр отложен занятым первым слотом");
        assert_eq!(sessions.spawned.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn spawn_failure_isolates_single_cron() {
        // Ревью #1214: сбой спавна одного мастера не обрывает тик —
        // остальные мастера файрятся при свободных слотах. Журнальная
        // строка у несостоявшегося рана уже стоит (append до спавна) —
        // stale-детект поднимет сигнал; Outcome в тик не попадает.
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
        let u = cron_uc(wiki.clone(), sessions, 7);
        let fired = u.tick().await.unwrap();
        assert!(fired.is_empty(), "спавны не удались — outcome нет");
        let w = wiki.0.lock().unwrap();
        assert!(
            w.contains_key("cron/тест-1/Журнал") && w.contains_key("cron/тест-2/Журнал"),
            "журнальные строки у обоих мастеров стоят — тик дошёл до обоих"
        );
    }

    #[tokio::test]
    async fn masters_inventory_filter_and_journal_exclusion() {
        // Инвентарь /cron: журнал рядом с README — не мастер; project —
        // опциональный фильтр (cron_list без проекта — все кроны).
        let mut docs = std::collections::HashMap::new();
        for (name, project) in [("крон-а", "uwuwu"), ("крон-б", "origa")] {
            docs.insert(
                format!("cron/{name}/README"),
                format!("---\ntitle: {name}\nschedule: 1h\nproject: {project}\n---\nТело"),
            );
        }
        docs.insert("cron/крон-а/Журнал".into(), "- журнал".into());
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
        let u = cron_uc(wiki, sessions, 1);
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
