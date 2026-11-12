//! Секция «Комменты» и снапшот-гварды тела: перенос секции, отказ от
//! фрагментированного хвоста, сверка свежести, печать тредов.

use crate::cmd::sy_dispatch::strip::strip_task_get_output;
use crate::markdown;
use crate::md_heading;
use crate::siyuan_task as syt;
use anyhow::{bail, Result};

/// Системная секция «Комменты» переживает полный replace: если новый контент
/// её не несёт, существующая переносится в конец дословно (история диалога
/// юзер ↔ агент невосстановима — инцидент 06.11, крон «stable-ватчер»).
/// Контент с h2-секцией пишется как есть — осознанная замена; осознанный снос
/// — пустая секция в контенте. Тёзка «Комменты» на другом уровне — отказ:
/// перенос создал бы дубль (гард по образцу replace_md_section, ADR-048).
/// Инвариант системной секции, не сверка: действует и при --force.
pub(super) fn carry_over_comments_section(
    doc_title: &str,
    existing: &str,
    content: &str,
) -> anyhow::Result<String> {
    if crate::md_heading::has_h2_section(content, "Комменты") {
        return Ok(content.to_string());
    }
    if let Some((level, twin_line)) =
        crate::md_heading::heading_twin_outside_h2(content, "Комменты")
    {
        bail!(
            "контент несёт «Комменты» на уровне h{level}: «{twin_line}» — авто-перенос системной \
             секции создал бы дубль. Включи секцию «## Комменты» в контент осознанно (замена/снос \
             истории) или переименуй подсекцию"
        );
    }
    let Some(comments) = crate::md_heading::extract_h2_section(existing, "Комменты") else {
        return Ok(content.to_string());
    };
    let mut carried = content.trim_end().to_string();
    if carried.is_empty() {
        bail!("пустой контент replace тела «{doc_title}» — используй append без --replace");
    }
    carried.push_str("\n\n");
    carried.push_str(comments.trim_end());
    carried.push('\n');
    refuse_fragmented_comment_tail(doc_title, existing, &carried)?;
    Ok(carried)
}

/// Хвостовой гард фрагментации «Комменты» (инцидент 08.11, 20261108044740):
/// чужой h2 после секции (replace-section «не найдена — в конец») закрывает
/// её, поздние комменты (task comment аппендит в конец дока) остаются за
/// пределами — авто-перенос берёт только головной фрагмент, хвост пропадает
/// молча. Свежесть --base тут бессильна: снапшот несёт ту же фрагментацию,
/// нормализованная сверка сходится. Комменты хвоста, отсутствующие в
/// carried (контент + перенесённая секция), — отказ с перечнем, пока потеря
/// поправима; влитые агентом в контент матчатся подстрокой — гард молчит.
/// Осознанная замена истории (контент с собственной «## Комменты») сюда
/// не доходит — ветка выше.
fn refuse_fragmented_comment_tail(
    doc_title: &str,
    existing: &str,
    carried: &str,
) -> anyhow::Result<()> {
    let Some(tail) = crate::md_heading::tail_after_h2_section(existing, "Комменты") else {
        return Ok(());
    };
    let lost: Vec<String> = syt::comment_lines(&tail)
        .into_iter()
        .filter(|line| !carried.contains(line.as_str()))
        .collect();
    if lost.is_empty() {
        return Ok(());
    }
    let listed: String = lost.iter().map(|line| format!("  {line}\n")).collect();
    bail!(
        "секция «Комменты» в теле «{doc_title}» фрагментирована чужим h2: за её пределами сидят \
         хвостовые комменты — полный replace переносит только головную секцию, хвост пропал бы \
         молча:\n{listed}Влей перечисленные комменты в контент (в секцию ## Комменты нового тела) \
         и повтори; осознанный снос хвоста — включи в контент собственную секцию ## Комменты \
         (замена истории)"
    );
}

/// Freshness-гард полного --replace (потеря апдейта — класс инцидента 05.11
/// «Стек и архитектура», путь task body тот же класс: инцидент 06.11, крон
/// «stable-ватчер»). Снапшот --base — нетронутый вывод task get (или сырой
/// экспорт/чистое тело), снятый до правки: сверка со свежим экспортом
/// нормализованная (normalized_lines гасит рендер-шум round-trip — ложный
/// отказ на whitespace-дрейфе невозможен). Без снапшота сверять не с чем —
/// отказ с рецептом, не тихая запись. Существующая секция «Комменты» — часть
/// тела: свой коммент (task comment) между снапшотом и replace — легитимная
/// правка, гард честно откажет; порядок — replace → comment.
pub(super) fn require_fresh_snapshot(
    doc_title: &str,
    existing: &str,
    snapshot: Option<&str>,
) -> Result<()> {
    let Some(snapshot_text) = snapshot else {
        bail!(
            "replace требует --base <снапшот.md> — вывод `task get <id>` (допустим сырой \
             экспорт или чистое тело), снятый до правки: workflow «task get > снапшот → \
             правь → --replace --base снапшот»; без снапшота параллельная правка тела \
             теряется тихо. --force — осознанный overwrite без сверки"
        );
    };
    let snapshot_body =
        markdown::strip_export_wrapper(&strip_task_get_output(snapshot_text), doc_title);
    if markdown::normalized_lines(&snapshot_body) != markdown::normalized_lines(existing) {
        bail!(
            "тело менялось после снапшота (--base) — replace затёр бы чужие правки. \
             Перечитай текущий контент: task get <id>; влей дельту в новый контент, \
             пересними снапшот и повтори; --force — overwrite без сверки. Если между \
             снапшотом и replace тело комментировалось (task comment) — это тоже \
             правка: комменты после replace"
        );
    }
    Ok(())
}

pub(super) fn print_threads(md: &str) {
    println!("{}", collect_threads(md).trim_end());
}

/// Секции «## Комменты» тела: текст выдачи thread либо «комментов нет».
/// Матч нормализованный (двойные пробелы не теряют ответы), секция длится до
/// заголовка уровня ≤ 2 вне фенсов, дубли печатаются обе — read-путь не роняет.
/// Проход collect_threads: печатается секция «Комменты» до следующей
/// границы h1/h2 вне фенса.
struct ThreadsScan {
    out: String,
    printing: bool,
    fenced: bool,
}

impl ThreadsScan {
    fn copy_if_printing(&mut self, line: &str) {
        if self.printing {
            self.out.push_str(line);
            self.out.push('\n');
        }
    }

    /// Один шаг прохода по строке (дословное тело итерации монолита).
    fn step(&mut self, line: &str) {
        if md_heading::toggles_fence(line) {
            self.fenced = !self.fenced;
            self.copy_if_printing(line);
            return;
        }
        if self.fenced {
            self.copy_if_printing(line);
            return;
        }
        if let Some((level, text)) = md_heading::heading_of(line) {
            if level <= 2 {
                self.printing = level == 2 && text == "Комменты";
                self.copy_if_printing(line);
                return;
            }
        }
        self.copy_if_printing(line);
    }
}

pub(super) fn collect_threads(md: &str) -> String {
    let mut scan = ThreadsScan {
        out: String::new(),
        printing: false,
        fenced: false,
    };
    for line in md.lines() {
        scan.step(line);
    }
    if scan.out.is_empty() {
        scan.out.push_str("комментов нет\n");
    }
    scan.out
}
