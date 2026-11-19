//! Кроны: расписание (value object) и инварианты файра. Чистый домен:
//! ноль зависимостей, время — epoch ms; грамматика строк носителя и
//! рендер штампов журнала — юзкейс/инфра (crons.rs, naming).

use crate::agent::AgentSlot;
/// Мастер крона: вики-док `cron/<имя>/README.md` с валидным расписанием.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronMaster {
    pub slug: String,
    pub title: String,
    pub schedule: Schedule,
    /// Агент рана: дефолтная сессия или роль конвейера.
    pub agent: AgentSlot,
    /// fm `project` — ссылка на [[project]] конфига сервера: роутинг
    /// executor и checkout крон-рана (#1214, плоская раскладка /cron).
    pub project: String,
    /// Тело мастера (промпт рана).
    pub body: String,
}

/// Расписание крона: распарсенный вид за инкапсуляцией; строится
/// парсером юзкейс-слоя (Schedule::new — валидированные value-типы).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    kind: ScheduleKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleKind {
    Interval(IntervalMinutes),
    Daily(JstTime),
    Once {
        at: JstTime,
        on: JstDate,
    },
    /// cron-expr (время JST).
    Cron(CronSchedule),
}

impl Schedule {
    pub fn new(kind: ScheduleKind) -> Self {
        Self { kind }
    }

    /// Вид расписания (рендер — naming юзкейс-слоя).
    pub fn kind(&self) -> &ScheduleKind {
        &self.kind
    }

    /// Настал ли момент файра: `now_ms` против последнего запуска.
    /// `last_run` — epoch ms прошлой строки журнала (None — не запускался).
    pub fn is_due(&self, now_ms: u64, last_run: Option<u64>) -> bool {
        let now = now_ms;
        let last = last_run;
        match &self.kind {
            ScheduleKind::Interval(minutes) => match last {
                // первый файр сразу (reconcile-семантика)
                None => true,
                Some(ts) => now >= ts + minutes.get() * 60_000,
            },
            ScheduleKind::Daily(at) => {
                // расписание канонично JST: минуты суток JST
                let (mi, hr, _, _, _) = jst_fields(now);
                let now_minute_of_day = hr * 60 + mi;
                let target = at.minute_of_day();
                match last {
                    None => now_minute_of_day >= target,
                    Some(ts) => {
                        // следующий день: >20ч с прошлого запуска и время прошло
                        now >= ts + 20 * 3_600_000 && now_minute_of_day >= target
                    }
                }
            }
            ScheduleKind::Once { at, on } => {
                if last.is_some() {
                    return false;
                }
                let (mi, hr, d_now, mo_now, _) = jst_fields(now);
                let now_minute_of_day = hr * 60 + mi;
                d_now == on.day && mo_now == on.month && now_minute_of_day >= at.minute_of_day()
            }
            ScheduleKind::Cron(cron) => {
                // Дрейф-иммунный матч: последняя матчащая минута в окне
                // [now − 24ч, now] (тик ~3 мин — минута матча между тиками
                // не теряется). Файр = эта минута новее прошлого запуска.
                let step = 60_000;
                let horizon = now.saturating_sub(24 * 3_600_000);
                let mut probe = now;
                while probe >= horizon {
                    if cron.matches_ms(probe) {
                        return last.map(|ts| probe > ts).unwrap_or(true);
                    }
                    probe -= step;
                }
                false
            }
        }
    }
}

/// Положительный интервал в минутах (>0 — инвариант).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntervalMinutes(u64);

impl IntervalMinutes {
    pub fn new(minutes: u64) -> Option<Self> {
        (minutes > 0).then_some(Self(minutes))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// Время суток JST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JstTime {
    hour: u32,
    minute: u32,
}

impl JstTime {
    pub fn new(hour: u32, minute: u32) -> Option<Self> {
        (hour < 24 && minute < 60).then_some(Self { hour, minute })
    }

    pub fn minute_of_day(self) -> u32 {
        self.hour * 60 + self.minute
    }
}

/// Дата JST (день.месяц, без года — грамматика `HH:MM DD.MM`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JstDate {
    day: u32,
    month: u32,
}

impl JstDate {
    /// (day, month) для рендера.
    pub fn parts(&self) -> (u32, u32) {
        (self.day, self.month)
    }

    pub fn new(day: u32, month: u32) -> Option<Self> {
        let max_day = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => 29,
            _ => return None,
        };
        ((1..=max_day).contains(&day) && (1..=12).contains(&month)).then_some(Self { day, month })
    }
}

/// cron-expr: пять полей (min hour dom month dow; время JST).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronSchedule {
    minute: CronField,
    hour: CronField,
    day_of_month: CronField,
    month: CronField,
    day_of_week: CronField,
}

impl CronSchedule {
    pub fn new(
        minute: CronField,
        hour: CronField,
        day_of_month: CronField,
        month: CronField,
        day_of_week: CronField,
    ) -> Self {
        Self {
            minute,
            hour,
            day_of_month,
            month,
            day_of_week,
        }
    }

    fn matches_ms(&self, ms: u64) -> bool {
        let (mi, hh, da, mo, dw) = jst_fields(ms);
        self.minute.matches(mi)
            && self.hour.matches(hh)
            && self.day_of_month.matches(da)
            && self.month.matches(mo)
            && self.day_of_week.matches(dw)
    }
}

/// Поле cron-expr: `*` или явный список значений.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronField {
    Any,
    List(CronValues),
}

impl CronField {
    pub fn list(values: Vec<u32>) -> Self {
        Self::List(CronValues(values))
    }

    fn matches(&self, v: u32) -> bool {
        match self {
            Self::Any => true,
            Self::List(values) => values.contains(v),
        }
    }
}

/// Список значений поля cron.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronValues(Vec<u32>);

impl CronValues {
    pub fn new(values: Vec<u32>) -> Self {
        Self(values)
    }

    fn contains(&self, v: u32) -> bool {
        self.0.contains(&v)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Мс эпохи из UTC-полей.
    fn ms(days_from_epoch: i64, hour: i64, minute: i64) -> u64 {
        ((days_from_epoch * 86400 + hour * 3600 + minute * 60) * 1000) as u64
    }

    /// 2026-11-14 16:30 UTC = 2026-11-15 01:30 JST.
    fn nov14_1630_utc() -> u64 {
        ms(20771, 16, 30)
    }

    #[test]
    fn interval_due_by_elapsed() {
        let s = Schedule::new(ScheduleKind::Interval(IntervalMinutes::new(5).unwrap()));
        let last = ms(20771, 17, 29);
        assert!(s.is_due(ms(20771, 18, 0), Some(last)), "5 минут истекли");
        assert!(!s.is_due(ms(20771, 17, 31), Some(last)), "ещё рано");
        assert!(s.is_due(nov14_1630_utc(), None), "первый файр сразу");
    }

    #[test]
    fn daily_due_next_day() {
        let s = Schedule::new(ScheduleKind::Daily(JstTime::new(6, 0).unwrap()));
        let last = ms(20771, 21, 0);
        assert!(s.is_due(ms(20772, 21, 0), Some(last)), "сутки прошли");
        assert!(!s.is_due(ms(20772, 18, 0), Some(last)), "03:00 JST < 06:00");
    }

    #[test]
    fn once_fires_only_without_run() {
        let s = Schedule::new(ScheduleKind::Once {
            at: JstTime::new(6, 0).unwrap(),
            on: JstDate::new(15, 11).unwrap(),
        });
        assert!(s.is_due(ms(20771, 21, 0), None), "день настал");
        assert!(!s.is_due(ms(20771, 21, 0), Some(1)), "уже запускался");
    }

    #[test]
    fn cron_expr_window_dedup() {
        let s = Schedule::new(ScheduleKind::Cron(CronSchedule::new(
            CronField::list(vec![30]),
            CronField::list(vec![1]),
            CronField::Any,
            CronField::Any,
            CronField::Any,
        )));
        // тик проскочил матч-минуту: сейчас 01:33 JST, матч был в 01:30 —
        // окно догона файрит (старая семантика «ровно в минуту» теряла его)
        let now = ms(20771, 16, 33);
        let yesterday = now - 24 * 3_600_000;
        assert!(s.is_due(now, None), "матч был, запуска не было");
        assert!(
            s.is_due(now, Some(yesterday)),
            "вчерашний запуск — файрим сегодня"
        );
        assert!(
            !s.is_due(now, Some(ms(20771, 16, 31))),
            "01:31 JST — после матча"
        );
        assert!(
            !s.is_due(now, Some(ms(20771, 16, 30))),
            "01:30 JST — сам матч"
        );
        assert!(!s.is_due(ms(20771, 16, 29), Some(yesterday)));
    }

    #[test]
    fn value_types_hold_invariants() {
        assert!(IntervalMinutes::new(0).is_none());
        assert!(JstTime::new(24, 0).is_none());
        assert!(JstTime::new(6, 60).is_none());
        assert!(JstDate::new(31, 4).is_none(), "апрель — 30 дней");
        assert!(JstDate::new(29, 2).is_some(), "февраль — до 29");
        assert!(JstDate::new(1, 13).is_none());
    }
}
