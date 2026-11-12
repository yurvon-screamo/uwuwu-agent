//! Section-хирургия markdown: replace_md_section, strip task get-обёртки,
//! отчёт о секционных правках.

use crate::md_heading;
use anyhow::{bail, Result};

/// Разметка вывода `task get` вокруг тела — не контент: шапка карточки
/// (Задача:/Статус:/…) до строки-маркера «----- тело -----» и хвост «Аттачи:»
/// с путями ассетов после тела. Полный get-вывод, сырой экспорт и чистое
/// тело сводятся к телу (контракт --base). Легитимные строки-маркеры в
/// чистом теле срежут часть контента — false-mismatch, отказ (safe-направление).
/// Маркер — общая константа с write-side детектором (md_artifacts):
/// срезка снапшота и детекция входа не разъедутся.
/// Итог секционной правки: заменено (со счётчиком удалённых дублей) или добавлено.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SectionEdit {
    Replaced { duplicates_removed: usize },
    Appended,
}

/// Секция «## X» в markdown: содержимое первого вхождения заменяется,
/// заголовок пишется байт-в-байт (двойные пробелы и ^block-id живы); одноимённые
/// дубли удаляются вместе с контентом — хилинг заманженных прод-тел, счётчик
/// попадает в SectionEdit. Граница секции — заголовок уровня ≤ 2 вне кодового
/// фенса: gherkin-фенсы bug-шаблонов несут строки «# …», фенс-трекинг обязателен,
/// иначе новая граница резала бы их (регрессия относительно кода до уровня-aware).
/// Запрос нормализован от собственного ATX-префикса: «## Решение» матчит
/// секцию «Решение» (иначе want нёс бы хэши, матч не сходился, а append
/// писал мусор «## ## Решение»). Гард уровня: h2-матча нет, но тёзка «X» есть
/// на уровне 1 или ≥ 3 — Err с уровнем и байт-точной строкой (первой по
/// порядку): иначе контент молча аппендится новой ##-секцией в конец — дубль
/// (ожог ###-правок). Тёзка в фенсе не заголовок — гард молчит; при живом
/// h2-матче тёзка игнорируется. Секция не найдена вовсе — append в конец
/// с нормализованным заголовком; пустой ввод очищает секцию (контракт);
/// пустая строка перед следующим заголовком восстанавливается.
/// Проход replace_md_section: строки секции/дубля до границы не копируются;
/// тёзка на чужом уровне собирается в тот же проход, гард стреляет только
/// если h2-матча не было; первая по порядку — детерминизм ошибки.
struct SectionScan<'a> {
    want: &'a str,
    content: &'a str,
    out: String,
    fenced: bool,
    dropping: bool,
    replaced: bool,
    duplicates_removed: usize,
    twin: Option<(usize, &'a str)>,
}

impl<'a> SectionScan<'a> {
    fn copy(&mut self, line: &str) {
        self.out.push_str(line);
        self.out.push('\n');
    }

    fn copy_if_not_dropping(&mut self, line: &str) {
        if !self.dropping {
            self.copy(line);
        }
    }

    /// Один шаг прохода по строке (дословное тело итерации монолита).
    fn step(&mut self, line: &'a str) {
        if md_heading::toggles_fence(line) {
            self.fenced = !self.fenced;
            self.copy_if_not_dropping(line);
            return;
        }
        if self.fenced {
            self.copy_if_not_dropping(line);
            return;
        }
        let Some((level, text)) = md_heading::heading_of(line) else {
            self.copy_if_not_dropping(line);
            return;
        };
        if level == 2 && text == self.want {
            if self.replaced {
                self.duplicates_removed += 1;
            } else {
                push_heading_block(&mut self.out, line, self.content);
                self.replaced = true;
            }
            self.dropping = true;
            return;
        }
        if text == self.want && self.twin.is_none() {
            self.twin = Some((level, line));
        }
        if level <= 2 {
            self.dropping = false;
            push_blank_line(&mut self.out);
            self.copy(line);
            return;
        }
        self.copy_if_not_dropping(line);
    }
}

pub(super) fn replace_md_section(
    md: &str,
    section: &str,
    content: &str,
) -> Result<(String, SectionEdit)> {
    let want = md_heading::normalize_heading_query(section);
    let mut scan = SectionScan {
        want: &want,
        content,
        out: String::new(),
        fenced: false,
        dropping: false, // контент секции/дубля до границы не копируется
        replaced: false,
        duplicates_removed: 0usize,
        twin: None,
    };
    for line in md.lines() {
        scan.step(line);
    }
    if !scan.replaced {
        if let Some((level, twin_line)) = scan.twin {
            bail!(
                "секция «{section}» на уровне ## не найдена, но есть тёзка на уровне h{level}: «{twin_line}»; \
                 --replace-section матчит только ## — правь через содержащую ##-секцию или полный task body --replace"
            );
        }
        let heading = format!("## {want}");
        if !scan.out.is_empty() {
            scan.out.push('\n');
        }
        scan.out.push_str(&heading);
        scan.out.push('\n');
        if !content.trim().is_empty() {
            scan.out.push_str(content.trim());
            scan.out.push('\n');
        }
        return Ok((scan.out, SectionEdit::Appended));
    }
    Ok((
        scan.out,
        SectionEdit::Replaced {
            duplicates_removed: scan.duplicates_removed,
        },
    ))
}

/// Блок секции: заголовок исходной строкой + новый контент (пустой — очистка).
fn push_heading_block(out: &mut String, heading_line: &str, content: &str) {
    push_blank_line(out);
    out.push_str(heading_line);
    out.push('\n');
    if !content.trim().is_empty() {
        out.push_str(content.trim());
        out.push('\n');
    }
}

/// Пустая строка между блоками: не в начале вывода и не задвоенная.
fn push_blank_line(out: &mut String) {
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
}

/// Пометка секционной правки в stdout: append и хилинг дублей — не тихие.
pub(super) fn report_section_edit(section: &str, edit: SectionEdit) {
    match edit {
        SectionEdit::Appended => println!("секция «{section}» не найдена — добавлена в конец"),
        SectionEdit::Replaced { duplicates_removed } if duplicates_removed > 0 => {
            println!("удалён дубль секции «{section}» ×{duplicates_removed}");
        }
        SectionEdit::Replaced { .. } => {}
    }
}
