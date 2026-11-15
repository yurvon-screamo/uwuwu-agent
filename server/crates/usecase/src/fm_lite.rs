//! Лёгкий разбор markdown-фронтmatterа и ISO-штампов для юзкейсов
//! (fm-модуль инфры недоступен слою — здесь свой минимальный, без regex).

use std::collections::BTreeMap;

pub fn parse(md: &str) -> BTreeMap<String, String> {
    let rest = match md.strip_prefix("---\n") {
        Some(r) => r,
        None => return BTreeMap::new(),
    };
    let Some(end) = rest.find("\n---") else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for line in rest[..end].lines() {
        if let Some((k, v)) = line.split_once(':') {
            out.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

pub fn strip(md: &str) -> &str {
    let Some(rest) = md.strip_prefix("---\n") else {
        return md;
    };
    let Some(end) = rest.find("\n---") else {
        return md;
    };
    let body = rest[end + 4..].trim_start_matches('\n');
    // хвостовой сепаратор fm-эпохи: «fm\n---\n---\nконтент» — после вырезки
    // блока остаётся одинокий «---» (закрытие + разделитель старого формата)
    body.strip_prefix("---\n")
        .map(|b| b.trim_start_matches('\n'))
        .unwrap_or(body)
}

/// ISO (2026-11-15T06:35:00Z | +09:00 | без зоны) → epoch ms.
pub fn parse_iso_ms(s: &str) -> Option<u64> {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() < 16 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let mo: u32 = s.get(5..7)?.parse().ok()?;
    let d: u32 = s.get(8..10)?.parse().ok()?;
    let hh: u64 = s.get(11..13)?.parse().ok()?;
    let mi: u64 = s.get(14..16)?.parse().ok()?;
    let ss: u64 = s.get(17..19).and_then(|v| v.parse().ok()).unwrap_or(0);
    // дни от эпохи (гражданский календарь)
    let days = days_from_civil(y, mo, d);
    let mut ms = (days as u64 * 86400 + hh * 3600 + mi * 60 + ss) * 1000;
    // зона: Z | +HH:MM | -HH:MM → к UTC
    if let Some(tz) = s.get(19..) {
        let tz = tz.trim();
        if let Some(offset) = tz.strip_prefix('+').or_else(|| tz.strip_prefix('-')) {
            let sign = if tz.starts_with('-') { -1i64 } else { 1 };
            if let Some((oh, om)) = offset.split_once(':') {
                let oh: i64 = oh.parse().unwrap_or(0);
                let om: i64 = om.parse().unwrap_or(0);
                ms = (ms as i64 - sign * (oh * 3600 + om * 60) * 1000) as u64;
            }
        }
    }
    Some(ms)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_strip() {
        let md = "---\ntitle: Ночной QA\nschedule: 1d\nagent: tester\n---\n\n## Инструкция\nтекст";
        let fm = parse(md);
        assert_eq!(fm.get("schedule").map(String::as_str), Some("1d"));
        assert_eq!(fm.get("agent").map(String::as_str), Some("tester"));
        assert!(strip(md).starts_with("## Инструкция"));
        assert!(parse("без fm").is_empty());
    }

    #[test]
    fn iso_jst_stamp() {
        // штамп журнала кронов (jst_stamp)
        let v = parse_iso_ms("2026-11-14T06:00:00+09:00").unwrap();
        // 06:00 JST = 21:00 UTC пред-дня
        assert_eq!(v % 86_400_000, 21 * 3_600_000, "UTC-время суток");
    }

    #[test]
    fn iso_parsing() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0));
        let ms = parse_iso_ms("2026-11-15T06:35:00Z");
        assert!(ms.is_some());
        // зона +09:00 → минус 9ч в UTC
        let a = parse_iso_ms("2026-11-15T06:35:00Z").unwrap();
        let b = parse_iso_ms("2026-11-15T15:35:00+09:00").unwrap();
        assert_eq!(a, b);
        assert_eq!(parse_iso_ms("мусор"), None);
    }

    #[test]
    fn strip_double_separator() {
        // fm-эпоха: закрытие fm + разделитель тела → одинокий «---»
        let md = "---\nstatus: Draft\n---\n---\n\n## Постановка\n";
        assert_eq!(strip(md), "## Постановка\n");
        // обычный fm
        assert_eq!(strip("---\na: 1\n---\nтекст"), "текст");
        // без fm — как есть
        assert_eq!(strip("просто текст"), "просто текст");
    }
}
