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
    Some(ParsedSchedule {
        kind: ScheduleKind::Once { hh, mm, day },
    })
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
    /// Один проход: мастера проекта, due → журнальная строка + спавн рана.
    pub async fn tick(&self, project: &str) -> UsecaseResult<Vec<FireOutcome>> {
        let mut fired = Vec::new();
        let dir = crons_dir(project);
        for slug in self.wiki.list_docs(&dir).await? {
            let path = format!("{dir}/{slug}");
            let Some(md) = self.wiki.read_doc(&path).await? else {
                continue;
            };
            let Some(master) = parse_master(&slug, &md) else {
                continue;
            };
            let journal = format!("{dir}/{slug}/Журнал");
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
                let now_min = self.clock.utc_minute();
                let target = hh * 60 + mm;
                match last {
                    None => now_min.minute_of_day >= target,
                    Some(ts) => {
                        // следующий день настал: >20ч с прошлого запуска и время прошло
                        now >= ts + 20 * 3_600_000 && now_min.minute_of_day >= target
                    }
                }
            }
            ScheduleKind::Once { hh, mm, day } => {
                if last.is_some() {
                    return false;
                }
                let now_min = self.clock.utc_minute();
                if day.is_none() {
                    return now_min.minute_of_day >= hh * 60 + mm;
                }
                let Some((d, m)) = day else { return false };
                let (day_now, month_now) = jst_day(now);
                day_now == d && month_now == m && now_min.minute_of_day >= hh * 60 + mm
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

fn jst_day(ms: u64) -> (u32, u32) {
    let secs = ms / 1000 + 9 * 3600;
    let (_, mo, da) = civil_from_days((secs / 86400) as i64);
    (da, mo)
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
    format!(
        "Крон-ран «{title}» (проект {project}, расписание `{schedule}`).\n\n\
         ## Инструкция мастера\n\n{body}\n\n\
         ## Журнал (последние записи, сверху новые)\n\n{tail}\n\n\
         Выполни ран автономно. Сводку допиши в журнал:\n\
         uwuwu_docs_append {{slug: \"Projects/{project}/Кроны/{slug}/Журнал\", line: \"- <итог одной строкой>\"}}.\n\
         Блокер — коммент в задачу-хвост не нужен; просто зафиксируй в журнале.",
        title = master.title,
        schedule = master.schedule,
        slug = master.slug,
        body = master.body,
        tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
    )
}

/// Минуты UTC для планировщика (переиспользуем доменный UtcMinute через Clock).
pub fn utc_minute_of(clock: &dyn Clock) -> UtcMinute {
    clock.utc_minute()
}
