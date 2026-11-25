//! Секционная хирургия markdown: replace_md_section + body_hash —
//! серверный write-path больших тел задач (#1152). Порт легаси cli
//! (body_sections + md_heading, git ba82ec9^) с осознанной модификацией:
//! ATX-префикс запроса пинит уровень секции («### X» — h3, bare/«## X» —
//! дефолт h2) вместо легаси-среза префикса. Легаси-срез давал неявный
//! матч уровня — отвергнутая в ADR-048 альтернатива; пин сохраняет
//! намерение ADR-048 (матч никогда не неявный) и открывает h3-секции
//! (первый потребитель — H3-нутро «Отчёта инженера», кейс #1046).

/// (уровень, нормализованный текст) заголовка markdown в строке; None —
/// не заголовок. Заголовок ATX: `#{1,6}` + пробел — строки вида `#bug`
/// заголовком не являются. Нормализация текста (схлопывание пробелов,
/// срез хвостового `^block-id`) делает матч устойчивым к двойным пробелам
/// и идентификаторам блоков; записанная строка при этом хранится
/// байт-в-байт на стороне вызывающего.
pub(crate) fn heading_of(line: &str) -> Option<(usize, String)> {
    let level = line.chars().take_while(|&c| c == '#').count();
    if level == 0 || level > 6 || !line[level..].starts_with(' ') {
        return None;
    }
    let text = normalize_heading_text(&line[level..]);
    if text.is_empty() {
        return None;
    }
    Some((level, text))
}

/// Нормализация текста заголовка/аргумента для матча: пробелы схлопнуты,
/// хвостовой `^block-id` срезан.
pub(crate) fn normalize_heading_text(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    match collapsed.rsplit_once(" ^") {
        Some((text, suffix)) if is_block_id(suffix) => text.to_string(),
        _ => collapsed,
    }
}

/// Суффикс block-id SiYuan: непустой ASCII (буквы, цифры, дефисы).
fn is_block_id(suffix: &str) -> bool {
    !suffix.is_empty()
        && suffix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Запрос секции → (пин уровня, текст): «### X» → (3, «X»), bare/«## X» →
/// (2, «X»), «# X» → (1, «X»). Валидный ATX-префикс пинит уровень,
/// прочий ввод матчится как h2 (легаси срезал префикс — см. шапку модуля).
pub(crate) fn normalize_heading_query(raw: &str) -> (usize, String) {
    let trimmed = raw.trim();
    let level = trimmed.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&level) && trimmed[level..].starts_with(' ') {
        let text = normalize_heading_text(&trimmed[level..]);
        if !text.is_empty() {
            return (level, text);
        }
    }
    (2, normalize_heading_text(trimmed))
}

/// Строка переключает состояние кодового фенса (открывает/закрывает ```).
pub(crate) fn toggles_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

/// Исход секционной замены.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionEdit {
    /// Секция заменена; одноимённые дубли на уровне пина сняты (хилинг);
    /// `old_content_len` — байт-объём всего снятого контента: первая
    /// найденная секция до границы + её дубли на уровне пина (объём,
    /// уходящий с носителя; пустые строки по нулям) — мера гарда
    /// радикального сжатия и честных длин квитанции (#1409, #1572):
    /// дубли входят в меру, иначе числа гарда противоречили дельте тела.
    Replaced {
        duplicates_removed: usize,
        old_content_len: usize,
    },
    /// Секции не было (пин h2) — добавлена в конец.
    Appended,
}

/// Секционная замена: матч на уровне пина с фенс-трекингом; заголовок
/// пишется байт-в-байт (двойные пробелы, `^block-id` живы), матч по
/// нормализованному тексту; одноимённые дубли на уровне пина удаляются
/// вместе с контентом (счётчик — в SectionEdit); пустой content очищает
/// секцию (заголовок жив); пустая строка перед следующим заголовком
/// восстанавливается; тёзка в фенсе — не заголовок (гард молчит); при
/// живом матче тёзка чужого уровня игнорируется. Граница секции уровня L
/// — следующий заголовок уровня ≤ L вне кодового фенса (gherkin-фенсы
/// bug-шаблонов несут строки «# …» — фенс-трекинг обязателен). Секции на
/// пине нет + тёзка на другом уровне → Err (твин-гард: уровень, байт-точная
/// строка, рецепт); для пина h2 без тёзки — append в конец чистым
/// `## {want}`, для прочих пинов — Err (append только h2); запрос очистки
/// отсутствующей секции (пустой content + секции нет) → Err — append
/// создал бы пустую секцию.
pub fn replace_md_section(
    md: &str,
    section: &str,
    content: &str,
) -> Result<(String, SectionEdit), String> {
    let (pin, want) = normalize_heading_query(section);
    let mut scan = SectionScan {
        want: &want,
        pin,
        content,
        out: String::new(),
        fenced: false,
        dropping: false, // контент секции/дубля до границы не копируется
        replaced: false,
        duplicates_removed: 0usize,
        old_content_len: 0usize,
        measuring: false,
        twin: None,
    };
    for line in md.lines() {
        scan.step(line);
    }
    if scan.replaced {
        return Ok((
            scan.out,
            SectionEdit::Replaced {
                duplicates_removed: scan.duplicates_removed,
                old_content_len: scan.old_content_len,
            },
        ));
    }
    if let Some((level, twin_line)) = scan.twin {
        return Err(twin_rejection(section, pin, level, twin_line, &want));
    }
    if pin != 2 {
        return Err(format!(
            "section «{section}» at h{pin} not found — append is for h2 only; \
             check the level/name or edit with a full task_body"
        ));
    }
    if content.trim().is_empty() {
        // очистка отсутствующей секции создала бы пустую «## X» в хвосте —
        // исход неожидаем (находка дельта-ревью #1152): громкий отказ
        return Err(format!(
            "section «{section}» not found — nothing to clear; \
             check the section name or pass content"
        ));
    }
    // append: секции не было — заголовок пишется нормализованным (байт-запрос
    // исходной строки не нужен: строки секции в теле нет)
    let mut out = scan.out;
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&format!("## {want}\n"));
    if !content.trim().is_empty() {
        out.push_str(content.trim());
        out.push('\n');
    }
    Ok((out, SectionEdit::Appended))
}

/// Объём контента секции в байтах (масштаб для честных длин отказа
/// «content не передан», #1572): None — матчабельной секции на пине нет
/// (отсутствующая, твин, не-h2 пин — для текста отказа этого достаточно).
pub(crate) fn section_content_len(md: &str, section: &str) -> Option<usize> {
    match replace_md_section(md, section, "") {
        Ok((
            _,
            SectionEdit::Replaced {
                old_content_len, ..
            },
        )) => Some(old_content_len),
        _ => None,
    }
}

/// Единая мера объёма контента — байты строк без переводов строк, пустые
/// строки по нулям: «было» (мера SectionScan) и «стало» (квитанция и пол
/// гарда) одной меры, числа сопоставимы (ревью #1572 Low).
pub(crate) fn section_content_metric(s: &str) -> usize {
    s.trim().lines().map(|l| l.len()).sum()
}

/// Твин-гард: громкий отказ с уровнем тёзки, байт-точной строкой (первой
/// по порядку — детерминизм) и рецептом обхода; записи нет.
fn twin_rejection(
    section: &str,
    pin: usize,
    twin_level: usize,
    twin_line: &str,
    want: &str,
) -> String {
    let pin_hint = format!("{} {}", "#".repeat(twin_level), want);
    format!(
        "section «{section}» at h{pin} not found, but a namesake exists at h{twin_level}: \
         «{twin_line}»; request it pinned `{pin_hint}` / edit through the containing ## section / \
         full task_body"
    )
}

/// Один шаг прохода по строке: строки секции/дубля до границы не
/// копируются; объём снятых строк (первая секция + её дубли) копится в
/// old_content_len (мера гарда сжатия, #1572); тёзка чужого уровня
/// собирается в тот же проход (гард стреляет только если матча не было;
/// первая по порядку — детерминизм).
struct SectionScan<'a> {
    want: &'a str,
    pin: usize,
    content: &'a str,
    out: String,
    fenced: bool,
    dropping: bool,
    replaced: bool,
    duplicates_removed: usize,
    /// байт-объём снятых строк: первая секция до границы + её дубли;
    /// пустые строки по нулям, '\n' не считается — мера контента.
    old_content_len: usize,
    /// копление меры открыто: от первой найденной секции до её границы.
    measuring: bool,
    twin: Option<(usize, &'a str)>,
}

impl<'a> SectionScan<'a> {
    fn copy(&mut self, line: &str) {
        self.out.push_str(line);
        self.out.push('\n');
    }

    /// Строка внутри секции/дубля: не копируется; мера первой найденной
    /// секции копится до границы.
    fn drop_or_copy(&mut self, line: &str) {
        if !self.dropping {
            self.copy(line);
            return;
        }
        if self.measuring {
            self.old_content_len += line.len();
        }
    }

    fn step(&mut self, line: &'a str) {
        if toggles_fence(line) {
            self.fenced = !self.fenced;
            self.drop_or_copy(line);
            return;
        }
        if self.fenced {
            self.drop_or_copy(line);
            return;
        }
        let Some((level, text)) = heading_of(line) else {
            self.drop_or_copy(line);
            return;
        };
        if level == self.pin && text == self.want {
            if self.replaced {
                self.duplicates_removed += 1;
                // контент дубля тоже стирается — мера снятого копится
                // дальше (симметрия с дельтой тела: иначе телесное плечо
                // гарда сжатия стреляло бы с «ровными» числами секции,
                // ревью #1572 Low)
                self.measuring = true;
            } else {
                push_heading_block(&mut self.out, line, self.content);
                self.replaced = true;
                self.measuring = true;
                self.old_content_len = 0;
            }
            self.dropping = true;
            return;
        }
        if text == self.want && self.twin.is_none() {
            self.twin = Some((level, line));
        }
        if level <= self.pin {
            self.dropping = false;
            self.measuring = false;
            push_blank_line(&mut self.out);
            self.copy(line);
            return;
        }
        self.drop_or_copy(line);
    }
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

/// FNV-1a 64-bit hex тела issue как записано в GitHub: токен свежести
/// секционной правки (base_hash), не криптография — зависимость не
/// заводится. fm-обработка на границе (стрип) применяется к контенту при
/// записи и на хэш не влияет: после записи хэш считается ровно по тому,
/// что вернул и подтвердил PATCH (replace_body сверяет ответ с отправленным).
pub fn body_hash(body: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in body.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}
