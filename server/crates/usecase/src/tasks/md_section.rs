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
    /// Секция заменена; одноимённые дубли на уровне пина сняты (хилинг).
    Replaced { duplicates_removed: usize },
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
            },
        ));
    }
    if let Some((level, twin_line)) = scan.twin {
        return Err(twin_rejection(section, pin, level, twin_line, &want));
    }
    if pin != 2 {
        return Err(format!(
            "секция «{section}» на уровне h{pin} не найдена — append только для h2; \
             проверь уровень/имя или правь полным task_body"
        ));
    }
    if content.trim().is_empty() {
        // очистка отсутствующей секции создала бы пустую «## X» в хвосте —
        // исход неожидаем (находка дельта-ревью #1152): громкий отказ
        return Err(format!(
            "секция «{section}» не найдена — очищать нечего; \
             проверь имя секции или передай контент"
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
        "секция «{section}» на уровне h{pin} не найдена, но есть тёзка на уровне h{twin_level}: \
         «{twin_line}»; запроси с пином `{pin_hint}` / правь через содержащую ##-секцию / \
         полный task_body"
    )
}

/// Один шаг прохода по строке: строки секции/дубля до границы не
/// копируются; тёзка чужого уровня собирается в тот же проход (гард
/// стреляет только если матча не было; первая по порядку — детерминизм).
struct SectionScan<'a> {
    want: &'a str,
    pin: usize,
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

    fn step(&mut self, line: &'a str) {
        if toggles_fence(line) {
            self.fenced = !self.fenced;
            self.copy_if_not_dropping(line);
            return;
        }
        if self.fenced {
            self.copy_if_not_dropping(line);
            return;
        }
        let Some((level, text)) = heading_of(line) else {
            self.copy_if_not_dropping(line);
            return;
        };
        if level == self.pin && text == self.want {
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
        if level <= self.pin {
            self.dropping = false;
            push_blank_line(&mut self.out);
            self.copy(line);
            return;
        }
        self.copy_if_not_dropping(line);
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
