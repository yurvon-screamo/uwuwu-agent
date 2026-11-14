//! Разовая миграция клеток: fm-шапка тел → глобальные issue fields
//! (ADR-098, задача 1087). One-shot: после --apply и валидации команда
//! становится no-op (fm нет — нечего мигрировать).
//!
//! Правило приоритета (ревью Common-5): непустое значение поля выигрывает
//! у fm (поле — канон-носитель, «Запланировано» пишется туда с эпохи #258);
//! fm — фолбэк только при пустом поле; fm ≠ поле при обоих непустых — лог
//! расхождения без перезаписи. Status: Pv2 не трогаем (канон там), fm ≠
//! Pv2 — лог.
//!
//! Очистка fm-шапки: тело PATCH'ается без блока (rest_md); идемпотентно —
//! второй проход fm не находит.

use crate::store::github_issue_fields::{self, names, FieldValue};
use crate::store::github_gh::gh_json;
use crate::store::github_task::GitHubStore;
use crate::store::task_store::TaskStore;
use anyhow::{Context, Result};
use std::collections::HashMap;

/// fm-парсер legacy-шапки (`---\nkey: value\n…\n---`) — последняя обязанность
/// снесённого носителя: разовая миграция читает старые тела.
pub(crate) struct LegacyFm {
    pub fields: HashMap<String, String>,
    pub rest_md: String,
}

pub(crate) fn parse_legacy_fm(body_md: &str) -> LegacyFm {
    let mut fields = HashMap::new();
    let mut rest_md = body_md.to_string();
    if let Some(after_open) = body_md.strip_prefix("---\n") {
        if let Some(end) = after_open.find("\n---\n") {
            for line in after_open[..end].lines() {
                if let Some((k, v)) = line.split_once(": ") {
                    fields.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
            rest_md = after_open[end + 5..].to_string();
        }
    }
    LegacyFm { fields, rest_md }
}

/// fm-значение → действие над полем. `current` — значение поля из payload
/// ("" — поле пусто): непустое поле-канон выигрывает всегда (согласовано
/// или расхождение — расхождение логируется снаружи), fm — запись только
/// в пустое поле.
pub(crate) enum CellAction {
    Write,
    Skip,
}

pub(crate) fn cell_action(fm_value: &str, current: &str) -> CellAction {
    if fm_value.is_empty() || !current.is_empty() {
        CellAction::Skip
    } else {
        CellAction::Write
    }
}

/// fm-карта → батч клеток задачи (не-Status). Спец-маппинги: autodone
/// true/false/✓ → on/off; priority — select как есть; planned — date
/// (нормализация); прочие — text. Носители мимо: status (Pv2-канон),
/// type (нативный issue type), custom-agent (реликт, снос без записи).
pub(crate) fn fm_to_cells(
    fm: &HashMap<String, String>,
    current_fields: &HashMap<String, String>,
) -> Result<Vec<(&'static str, Option<FieldValue>)>> {
    let mut cells: Vec<(&'static str, Option<FieldValue>)> = Vec::new();
    let push = |name: &'static str,
                fm_value: Option<&String>,
                make: fn(&str) -> Result<FieldValue>,
                cells: &mut Vec<(&'static str, Option<FieldValue>)>|
     -> Result<()> {
        let Some(v) = fm_value else {
            return Ok(());
        };
        match cell_action(
            v,
            current_fields.get(name).map(String::as_str).unwrap_or(""),
        ) {
            CellAction::Skip => Ok(()),
            CellAction::Write => {
                cells.push((name, Some(make(v)?)));
                Ok(())
            }
        }
    };
    push(
        names::PRIORITY,
        fm.get("priority"),
        |v| Ok(FieldValue::Select(v.to_string())),
        &mut cells,
    )?;
    let autodone = fm.get("autodone").map(|v| match v.as_str() {
        "true" | "✓" | "on" => "on".to_string(),
        _ => "off".to_string(),
    });
    push(
        names::AUTODONE,
        autodone.as_ref(),
        |v| Ok(FieldValue::Select(v.to_string())),
        &mut cells,
    )?;
    push(
        names::BRANCH,
        fm.get("branch"),
        |v| Ok(FieldValue::Text(v.to_string())),
        &mut cells,
    )?;
    push(
        names::PR,
        fm.get("pr"),
        |v| Ok(FieldValue::Text(v.to_string())),
        &mut cells,
    )?;
    push(
        names::DEV_SESSION,
        fm.get("dev-session"),
        |v| Ok(FieldValue::Text(v.to_string())),
        &mut cells,
    )?;
    push(
        names::SCHEDULE,
        fm.get("schedule"),
        |v| Ok(FieldValue::Text(v.to_string())),
        &mut cells,
    )?;
    push(
        names::PLANNED,
        fm.get("planned"),
        |v| {
            Ok(FieldValue::Date(
                crate::store::github_project::normalize_date(v)?,
            ))
        },
        &mut cells,
    )?;
    Ok(cells)
}

/// Итог миграции одной строки.
pub(crate) struct RowOutcome {
    pub number: String,
    pub action: &'static str, // "migrated" | "skipped" | "conflict"
    pub detail: String,
}

/// Миграция одной строки: fm-клетки → поля, снос шапки. apply=false —
/// dry-run. Итог — один RowOutcome (conflict-лог — отдельным исходом,
/// миграция при этом выполняется).
async fn migrate_row(
    store: &GitHubStore,
    task: &crate::store::Task,
    apply: bool,
) -> Result<RowOutcome> {
    let number = task.id.clone();
    let issue = gh_json(
        &["api", &format!("repos/{}/issues/{number}", store.repo)],
        None,
    )
    .await
    .with_context(|| format!("#{number}: issue не прочитан"))?;
    let fm = parse_legacy_fm(issue["body"].as_str().unwrap_or(""));
    if fm.fields.is_empty() {
        return Ok(RowOutcome {
            number,
            action: "skipped",
            detail: "fm-шапки нет".into(),
        });
    }
    let current = github_issue_fields::fields_of(&issue);
    let cells = fm_to_cells(&fm.fields, &current)?;
    let mut outcome = Vec::new();
    // расхождения fm vs поле-канон — лог без перезаписи
    for (key, field_name) in [
        ("priority", names::PRIORITY),
        ("branch", names::BRANCH),
        ("pr", names::PR),
        ("dev-session", names::DEV_SESSION),
        ("schedule", names::SCHEDULE),
        ("planned", names::PLANNED),
    ] {
        if let (Some(fm_v), Some(cur)) = (fm.fields.get(key), current.get(field_name)) {
            if !fm_v.is_empty() && fm_v != cur {
                outcome.push(format!("{key}: fm «{fm_v}» ≠ поле «{cur}»"));
            }
        }
    }
    let (action, detail) = if apply {
        if !cells.is_empty() {
            github_issue_fields::set_fields(&number, &store.repo, &cells)
                .await
                .with_context(|| format!("#{number}: клетки не записаны"))?;
        }
        // снос fm-шапки: тело = rest_md (единый write-path store)
        store
            .set_issue_body(&number, &fm.rest_md)
            .await
            .with_context(|| format!("#{number}: fm-шапка не снесена"))?;
        ("migrated", format!("клеток записано: {}", cells.len()))
    } else {
        (
            "migrated",
            format!(
                "dry-run: клеток к записи {}, шапка была бы снесена",
                cells.len()
            ),
        )
    };
    if outcome.is_empty() {
        return Ok(RowOutcome {
            number,
            action,
            detail,
        });
    }
    Ok(RowOutcome {
        number,
        action: "conflict",
        detail: format!("{detail}; {}", outcome.join("; ")),
    })
}

/// Проход по всем открытым строкам всех досок: fm-клетки → поля, снос шапки.
/// `apply` = false — dry-run (только отчёт, мутаций нет).
pub(crate) async fn migrate_all(store: &GitHubStore, apply: bool) -> Result<Vec<RowOutcome>> {
    let boards = store.discover_boards().await?;
    let mut out = Vec::new();
    for board in boards {
        let tasks = store.list(&board).await?;
        for task in tasks {
            out.push(migrate_row(store, &task, apply).await?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_legacy_fm_разбор_и_rest() {
        let fm = parse_legacy_fm("---\nstatus: Todo\nbranch: work/x\n---\n\n# тело\n");
        assert_eq!(fm.fields.get("status").map(String::as_str), Some("Todo"));
        assert_eq!(fm.fields.get("branch").map(String::as_str), Some("work/x"));
        assert_eq!(fm.rest_md, "\n# тело\n");
    }

    #[test]
    fn parse_legacy_fm_без_шапки_тело_целиком() {
        let fm = parse_legacy_fm("плоское тело\n");
        assert!(fm.fields.is_empty());
        assert_eq!(fm.rest_md, "плоское тело\n");
    }

    #[test]
    fn cell_action_правило_приоритета() {
        assert!(matches!(cell_action("", ""), CellAction::Skip));
        assert!(matches!(cell_action("work/x", ""), CellAction::Write));
        assert!(matches!(cell_action("work/x", "work/x"), CellAction::Skip));
        assert!(matches!(
            cell_action("fm-val", "field-val"),
            CellAction::Skip
        ));
    }

    #[test]
    fn fm_to_cells_маппинги_спецклеток() {
        let fm: HashMap<String, String> = [
            ("priority".to_string(), "high".to_string()),
            ("autodone".to_string(), "true".to_string()),
            ("branch".to_string(), "work/x".to_string()),
            ("planned".to_string(), "2026-11-20 15:04".to_string()),
            ("custom-agent".to_string(), "реликт".to_string()),
        ]
        .into_iter()
        .collect();
        let cells = fm_to_cells(&fm, &HashMap::new()).expect("cells");
        let written: Vec<&str> = cells.iter().map(|(n, _)| *n).collect();
        assert!(written.contains(&names::PRIORITY));
        assert!(written.contains(&names::AUTODONE));
        assert!(written.contains(&names::BRANCH));
        assert!(written.contains(&names::PLANNED));
        assert!(!written.iter().any(|n| n.contains("agent")));
        let (_, ad) = cells.iter().find(|(n, _)| *n == names::AUTODONE).unwrap();
        assert_eq!(
            ad.as_ref().unwrap().clone(),
            FieldValue::Select("on".into())
        );
        let (_, pl) = cells.iter().find(|(n, _)| *n == names::PLANNED).unwrap();
        assert_eq!(
            pl.as_ref().unwrap().clone(),
            FieldValue::Date("2026-11-20".into())
        );
    }

    #[test]
    fn fm_to_cells_поле_канон_выигрывает() {
        let fm: HashMap<String, String> = [("branch".to_string(), "fm-branch".to_string())]
            .into_iter()
            .collect();
        let current: HashMap<String, String> =
            [(names::BRANCH.to_string(), "field-branch".to_string())]
                .into_iter()
                .collect();
        let cells = fm_to_cells(&fm, &current).expect("cells");
        assert!(
            cells.is_empty(),
            "поле непусто — fm не перезаписывает: {cells:?}"
        );
    }

    #[test]
    fn fm_to_cells_autodone_off() {
        let fm: HashMap<String, String> = [("autodone".to_string(), "false".to_string())]
            .into_iter()
            .collect();
        let cells = fm_to_cells(&fm, &HashMap::new()).expect("cells");
        let (_, ad) = cells.iter().find(|(n, _)| *n == names::AUTODONE).unwrap();
        assert_eq!(
            ad.as_ref().unwrap().clone(),
            FieldValue::Select("off".into())
        );
    }
}
