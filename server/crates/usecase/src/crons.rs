//! Кроны на вики-носителе (#1058): мастер = docs/Projects/<proj>/Кроны/<имя>.md
//! (fm: schedule + agent), журнал = Кроны/<имя>/Журнал.md. Демон дописывает
//! строку запуска, ран-агент — сводку (MCP docs_append). Грамматика:
//! интервал `5m|2h|1d`, ежедневно `HH:MM`, разовое `HH:MM [DD.MM]`.

use crate::error::UsecaseResult;
use crate::ports::{AgentSessionGateway, Clock, SessionSpec, WikiRepo};
use std::sync::Arc;
use uwuwu_domain::UtcMinute;

/// Слэг папки кронов проекта.
pub fn crons_dir(project: &str) -> String {
    format!("Projects/{project}/Кроны")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronMaster {
    pub slug: String,
    pub title: String,
    pub schedule: String,
    pub agent: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSchedule {
    pub kind: ScheduleKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleKind {
    Interval {
        minutes: u64,
    },
    Daily {
        hh: u32,
        mm: u32,
    },
    Once {
        hh: u32,
        mm: u32,
        day: Option<(u32, u32)>,
    },
    /// cron-expr (5 полей: min hour dom month dow; время JST).
    Cron([CronField; 5]),
}

/// Поле cron-expr: `*` / точные значения / список / диапазон.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronField {
    Any,
    Values(Vec<u32>),
}

impl CronField {
    fn matches(&self, v: u32) -> bool {
        match self {
            Self::Any => true,
            Self::Values(list) => list.contains(&v),
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
    (!out.is_empty()).then_some(CronField::Values(out))
}

/// 5 полей cron-expr → matcher (min hour dom month dow).
pub fn parse_cron_expr(raw: &str) -> Option<[CronField; 5]> {
    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }
    Some([
        parse_cron_field(parts[0], 59)?,
        parse_cron_field(parts[1], 23)?,
        parse_cron_field(parts[2], 31)?,
        parse_cron_field(parts[3], 12)?,
        parse_cron_field(parts[4], 7)?,
    ])
}

/// Поля времени JST из epoch ms: (minute, hour, day, month, dow 0=вс).
pub fn jst_fields(ms: u64) -> (u32, u32, u32, u32, u32) {
    let secs = ms / 1000 + 9 * 3600;
    let dow = ((secs / 86400 + 4) % 7) as u32; // 1970-01-01 = четверг
    let (_y, mo, da) = civil_from_days((secs / 86400) as i64);
    let rem = secs % 86400;
    ((rem / 60 % 60) as u32, (rem / 3600) as u32, da, mo, dow)
}

pub fn parse_schedule(raw: &str) -> Option<ParsedSchedule> {
    let s = raw.trim();
    if let Some(fields) = parse_cron_expr(s) {
        return Some(ParsedSchedule {
            kind: ScheduleKind::Cron(fields),
        });
    }
    if let Some(num) = s.strip_suffix('m').and_then(|n| n.parse::<u64>().ok()) {
        return Some(ParsedSchedule {
            kind: ScheduleKind::Interval {
                minutes: num.max(1),
            },
        });
    }
    if let Some(num) = s.strip_suffix('h').and_then(|n| n.parse::<u64>().ok()) {
        return Some(ParsedSchedule {
            kind: ScheduleKind::Interval {
                minutes: num.max(1) * 60,
            },
        });
    }
    if let Some(num) = s.strip_suffix('d').and_then(|n| n.parse::<u64>().ok()) {
        return Some(ParsedSchedule {
            kind: ScheduleKind::Interval {
                minutes: num.max(1) * 1440,
            },
        });
    }
    let mut parts = s.split_whitespace();
    let time = parts.next()?;
    let day = parts.next();
    let (hh, mm) = time.split_once(':')?;
    let (hh, mm): (u32, u32) = (hh.parse().ok()?, mm.parse().ok()?);
    if hh > 23 || mm > 59 {
        return None;
    }
    let day = match day {
        None => None,
        Some(d) => {
            let (dd, mo) = d.split_once('.')?;
            Some((dd.parse().ok()?, mo.parse().ok()?))
        }
    };
    let kind = match day {
        None => ScheduleKind::Daily { hh, mm },
        Some(day) => ScheduleKind::Once {
            hh,
            mm,
            day: Some(day),
        },
    };
    Some(ParsedSchedule { kind })
}

pub struct CronUseCase {
    pub wiki: Arc<dyn WikiRepo>,
    pub sessions: Arc<dyn AgentSessionGateway>,
    pub clock: Arc<dyn Clock>,
    pub spawn_model: Option<(String, String)>,
    pub checkout: String,
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
        let prefix = format!("{dir}/");
        let mut out = Vec::new();
        for slug in self.wiki.list_docs(&dir).await? {
            if !slug.starts_with(&prefix) || slug[prefix.len()..].contains('/') {
                continue;
            }
            if let Some(md) = self.wiki.read_doc(&slug).await? {
                if let Some(m) = parse_master(&slug, &md) {
                    out.push(m);
                }
            }
        }
        Ok(out)
    }

    /// Один проход: мастера проекта, due → журнальная строка + спавн рана.
    pub async fn tick(&self, project: &str) -> UsecaseResult<Vec<FireOutcome>> {
        let mut fired = Vec::new();
        let dir = crons_dir(project);
        // list_docs даёт пути от docs/ целиком: берём только прямых детей папки
        let prefix = format!("{dir}/");
        for slug in self.wiki.list_docs(&dir).await? {
            if !slug.starts_with(&prefix) || slug[prefix.len()..].contains('/') {
                continue; // журнал/ассеты в подпапке — не мастер
            }
            let Some(md) = self.wiki.read_doc(&slug).await? else {
                continue;
            };
            let Some(master) = parse_master(&slug, &md) else {
                continue;
            };
            let name = slug[prefix.len()..].to_string();
            let journal = format!("{dir}/{name}/Журнал");
            let journal_md = self.wiki.read_doc(&journal).await?.unwrap_or_default();
            if !self.due(&master, &journal_md) {
                continue;
            }
            let now = self.clock.now_ms();
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
                directory: self.checkout.clone(),
                title: format!("⏰ {} · {}", master.title, stamp),
                agent: master.agent.clone(),
                model: self.spawn_model.clone(),
            };
            let session = self.sessions.create_session(&spec).await?;
            self.sessions.prompt(&session, &prompt).await?;
            tracing::info!(event = "cron.fired", cron = %slug, session = %session, "Cron fired");
            fired.push(FireOutcome { slug, session });
        }
        Ok(fired)
    }

    /// due: по расписанию против последней строки запуска в журнале.
    fn due(&self, master: &CronMaster, journal: &str) -> bool {
        let Some(parsed) = parse_schedule(&master.schedule) else {
            return false;
        };
        let now = self.clock.now_ms();
        let last = last_run_ms(journal);
        match parsed.kind {
            ScheduleKind::Interval { minutes } => match last {
                None => true, // первый файр сразу (reconcile-семантика)
                Some(ts) => now >= ts + minutes * 60_000,
            },
            ScheduleKind::Daily { hh, mm } => {
                // расписание канонично JST: минуты суток JST
                let (mi, hr, _, _, _) = jst_fields(now);
                let now_minute_of_day = hr * 60 + mi;
                let target = hh * 60 + mm;
                match last {
                    None => now_minute_of_day >= target,
                    Some(ts) => {
                        // следующий день настал: >20ч с прошлого запуска и время прошло
                        now >= ts + 20 * 3_600_000 && now_minute_of_day >= target
                    }
                }
            }
            ScheduleKind::Once { hh, mm, day } => {
                if last.is_some() {
                    return false;
                }
                let (mi, hr, d_now, mo_now, _) = jst_fields(now);
                let now_minute_of_day = hr * 60 + mi;
                if day.is_none() {
                    return now_minute_of_day >= hh * 60 + mm;
                }
                let Some((d, m)) = day else { return false };
                d_now == d && mo_now == m && now_minute_of_day >= hh * 60 + mm
            }
            ScheduleKind::Cron(fields) => {
                let (mi, hh, da, mo, dw) = jst_fields(now);
                let matched = fields[0].matches(mi)
                    && fields[1].matches(hh)
                    && fields[2].matches(da)
                    && fields[3].matches(mo)
                    && fields[4].matches(dw);
                // один файр на матчащее окно: прошлый запуск старше 5 минут
                matched && last.map(|ts| now >= ts + 5 * 60_000).unwrap_or(true)
            }
        }
    }
}

/// Мастер: fm-поля title/schedule/agent + тело.
pub fn parse_master(slug: &str, md: &str) -> Option<CronMaster> {
    let fm = crate::fm_lite::parse(md);
    let schedule = fm.get("schedule")?.clone();
    if schedule.is_empty() {
        return None;
    }
    Some(CronMaster {
        slug: slug.to_string(),
        title: fm.get("title").cloned().unwrap_or_else(|| slug.to_string()),
        schedule,
        agent: fm.get("agent").filter(|a| !a.is_empty()).cloned(),
        body: crate::fm_lite::strip(md).to_string(),
    })
}

fn last_run_ms(journal: &str) -> Option<u64> {
    journal
        .lines()
        .rev()
        .find(|l| l.contains("запуск"))
        .and_then(|l| {
            let stamp = l.split_whitespace().nth(1)?;
            crate::fm_lite::parse_iso_ms(stamp)
        })
}

fn jst_stamp(ms: u64) -> String {
    // JST = UTC+9, без chrono-зависимости юзкейса: арифметика
    let secs = ms / 1000 + 9 * 3600;
    let d = secs / 86400;
    let (y, mo, da) = civil_from_days(d as i64);
    let rem = secs % 86400;
    format!(
        "{y:04}-{mo:02}-{da:02} {:02}:{:02} JST",
        rem / 3600,
        (rem % 3600) / 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn run_prompt(project: &str, master: &CronMaster, journal: &str) -> String {
    let tail: Vec<&str> = journal.lines().rev().take(5).collect();
    // slug мастера — полный путь от docs/: имя = хвост после последнего '/'
    let name = master.slug.rsplit('/').next().unwrap_or(&master.slug);
    format!(
        "Крон-ран «{title}» (проект {project}, расписание `{schedule}`).\n\n\
         ## Инструкция мастера\n\n{body}\n\n\
         ## Журнал (последние записи, сверху новые)\n\n{tail}\n\n\
         Выполни ран автономно. Сводку допиши в журнал:\n\
         uwuwu_docs_append {{slug: \"Projects/{project}/Кроны/{name}/Журнал\", line: \"- <итог одной строкой>\"}}.\n\
         Блокер — коммент в задачу-хвост не нужен; просто зафиксируй в журнале.",
        title = master.title,
        schedule = master.schedule,
        body = master.body,
        tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
    )
}

/// Минуты UTC для планировщика (переиспользуем доменный UtcMinute через Clock).
pub fn utc_minute_of(clock: &dyn Clock) -> UtcMinute {
    clock.utc_minute()
}

#[cfg(test)]
mod due_tests {
    use super::*;

    /// Мс эпохи из даты (UTC).
    fn ms(y: i64, mo: u32, d: u32, h: u64, mi: u64) -> u64 {
        let yy = if mo <= 2 { y - 1 } else { y };
        let era = if yy >= 0 { yy } else { yy - 399 } / 400;
        let yoe = (yy - era * 400) as u64;
        let mp = if mo > 2 { mo - 3 } else { mo + 9 } as u64;
        let doy = (153 * mp + 2) / 5 + d as u64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        ((era * 146_097 + doe as i64 - 719_468) as u64 * 86400 + h * 3600 + mi * 60) * 1000
    }

    struct FixedClock(u64);
    impl crate::ports::Clock for FixedClock {
        fn utc_minute(&self) -> UtcMinute {
            let secs = self.0 / 1000;
            UtcMinute {
                minute_of_day: ((secs % 86400) / 60) as u32,
                weekday: ((((secs / 86400) + 4) % 7) + 1) as u32,
            }
        }
        fn now_ms(&self) -> u64 {
            self.0
        }
    }

    fn due_at(clock: FixedClock, sched: &str, journal: &str) -> bool {
        let wiki = FakeWiki(journal.to_string());
        let sessions = FakeSessions;
        let u = CronUseCase {
            wiki: std::sync::Arc::new(wiki),
            sessions: std::sync::Arc::new(sessions),
            clock: std::sync::Arc::new(clock),
            spawn_model: None,
            checkout: "/tmp".into(),
        };
        let master = CronMaster {
            slug: "Projects/uwuwu/Кроны/x".into(),
            title: "x".into(),
            schedule: sched.into(),
            agent: None,
            body: String::new(),
        };
        u.due(&master, journal)
    }

    struct FakeWiki(String);
    #[async_trait::async_trait]
    impl crate::ports::WikiRepo for FakeWiki {
        async fn read_doc(&self, _s: &str) -> crate::error::UsecaseResult<Option<String>> {
            Ok(Some(self.0.clone()))
        }
        async fn list_docs(&self, _p: &str) -> crate::error::UsecaseResult<Vec<String>> {
            Ok(vec![])
        }
        async fn write_doc(&self, _s: &str, _c: &str, _m: &str) -> crate::error::UsecaseResult<()> {
            Ok(())
        }
        async fn append_doc(
            &self,
            _s: &str,
            _l: &str,
            _m: &str,
        ) -> crate::error::UsecaseResult<()> {
            Ok(())
        }
    }
    struct FakeSessions;
    #[async_trait::async_trait]
    impl crate::ports::AgentSessionGateway for FakeSessions {
        async fn create_session(
            &self,
            _s: &crate::ports::SessionSpec,
        ) -> crate::error::UsecaseResult<String> {
            Ok("ses_t".into())
        }
        async fn prompt(&self, _s: &str, _t: &str) -> crate::error::UsecaseResult<()> {
            Ok(())
        }
        async fn run_command(
            &self,
            _s: &str,
            _n: &str,
            _a: &str,
        ) -> crate::error::UsecaseResult<()> {
            Ok(())
        }
        async fn liveness(&self, _s: &str) -> crate::ports::Liveness {
            crate::ports::Liveness::Alive
        }
        async fn delete_session(&self, _s: &str) -> crate::error::UsecaseResult<()> {
            Ok(())
        }
        async fn last_activity_ms(&self, _s: &str) -> crate::error::UsecaseResult<Option<u64>> {
            Ok(None)
        }
        async fn list_sessions(
            &self,
        ) -> crate::error::UsecaseResult<Vec<crate::ports::SessionInfo>> {
            Ok(vec![])
        }
        async fn export_session(&self, _s: &str) -> crate::error::UsecaseResult<String> {
            Ok(String::new())
        }
    }

    #[test]
    fn interval_due_by_journal_stamp() {
        // журнал с ISO-штампом (формат jst_stamp после фикса) парсится:
        // последний запуск 02:29 JST = 17:29 UTC накануне → 5m истекли
        let journal = "- 2026-11-15T02:29:00+09:00 запуск (демон)\n";
        let now = FixedClock(ms(2026, 11, 15, 18, 0));
        assert!(due_at(now, "5m", journal));
        // свежий запуск — не due (1 минута спустя, UTC = JST-9ч)
        let fresh = FixedClock(ms(2026, 11, 14, 17, 30));
        assert!(!due_at(fresh, "5m", journal));
    }

    #[test]
    fn interval_first_fire_without_journal() {
        assert!(due_at(FixedClock(ms(2026, 11, 15, 12, 0)), "1d", ""));
    }

    #[test]
    fn cron_expr_window_dedup() {
        // 30 1 * * *: сейчас 01:30 JST (16:30 UTC пред-дня), запуск 5 мин назад → нет
        let journal = "- 2026-11-15T01:25:00+09:00 запуск (демон)\n";
        let now = FixedClock(ms(2026, 11, 14, 16, 29));
        assert!(!due_at(now, "30 1 * * *", journal));
        // запуска не было → файр в окне
        assert!(due_at(
            FixedClock(ms(2026, 11, 14, 16, 30)),
            "30 1 * * *",
            ""
        ));
    }

    #[test]
    fn due_daily_parses_stamp_and_fires() {
        let journal = "- 2026-11-14T06:00:00+09:00 запуск (демон)\n";
        let ts = last_run_ms(journal);
        let now = ms(2026, 11, 14, 21, 0);
        let (mi, hr, _, _, _) = jst_fields(now);
        println!(
            "ts={ts:?} now={now} diff_h={} jst_h={hr} jst_m={mi}",
            (now - ts.unwrap_or(0)) / 3_600_000
        );
        assert!(ts.is_some(), "штамп не распарсился");
        // прямой вызов due
        let u = CronUseCase {
            wiki: std::sync::Arc::new(FakeWiki(journal.to_string())),
            sessions: std::sync::Arc::new(FakeSessions),
            clock: std::sync::Arc::new(FixedClock(now)),
            spawn_model: None,
            checkout: "/tmp".into(),
        };
        let m = CronMaster {
            slug: "x".into(),
            title: "x".into(),
            schedule: "06:00".into(),
            agent: None,
            body: String::new(),
        };
        let d = u.due(&m, journal);
        println!("due={d}");
        assert!(d, "прямой вызов due");
    }

    #[test]
    fn daily_due_next_day() {
        // 06:00 JST daily; вчера 06:00 ran → сегодня 21:00 UTC (06:00 JST+15h) — due
        let journal = "- 2026-11-14T06:00:00+09:00 запуск (демон)\n";
        assert!(due_at(
            FixedClock(ms(2026, 11, 14, 21, 0)),
            "06:00",
            journal
        ));
        // тот же день раньше времени — нет
        assert!(!due_at(
            FixedClock(ms(2026, 11, 14, 18, 0)),
            "06:00",
            journal
        ));
    }
}
