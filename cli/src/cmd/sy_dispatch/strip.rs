//! strip: срез обёртки вывода task get (шапка карточки + аттачи) — вход
//! task body / сверка снапшота --base принимают контент без обёртки.

use crate::md_artifacts;
use crate::md_heading;

/// Построчный точный матч (задача 20261108045651, по образцу
/// task_get_wrapper_signature): граница — строка, trim_end() которой ровно
/// маркер, вне код-фенса. Матч по ПОДСТРОКЕ ломал срезку на легитимных
/// тайтлах с маркером внутри (строка шапки «Задача:   … «----- тело -----»…»
/// матчилась раньше настоящего маркера — остаток мусорный, сверка --base
/// ложно отказывала). Хвост «Аттачи:» — тем же построчным принципом
/// (префикс-старт строки, паритет с прежним "\nАттачи:"), фенсы не граница.
pub(crate) fn strip_task_get_output(raw: &str) -> String {
    let mut body_start: Option<usize> = None;
    let mut attach_start: Option<usize> = None;
    let mut in_fence = false;
    let mut offset = 0usize;
    for line in raw.lines() {
        let line_end = offset + line.len();
        // lines() режет CRLF-пару: реальный шаг до следующей строки — 2 байта
        // на "\r\n", иначе 1 ("\n" или конец входа)
        let step = if raw[line_end..].starts_with("\r\n") {
            2
        } else {
            1
        };
        if md_heading::toggles_fence(line) {
            in_fence = !in_fence;
        } else if !in_fence {
            if body_start.is_none() && line.trim_end() == md_artifacts::TASK_GET_BODY_MARKER {
                body_start = Some(line_end + step);
            } else if line.starts_with("Аттачи:") && offset > 0 {
                // срез с ведущим "\n" — паритет прежнего матча "\nАттачи:"
                attach_start = Some(offset - 1);
                break;
            }
        }
        offset = line_end + step;
    }
    let start = body_start.unwrap_or(0);
    let end = attach_start.unwrap_or(raw.len());
    // срез дословный (байтовые границы между строками): трейлинг-переводы
    // и CRLF-хвосты строк сохраняются, как при прежнем split_once
    let body = if start >= end { "" } else { &raw[start..end] };
    // ведущие переводы после маркера срезаются: strip_frontmatter дальше
    // требует строгий префикс «---\n» — "\n\n---\n…" свёл бы сверку вразнос
    body.trim_start_matches(['\r', '\n']).to_string()
}
