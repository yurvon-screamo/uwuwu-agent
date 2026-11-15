//! Кроны: расписание (value object) и инварианты файра. Грамматика:
//! `5m|2h|1d` — интервал; `HH:MM` — ежедневно (JST); `HH:MM DD.MM` — разово;
//! `m h dom mon dow` — cron-expr (время JST). Чистый домен: ноль зависимостей,
//! время — epoch ms (часы — порт юзкейса).

use crate::agent::Agent;

/// Мастер крона: wiki-док в Projects/<proj>/Кроны/ с валидным расписанием.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronMaster {
    pub slug: String,
    pub title: String,
    pub schedule: Schedule,
    /// Агент рана; None — дефолтная сессия исполнителя.
    pub agent: Option<Agent>,
    /// Тело мастера (промпт рана).
    pub body: String,
}

/// Расписание крона: сырая строка + распарсенный вид за инкапсуляцией.
/// Строится только через `parse` — невалидная строка не даёт Schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    raw: String,
    kind: ScheduleKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScheduleKind {
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
enum CronField {
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

impl Schedule {
    /// Сырая строка расписания (канонический вид из тела мастера).
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Парсинг грамматики расписаний; None — строка не является расписанием.
    pub fn parse(raw: &str) -> Option<Self> {
        let s = raw.trim();
        let kind = Self::parse_kind(s)?;
        Some(Self {
            raw: s.to_string(),
            kind,
        })
    }

    fn parse_kind(s: &str) -> Option<ScheduleKind> {
        if let Some(fields) = parse_cron_expr(s) {
            return Some(ScheduleKind::Cron(fields));
        }
        for (suffix, mult) in [('m', 1u64), ('h', 60), ('d', 1440)] {
            if let Some(num) = s.strip_suffix(suffix).and_then(|n| n.parse::<u64>().ok()) {
                return Some(ScheduleKind::Interval {
                    minutes: num.max(1) * mult,
                });
            }
        }
        let mut parts = s.split_whitespace();
        let time = parts.next()?;
        let day = parts.next();
        let (hh, mm) = time.split_once(':')?;
        let (hh, mm): (u32, u32) = (hh.parse().ok()?, mm.parse().ok()?);
        if hh > 23 || mm > 59 {
            return None;
        }
        match day {
            None => Some(ScheduleKind::Daily { hh, mm }),
            Some(d) => {
                let (dd, mo) = d.split_once('.')?;
                Some(ScheduleKind::Once {
                    hh,
                    mm,
                    day: Some((dd.parse().ok()?, mo.parse().ok()?)),
                })
            }
        }
    }

    /// Настал ли момент файра: `now_ms` против последнего запуска.
    /// `last_run` — epoch ms прошлой строки журнала (None — не запускался).
    pub fn is_due(&self, now_ms: u64, last_run: Option<u64>) -> bool {
        let now = now_ms;
        let last = last_run;
        match &self.kind {
            ScheduleKind::Interval { minutes } => match last {
                // первый файр сразу (reconcile-семантика)
                None => true,
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
                        // следующий день: >20ч с прошлого запуска и время прошло
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
                match day {
                    None => now_minute_of_day >= hh * 60 + mm,
                    Some((d, m)) => {
                        let (d, m) = (*d, *m);
                        d_now == d && mo_now == m && now_minute_of_day >= hh * 60 + mm
                    }
                }
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
fn parse_cron_expr(raw: &str) -> Option<[CronField; 5]> {
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

/// Поля времени JST из epoch ms: (minute, hour, day, month, dow; 0=вс).
pub fn jst_fields(ms: u64) -> (u32, u32, u32, u32, u32) {
    let secs = ms / 1000 + 9 * 3600;
    let dow = ((secs / 86400 + 4) % 7) as u32; // 1970-01-01 = четверг
    let (_y, mo, da) = civil_from_days((secs / 86400) as i64);
    let rem = secs % 86400;
    ((rem / 60 % 60) as u32, (rem / 3600) as u32, da, mo, dow)
}

/// Гражданская дата из дней эпохи (Гаусс; без зависимостей).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
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

/// JST-штамп журнала (ISO с зоной — парсится parse_iso_msfm юзкейса).
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Мс эпохи из UTC-полей (в тестах: 2026-11-14 16:30 UTC = 01:30 JST 15.11).
    fn ms(days_from_epoch: i64, hour: i64, minute: i64) -> u64 {
        ((days_from_epoch * 86400 + hour * 3600 + minute * 60) * 1000) as u64
    }

    /// 2026-11-14 16:30 UTC = 2026-11-15 01:30 JST.
    fn nov14_1630_utc() -> u64 {
        ms(20771, 16, 30)
    }

    #[test]
    fn interval_due_by_elapsed() {
        let s = Schedule::parse("5m").unwrap();
        // запуск был 2026-11-15T02:29 JST = 17:29 UTC 14.11
        let last = ms(20771, 17, 29);
        assert!(s.is_due(ms(20771, 18, 0), Some(last)), "5 минут истекли");
        assert!(!s.is_due(ms(20771, 17, 31), Some(last)), "ещё рано");
        assert!(s.is_due(nov14_1630_utc(), None), "первый файр сразу");
    }

    #[test]
    fn daily_due_next_day() {
        let s = Schedule::parse("06:00").unwrap();
        // вчера 06:00 JST ran (14.11 21:00 UTC) → сегодня 06:00 JST (15.11 21:00 UTC) — due
        let last = ms(20771, 21, 0);
        assert!(s.is_due(ms(20772, 21, 0), Some(last)), "сутки прошли");
        // тот же день раньше времени: 15.11 18:00 UTC = 03:00 JST < 06:00 — нет
        assert!(!s.is_due(ms(20772, 18, 0), Some(last)), "03:00 JST < 06:00");
    }

    #[test]
    fn once_fires_only_without_run() {
        let s = Schedule::parse("06:00 15.11").unwrap();
        // 06:00 JST 15.11 = 21:00 UTC 14.11
        assert!(s.is_due(ms(20771, 21, 0), None), "день настал");
        assert!(!s.is_due(ms(20771, 21, 0), Some(1)), "уже запускался");
    }

    #[test]
    fn cron_expr_window_dedup() {
        let s = Schedule::parse("30 1 * * *").unwrap();
        let now = nov14_1630_utc(); // 01:30 JST 15.11
                                    // запуск 4 мин назад — окно ещё держит → нет
        assert!(!s.is_due(now, Some(now - 4 * 60_000)));
        // запуска не было → файр в окне
        assert!(s.is_due(now, None));
        // запуск 6 мин назад (окно истекло) → новый файр
        assert!(s.is_due(now, Some(now - 6 * 60_000)));
        // вне окна (01:29 JST) — нет
        assert!(!s.is_due(now - 60_000, None));
    }

    #[test]
    fn rejects_garbage() {
        assert!(Schedule::parse("").is_none());
        assert!(Schedule::parse("когда-нибудь").is_none());
        assert!(Schedule::parse("25:00").is_none());
        assert!(Schedule::parse("* * *").is_none());
    }

    #[test]
    fn stamp_is_iso_jst() {
        // 2026-11-15 01:30 JST → ISO-штамп парсится обратно UTC 16:30 пред-дня
        assert_eq!(jst_stamp(nov14_1630_utc()), "2026-11-15T01:30:00+09:00");
    }
}
