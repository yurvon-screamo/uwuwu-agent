//! task clean: батч-снос строк с guard-вердиктами по независимым источникам.

use super::journal::{execute_removal, row_verdict};
use crate::siyuan::SiYuan;
use crate::siyuan_guard as guard;
use crate::siyuan_guard::Verdict;
use crate::siyuan_task as syt;
use anyhow::{bail, Result};

/// Батч-снос строк под guard'ом: dry-run дефолт, гейт ack на весь батч,
/// write-ahead журнал, один item_remove массивом, пост-чек состава.
pub(super) async fn task_clean(
    sy: &SiYuan,
    project: &str,
    ids: &str,
    yes: bool,
    live: Option<&str>,
) -> Result<()> {
    let frags: Vec<String> = ids
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(String::from)
        .collect();
    if frags.is_empty() {
        bail!("--ids обязателен: кандидаты через запятую (row-id/префикс/подстрока)");
    }
    let board = syt::find_board(sy, project).await?;
    let chain = syt::board_doc_chain(sy, &board).await?;
    guard::control_probe_board(sy, &board, &chain).await?;
    let rows_before = syt::read_rows(sy, &board).await?;

    println!(
        "guard план{} — доска «{}» ({}), кандидатов: {}",
        if yes { " (--yes)" } else { " (dry-run)" },
        board.hpath,
        project,
        frags.len()
    );
    // один render и одна выборка саб-доков на весь батч: транспортный сбой
    // здесь — громкая ошибка, а не ярлык «фантом» на кандидате
    let planned = plan_candidates(sy, &board, &chain, &rows_before, &frags).await?;

    let live_frags: Vec<String> = live
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(String::from)
        .collect();
    warn_unmatched_live(&planned, &live_frags);
    let (removable, refused) = split_removable(&planned, &live_frags);

    if !yes {
        println!(
            "dry-run: мутаций нет. Исполнение — повтор с --yes (журнал и пост-чек включатся сами)."
        );
        return Ok(());
    }
    if !refused.is_empty() {
        bail!(
            "батч отклонён ЦЕЛИКОМ, ноль мутаций:\n  - {}",
            refused.join("\n  - ")
        );
    }
    if removable.is_empty() {
        println!("сносить нечего: ни один кандидат не в вердиктах сноса");
        return Ok(());
    }

    execute_removal(sy, &board, project, &removable, &rows_before).await
}

/// Верификация кандидатов: match фрагментов по render-строкам, дедуп,
/// вердикты по независимым источникам, печать плана. (subdoc_ids — одна
/// выборка саб-доков на весь батч; вызывается после печати плана —
/// порядок проб дословно из монолита.)
async fn plan_candidates<'a>(
    sy: &SiYuan,
    board: &syt::SyBoard,
    chain: &[String],
    rows_before: &[syt::SyTask],
    frags: &'a [String],
) -> Result<Vec<(&'a str, syt::SyTask, Verdict)>> {
    let subdoc_ids = syt::board_subdoc_ids(sy, board).await?;
    let mut planned: Vec<(&str, syt::SyTask, Verdict)> = Vec::new();
    let mut seen_items: Vec<String> = Vec::new();
    for frag in frags {
        match syt::match_needle(rows_before, &subdoc_ids, frag) {
            syt::NeedleMatch::Found(task) => {
                if seen_items.contains(&task.item_id) {
                    println!("  [дубль        ] «{frag}» — та же строка, что и earlier фрагмент ({}), учтена один раз", task.item_id);
                    continue;
                }
                seen_items.push(task.item_id.clone());
                let verdict = row_verdict(sy, board, chain, &task).await?;
                let hint = if verdict.is_auto_removable() {
                    "снос с --yes (пустышка, владелец не нужен)"
                } else if verdict.is_removable() {
                    "снос с --yes (не-Draft: подтверждение владельца по процедуре)"
                } else if verdict.needs_live_ack() {
                    "защищена: снос только с --live"
                } else {
                    "не трогаю"
                };
                println!(
                    "  [{:<13}] «{}» (строка {}) — {}",
                    verdict.label(),
                    task.title,
                    task.item_id,
                    hint
                );
                planned.push((frag, *task, verdict));
            }
            syt::NeedleMatch::NotFound => {
                println!("  [Phantom]     «{frag}» — не найдена в render, не трогаю")
            }
            syt::NeedleMatch::Ambiguous { reason, matches } => {
                println!("  [Ambiguous]   «{frag}» — неоднозначный {reason}: {matches} совпадений, уточни фрагмент; не трогаю")
            }
        }
    }
    Ok(planned)
}

/// Предупреждение о --live фрагментах, не матчащих ни одну строку плана.
fn warn_unmatched_live(planned: &[(&str, syt::SyTask, Verdict)], live_frags: &[String]) {
    for frag in live_frags {
        let matched = planned
            .iter()
            .any(|(_, task, _)| guard::frag_matches(frag, task));
        if !matched {
            println!("предупреждение: --live «{frag}» не матчит ни одну строку плана (опечатка?)");
        }
    }
}

/// Разбор вердиктов: снос (с ack для --live-защищённых) против отказов батча.
fn split_removable<'a>(
    planned: &'a [(&str, syt::SyTask, Verdict)],
    live_frags: &[String],
) -> (Vec<(&'a syt::SyTask, Verdict)>, Vec<String>) {
    let mut removable: Vec<(&syt::SyTask, Verdict)> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    for (_, task, verdict) in planned {
        let acked = live_frags.iter().any(|f| guard::frag_matches(f, task));
        if verdict.is_removable() || (verdict.needs_live_ack() && acked) {
            removable.push((task, *verdict));
        } else if verdict.needs_live_ack() {
            refused.push(format!(
                "«{}» — Live: нужен --live <фрагмент> или исключи из --ids",
                task.title
            ));
        } else {
            refused.push(format!("«{}» — {}: не трогаю", task.title, verdict.label()));
        }
    }
    (removable, refused)
}
