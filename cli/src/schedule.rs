//! Грамматика расписаний крон-задач — общий контракт CLI (валидация при
//! записи `--schedule`) и демона uwuwu-cron (исполнение, cron-lib.ts
//! parseSchedule). Тест-кейсы зеркалят TS-набор: расхождение парсеров
//! ловится одинаковыми кейсами на обеих сторонах.

use anyhow::{bail, Result};

/// Валидировать строку расписания: интервал (`5m|2h|1d`), разово
/// (`HH:MM [DD.MM]`) или cron-выражение (5 полей). Причина отказа —
/// человеческая (выводится в CLI при записи).
pub(crate) fn validate_schedule(raw: &str) -> Result<()> {
    let s = raw.trim();
    if parse_interval(s).is_some() {
        return Ok(());
    }
    if parse_at(s).is_some() {
        return Ok(());
    }
    if parse_cron(s).is_some() {
        return Ok(());
    }
    bail!(
        "расписание «{raw}» не разобрано: жди интервал (5m|2h|1d), разово (HH:MM [DD.MM]) \
         или cron из 5 полей — по одному формату за раз"
    )
}

/// `5m|2h|1d` → миллисекунды интервала; n = 0 отвергается (вечный ноль).
pub(crate) fn parse_interval(s: &str) -> Option<u64> {
    // срез по границе символа: не-ascii хвост («к») не должен паниковать
    let unit = s.chars().next_back()?;
    let digits = &s[..s.len() - unit.len_utf8()];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let ms: u64 = match unit {
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => return None,
    };
    let n: u64 = digits.parse().ok()?;
    (n > 0).then_some(n * ms)
}

/// `HH:MM` с опциональной датой `DD.MM` (пробел-разделитель).
pub(crate) type ParsedAt = (u8, u8, Option<(u8, u8)>);

pub(crate) fn parse_at(s: &str) -> Option<ParsedAt> {
    let (time, date) = match s.split_once(' ') {
        Some((t, d)) => (t, Some(d)),
        None => (s, None),
    };
    let (h, m) = time.split_once(':')?;
    let hour: u8 = h.parse().ok()?;
    let minute: u8 = m.parse().ok()?;
    if h.len() > 2 || m.len() != 2 || hour > 23 || minute > 59 {
        return None;
    }
    let day = match date {
        None => None,
        Some(d) => {
            let (dd, mm) = d.split_once('.')?;
            // 1–2 цифры на компонент (паритет с TS-regex \d{1,2}): «001.12»
            // иначе разбирался бы Rust'ом и отвергался демоном
            if dd.len() > 2 || mm.len() > 2 {
                return None;
            }
            let day: u8 = dd.parse().ok()?;
            let month: u8 = mm.parse().ok()?;
            if !(1..=31).contains(&day) || !(1..=12).contains(&month) {
                return None;
            }
            Some((day, month))
        }
    };
    Some((hour, minute, day))
}

/// Границы cron-полей: минута, час, день месяца, месяц, день недели.
const CRON_FIELD_RANGES: [(u32, u32); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 6)];

/// Cron-выражение из 5 полей: каждое — comma-список `* | N | N-M` с
/// опциональным `/step` (шаг ≥ 1: `/0` молча никогда не срабатывает).
pub(crate) fn parse_cron(s: &str) -> Option<Vec<String>> {
    let fields: Vec<&str> = s.split_whitespace().collect();
    if fields.len() != 5 {
        return None;
    }
    fields
        .iter()
        .enumerate()
        .all(|(i, f)| valid_cron_field(f, CRON_FIELD_RANGES[i]))
        .then(|| fields.iter().map(|f| f.to_string()).collect())
}

fn valid_cron_field(field: &str, (min, max): (u32, u32)) -> bool {
    field.split(',').all(|part| {
        let range = match part.split_once('/') {
            // шаг обязателен, ≥ 1 и без ведущих нулей (паритет с TS [1-9]\d*):
            // `/0` и `/01` отвергаются обоими парсерами, чтобы крон не
            // «легально» молчал вечно
            Some((r, step_str)) => {
                if step_str.starts_with('0') || step_str.parse::<u32>().map(|n| n >= 1) != Ok(true)
                {
                    return false;
                }
                r
            }
            None => part,
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else {
            match range.split_once('-') {
                Some((a, b)) => match (a.parse::<u32>(), b.parse::<u32>()) {
                    (Ok(lo), Ok(hi)) => (lo, hi),
                    _ => return false,
                },
                None => match range.parse::<u32>() {
                    Ok(n) => (n, n),
                    Err(_) => return false,
                },
            }
        };
        lo >= min && hi <= max && lo <= hi
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Общий набор кейсов с TS-стороной (cron/cron-lib.test.ts): валидные
    /// и невалидные строки обязаны разбираться одинаково на обеих сторонах.
    #[test]
    fn valid_schedules_pass() {
        for s in [
            "5m",
            "2h",
            "1d",
            "30m",
            "05m",
            "09:30",
            "9:05",
            "09:30 01.12",
            "23:59 31.01",
            "09:30 1.1",
            "*/5 * * * *",
            "0 9 * * 1-5",
            "15,45 */2 * * *",
            "0 0 1 1 *",
            "01 * * * *",
        ] {
            assert!(validate_schedule(s).is_ok(), "«{s}» должен разбираться");
        }
    }

    #[test]
    fn invalid_schedules_rejected() {
        for s in [
            "",
            "90s",
            "5x",
            "0m",
            "  ",
            "25:00",
            "09:5",
            "09:30 32.01",
            "09:30 01.13",
            "09:30 001.12",
            "* * * *",
            "* * * * * *",
            "abc",
            "*/0 * * * *",
            "*/01 * * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 0 *",
            "* * * * 7",
            "5",
            "*/ * * * *",
            "1-5 * * * 9",
        ] {
            assert!(validate_schedule(s).is_err(), "«{s}» должен быть отвергнут");
        }
    }

    #[test]
    fn leading_trailing_whitespace_tolerated() {
        assert!(validate_schedule("  5m  ").is_ok());
        assert!(validate_schedule(" */5 * * * * ").is_ok());
    }
}
