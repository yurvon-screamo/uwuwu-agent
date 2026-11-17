//! Кроны на вики-носителе (#1058): мастер = docs/Projects/<proj>/Кроны/<имя>.md
//! (fm: schedule + agent), журнал = Кроны/<имя>/Журнал.md. Демон дописывает
//! строку запуска, ран-агент — сводку (MCP docs_append). Домен —
//! uwuwu_domain::cron (Schedule/is_due); здесь грамматика строк носителя
//! (parse_schedule_text) и IO.

use crate::error::UsecaseResult;
use crate::naming;
use crate::ports::{Clock, SessionSpec, WikiRepo};
use crate::router::SessionRouter;
use std::sync::Arc;
use uwuwu_domain::{
    civil_from_days, AgentSlot, CronField, CronMaster, CronSchedule, IntervalMinutes, JstDate,
    JstTime, ModelRef, Schedule, ScheduleKind,
};

/// Слэг папки кронов проекта.
pub fn crons_dir(project: &str) -> String {
    format!("Projects/{project}/Кроны")
}

pub struct CronUseCase {
    pub wiki: Arc<dyn WikiRepo>,
    /// Роутер «проект → executor»: крон-ран — гейтвей своего проекта.
    pub sessions: Arc<SessionRouter>,
    pub clock: Arc<dyn Clock>,
    pub spawn_model: Option<ModelRef>,
    pub checkout: String,
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
    /// Инвентарь мастеров проекта (прямые дети папки Кроны).
    pub async fn masters(&self, project: &str) -> UsecaseResult<Vec<CronMaster>> {
        let dir = crons_dir(project);
        let mut out = Vec::new();
        for (slug, md) in self.direct_children(&dir).await? {
            if let Some(m) = parse_master(&slug, &md) {
                out.push(m);
            }
        }
        Ok(out)
    }

    /// Один проход: мастера проекта, due → журнальная строка + спавн рана.
    pub async fn tick(&self, project: &str) -> UsecaseResult<Vec<FireOutcome>> {
        let mut fired = Vec::new();
        // CUP-гвард (#1231): крон-ран — запуск хода, ему нужен свободный
        // слот единого капа. Strict union (#1203): отказ скана — файр
        // откладывается (ран не считается состоявшимся, журнал не пишется).
        let busy = match self.sessions.list_sessions_strict().await {
            Ok(list) => list.iter().filter(|s| s.busy).count(),
            Err(e) => {
                tracing::warn!(
                    event = "cron.list_sessions_failed",
                    error = %e,
                    "Кроны: union-скан недоступен — файр отложен"
                );
                return Ok(fired);
            }
        };
        let dir = crons_dir(project);
        // list_docs даёт пути от docs/ целиком: прямые дети = мастера,
        // подпапки (журнал/ассеты) — нет
        for (slug, md) in self.direct_children(&dir).await? {
            let Some(master) = parse_master(&slug, &md) else {
                continue;
            };
            let name = slug.rsplit('/').next().unwrap_or(&slug).to_string();
            let journal = format!("{dir}/{name}/Журнал");
            let journal_md = self.wiki.read_doc(&journal).await?.unwrap_or_default();
            let now = self.clock.now_ms();
            // Инвариант DS: ран не закрылся = сигнал сбоя — последняя строка
            // журнала «запуск» без сводки старше 2 часов → warn (наблюдаемость)
            if let Some(last) = journal_md.lines().rev().find(|l| !l.trim().is_empty()) {
                if last.contains("запуск (демон)") {
                    let stale = parse_iso_ms(last.split_whitespace().nth(1).unwrap_or_default())
                        .map(|ts| now.saturating_sub(ts) > 2 * 3_600_000)
                        .unwrap_or(false);
                    if stale {
                        tracing::warn!(event = "cron.run_stale", cron = %slug,
                            last = %last.chars().take(60).collect::<String>(),
                            "Крон-ран не закрылся >2ч — сигнал сбоя");
                    }
                }
            }
            if !master.schedule.is_due(now, last_run_ms(&journal_md)) {
                continue;
            }
            if busy >= self.cup {
                tracing::info!(
                    event = "cron.held_cup",
                    cron = %slug,
                    active = busy,
                    cap = self.cup,
                    "Крон-файр отложен: CUP cap — ран не считается состоявшимся"
                );
                continue;
            }
            let stamp = jst_stamp(now);
            self.wiki
                .append_doc(
                    &journal,
                    &format!("- {stamp} запуск (демон)"),
                    &format!("cron: запуск {slug}"),
                )
                .await?;
            let prompt = run_prompt(project, &master, &journal_md);
            let spec = SessionSpec {
                custom_agent: None,
                directory: self.checkout.clone(),
                title: format!("⏰ {} · {}", master.title, stamp),
                agent: master.agent,
                model: self.spawn_model.clone(),
                variant: None,
                bootstrap: false,
            };
            let sessions = self.sessions.project(project);
            let session = sessions.create_session(&spec).await?;
            sessions.prompt(&session, &prompt).await?;
            tracing::info!(event = "cron.fired", cron = %slug, session = %session, "Cron fired");
            fired.push(FireOutcome { slug, session });
        }
        Ok(fired)
    }

    /// Прямые дети папки: (слэг, markdown) без захода в подпапки.
    async fn direct_children(&self, dir: &str) -> UsecaseResult<Vec<(String, String)>> {
        let prefix = format!("{dir}/");
        let mut out = Vec::new();
        for slug in self.wiki.list_docs(dir).await? {
            if !slug.starts_with(&prefix) || slug[prefix.len()..].contains('/') {
                continue;
            }
            if let Some(md) = self.wiki.read_doc(&slug).await? {
                out.push((slug, md));
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

/// Мастер: fm-поля title/schedule/agent + тело; расписание обязано парситься.
pub fn parse_master(slug: &str, md: &str) -> Option<CronMaster> {
    let fm = crate::fm_lite::parse(md);
    let schedule = parse_schedule_text(fm.get("schedule")?)?;
    Some(CronMaster {
        slug: slug.to_string(),
        title: fm.get("title").cloned().unwrap_or_else(|| slug.to_string()),
        schedule,
        agent: AgentSlot::of(fm.get("agent").and_then(|a| naming::parse_agent(a))),
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
    // slug мастера — полный путь от docs/: имя = хвост после последнего '/'
    let name = master.slug.rsplit('/').next().unwrap_or(&master.slug);
    format!(
        "Крон-ран «{title}» (проект {project}).\n\n\
         ## Инструкция мастера\n\n{body}\n\n\
         ## Журнал (последние записи, сверху новые)\n\n{tail}\n\n\
         Выполни ран автономно. Сводку допиши в журнал:\n\
         uwuwu_docs_append {{slug: \"Projects/{project}/Кроны/{name}/Журнал\", line: \"- <итог одной строкой>\"}}.\n\
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
    fn master_requires_valid_schedule() {
        let md = "---\ntitle: Тест\nschedule: 5m\n---\n\nТело";
        let m = parse_master("Projects/uwuwu/Кроны/x", md).unwrap();
        assert_eq!(m.title, "Тест");
        assert_eq!(m.agent, AgentSlot::Default);

        let bad = "---\ntitle: Битый\nschedule: не-расписание\n---\n\nТело";
        assert!(
            parse_master("x", bad).is_none(),
            "битое расписание — не мастер"
        );

        let nofm = "просто текст без fm";
        assert!(parse_master("x", nofm).is_none());
    }

    #[test]
    fn master_agent_parsed() {
        let md = "---\nschedule: 1h\nagent: dreamer\n---\n";
        let m = parse_master("x", md).unwrap();
        assert_eq!(m.agent, AgentSlot::Role(uwuwu_domain::Agent::Dreamer));
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
    }
    #[async_trait::async_trait]
    impl crate::ports::AgentSessionGateway for MemSessions {
        async fn create_session(&self, _spec: &crate::ports::SessionSpec) -> UsecaseResult<String> {
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
            spawn_model: None,
            checkout: "/tmp/checkout".into(),
            cup,
        }
    }

    fn master_wiki() -> Arc<MemWiki> {
        Arc::new(MemWiki(Mutex::new(std::collections::HashMap::from([(
            "Projects/uwuwu/Кроны/тест".into(),
            "---\ntitle: Тест\nschedule: 5m\n---\nТело".into(),
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
        });
        let u = cron_uc(wiki.clone(), sessions.clone(), 1);
        let fired = u.tick("uwuwu").await.unwrap();
        assert!(fired.is_empty(), "файр при полном капе отложен");
        assert!(sessions.spawned.lock().unwrap().is_empty(), "спавна нет");
        assert!(
            !wiki
                .0
                .lock()
                .unwrap()
                .contains_key("Projects/uwuwu/Кроны/тест/Журнал"),
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
            spawn_model: None,
            checkout: "/tmp/checkout".into(),
            cup: 7,
        };
        let fired = u.tick("uwuwu").await.unwrap();
        assert!(fired.is_empty(), "отказ скана — файр отложен");
    }

    #[tokio::test]
    async fn cron_fire_passes_with_free_slot() {
        // Свободный слот: файр состоявшийся — журнальная строка + спавн.
        let wiki = master_wiki();
        let sessions = Arc::new(MemSessions {
            busy: 0.into(),
            spawned: Mutex::default(),
        });
        let u = cron_uc(wiki.clone(), sessions.clone(), 1);
        let fired = u.tick("uwuwu").await.unwrap();
        assert_eq!(fired.len(), 1, "файр при свободном слоте");
        assert_eq!(sessions.spawned.lock().unwrap().len(), 1);
        let journal = wiki.0.lock().unwrap()["Projects/uwuwu/Кроны/тест/Журнал"].clone();
        assert!(journal.contains("запуск (демон)"), "{journal}");
    }
}
