//! Запрет записи в снесённые журналы кронов (#1480, #1576).
//!
//! Журналы-доки `cron/<имя>/Журнал` снесены как writable-носитель: запись
//! в них воскрешала бы мёртвый ящик через create-on-missing `docs_append`
//! (#1353). Правило «тихий ран ничего не пишет» живёт не только в мастерах —
//! мутационные тулы вики отклоняют этот путь громкой ошибкой.

/// Отказ для снесённого журнала крона; остальные слаги проходят.
pub fn refuse(slug: &str) -> Result<(), String> {
    let bare = slug.strip_suffix(".md").unwrap_or(slug);
    let mut parts = bare.split('/');
    let root = parts.next();
    let name = parts.next();
    let last = parts.next();
    if root == Some("cron")
        && name.is_some_and(|s| !s.is_empty())
        && last == Some("Журнал")
        && parts.next().is_none()
    {
        return Err(format!(
            "removed cron journal «{slug}»: writing refused (#1480, #1576) — run results live in tasks or the target carrier"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::refuse;

    #[test]
    fn refuses_removed_journal_slug() {
        let err = refuse("cron/Loop-надёжность-конвейера/Журнал").expect_err("отказ");
        assert!(err.contains("writing refused"));
        assert!(err.contains("#1576"));
    }

    #[test]
    fn refuses_slug_with_md_extension() {
        assert!(refuse("cron/Sentry/Журнал.md").is_err());
    }

    #[test]
    fn keeps_other_wiki_slugs_writable() {
        for slug in [
            "cron/Loop-надёжность-конвейера/README",
            "cron/Loop-надёжность-конвейера/Журнал-старый",
            "cron/Журнал",
            "cron/X/Журнал/часть",
            "Experience/platform/ops-mechanics",
        ] {
            assert!(refuse(slug).is_ok(), "слаг не журнал: {slug}");
        }
    }
}
