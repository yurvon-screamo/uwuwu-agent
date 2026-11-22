//! fm-гвард docs_write (#1444): шапка вики-дока переживает replace.
//! Три ветки контракта: вход с fm — дословно; шапка существующего дока
//! была, во входе нет — сырой блок переносится (lastmod := now); шапки
//! нет нигде — каноническая. Композитор-агент опускает шапку при сборке
//! replace-контента — гвард возвращает её на носитель (класс «молчаливой
//! порчи» #586/#1353; граница с ADR-105: fm тел задач — не здесь).

/// fm-блок дока: сырой текст между делимитерами + тело после шапки.
/// Предикат «док несёт fm»: первая строка ровно `---`, закрывающая
/// ровно `---` (не префикс вида `----`), в блоке ≥ 1 строки `key: value`
/// — thematic break и бессодержательный блок fm-ом не считаются.
pub fn split(md: &str) -> Option<(&str, &str)> {
    let rest = md.strip_prefix("---\n")?;
    let end = closing_delimiter(rest)?;
    has_key_value(&rest[..end]).then_some((&rest[..end], &rest[end + 4..]))
}

/// Закрывающая строка ровно `---`: `\n---\n` либо `\n---` в конце файла.
fn closing_delimiter(rest: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(off) = rest[from..].find("\n---") {
        let at = from + off;
        let tail = &rest[at + 4..];
        if tail.is_empty() || tail.starts_with('\n') {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

fn has_key_value(fm: &str) -> bool {
    fm.lines().any(|line| {
        line.split_once(':')
            .is_some_and(|(k, _)| !k.trim().is_empty())
    })
}

/// Итог гварда — честный ответ тула.
#[derive(Debug, PartialEq, Eq)]
pub enum FmGuard {
    /// Вход нёс fm — записан дословно.
    Verbatim,
    /// Шапка перенесена, lastmod освежён/дописан.
    Carried,
    /// Каноническая шапка создана.
    Created,
}

/// Ветвление #1444 по предикатам входа и существующего дока.
pub fn guard(existing: Option<&str>, incoming: &str, slug: &str, now: &str) -> (String, FmGuard) {
    if split(incoming).is_some() {
        return (incoming.to_string(), FmGuard::Verbatim);
    }
    if let Some((fm, _)) = existing.and_then(split) {
        return (carry(fm, incoming, now), FmGuard::Carried);
    }
    (canonical(slug, incoming, now), FmGuard::Created)
}

/// Ветка Carried: сырой fm-блок байт-в-байт (ключи, порядок,
/// форматирование — нестандартные ключи крон-мастеров выживают),
/// lastmod := now: значение заменяется, при отсутствии ключа —
/// дописывается последней строкой блока (C-2: крон-мастера и бот-доки
/// несут fm без lastmod).
pub fn carry(existing_fm: &str, incoming_body: &str, now: &str) -> String {
    let stamp = format!("lastmod: {now}");
    let mut out = String::from("---\n");
    let mut stamped = false;
    for line in existing_fm.lines() {
        if line.trim_start().starts_with("lastmod:") {
            out.push_str(&stamp);
            stamped = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !stamped {
        out.push_str(&stamp);
        out.push('\n');
    }
    out.push_str("---\n");
    push_body(&mut out, incoming_body);
    out
}

/// Ветка Created: `title` — хвост слага; значение с «:» — в двойных
/// кавычках (C-3: хвост слага нового ADR по канону #1225 содержит «: »,
/// голая сериализация рвёт YAML-рендер), `date` = `lastmod` = now.
pub fn canonical(slug: &str, body: &str, now: &str) -> String {
    let tail = slug.rsplit('/').next().unwrap_or(slug);
    let mut out = format!(
        "---\ntitle: {}\ndate: {now}\nlastmod: {now}\n---\n",
        yaml_scalar(tail)
    );
    push_body(&mut out, body);
    out
}

/// YAML plain-скаляр; значения с «:» — двойные кавычки с экранированием
/// (корпусный прецедент ADR-класса).
fn yaml_scalar(value: &str) -> String {
    if value.contains(':') {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.to_string()
    }
}

/// Тело после закрывающего делимитера: одна пустая строка перед телом,
/// завершающий перенос — корпусный формат шапки.
fn push_body(out: &mut String, body: &str) {
    let body = body.trim_start_matches('\n');
    if body.is_empty() {
        return;
    }
    out.push('\n');
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_doc_with_fm_and_body() {
        let md = "---\ntitle: X\nlastmod: 2026-11-20T20:50:00+09:00\n---\n\n# Заголовок\n";
        let (fm, body) = split(md).expect("fm");
        assert_eq!(fm, "title: X\nlastmod: 2026-11-20T20:50:00+09:00");
        // тело с закрытием делимитера и пустой строкой — потребители
        // нормализуют (push_body тримит ведущие переносы)
        assert_eq!(body, "\n\n# Заголовок\n");
    }

    #[test]
    fn split_rejects_thematic_break_and_keyless_block() {
        // тематический break первой строкой — не fm (L-1)
        assert!(split("---\n\n# Заголовок\n").is_none());
        // блок без key:value — не fm
        assert!(split("---\nпросто текст\n---\nтело\n").is_none());
    }

    #[test]
    fn split_rejects_dash_prefix_closing_and_accepts_eof_closing() {
        // «----» не закрывает блок (строгость делимитера)
        assert!(split("---\na: 1\n----\nтело\n").is_none());
        // закрытие в конце файла без завершающего переноса
        let (fm, body) = split("---\na: 1\n---").expect("fm");
        assert_eq!(fm, "a: 1");
        assert_eq!(body, "");
    }

    #[test]
    fn guard_verbatim_when_payload_has_fm() {
        let payload = "---\ntitle: x\n---\n\nтело\n";
        let (out, verdict) = guard(
            Some("---\ntitle: старый\n---\n\nстарое тело\n"),
            payload,
            "s",
            "N",
        );
        assert_eq!(verdict, FmGuard::Verbatim);
        assert_eq!(out, payload);
    }

    #[test]
    fn guard_carries_existing_header_without_fm_in_payload() {
        let existing = "---\ntitle: x\ndate: D\nlastmod: L\n---\n\nстарое тело\n";
        let (out, verdict) = guard(Some(existing), "# новое тело\n", "s", "N");
        assert_eq!(verdict, FmGuard::Carried);
        assert_eq!(
            out,
            "---\ntitle: x\ndate: D\nlastmod: N\n---\n\n# новое тело\n"
        );
    }

    #[test]
    fn guard_creates_canonical_when_no_header_anywhere() {
        let (out, verdict) = guard(None, "# тело\n", "a/b/Имя", "N");
        assert_eq!(verdict, FmGuard::Created);
        assert_eq!(out, "---\ntitle: Имя\ndate: N\nlastmod: N\n---\n\n# тело\n");
    }

    #[test]
    fn carry_appends_lastmod_as_last_block_line_when_missing() {
        let fm =
            "title: QA: ночной аудит\nschedule: 30 1 * * *\nagent: tester\nmigrated-from: '#114'";
        let out = carry(fm, "## Инструкция\n", "2026-11-23T10:00:00+09:00");
        assert!(
            out.contains("migrated-from: '#114'\nlastmod: 2026-11-23T10:00:00+09:00\n---\n"),
            "{out}"
        );
    }

    #[test]
    fn canonical_quotes_title_with_colon() {
        let out = canonical("Projects/uwuwu/ADR/ADR-x: Решение", "# текст\n", "N");
        assert!(
            out.starts_with("---\ntitle: \"ADR-x: Решение\"\ndate: N\nlastmod: N\n---\n"),
            "{out}"
        );
    }
}
