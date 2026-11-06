//! Конвертация ` ```html_embed [имя] `-фенсов в живые HTML-блоки SiYuan.
//!
//! Контракт (докстринг `task body`): анонимный фенс — html инлайн; именованный
//! непустой — пара «маркер-абзац `⁂ diagram:<имя>` + html»; именованный пустой —
//! удаление диаграммы. Модуль pure: разбор ввода и план операций без сети.

use anyhow::{bail, Result};

/// Префикс маркер-абзаца, адресующего диаграмму в доке (формат афин-эпохи,
/// живёт в прод-доках: «Стек и архитектура» и тела задач).
pub(crate) const DIAGRAM_MARKER_PREFIX: &str = "⁂ diagram:";

const FENCE_OPEN: &str = "```html_embed";

/// Тело именованного фенса: html на вставку/замену или пустое (удаление).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FenceBody {
    Html(String),
    Empty,
}

/// Именованный фенс — операция адресуется маркером в доке.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NamedFence {
    pub(crate) name: String,
    pub(crate) body: FenceBody,
}

/// Сегмент разобранного ввода в порядке следования.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputSegment {
    /// Markdown мимо фенсов (plain + прочие кодовые блоки).
    Markdown(String),
    /// Html-тело анонимного фенса.
    AnonymousHtml(String),
    /// Именованный фенс.
    Named(NamedFence),
}

/// Разбор ввода с фенсами.
#[derive(Debug)]
pub(crate) struct ParsedInput {
    pub(crate) segments: Vec<InputSegment>,
}

impl ParsedInput {
    /// Именованные фенсы в порядке следования (block-хирургия append-пути).
    pub(crate) fn named_fences(&self) -> Vec<NamedFence> {
        self.segments
            .iter()
            .filter_map(|s| match s {
                InputSegment::Named(f) => Some(f.clone()),
                _ => None,
            })
            .collect()
    }

    /// Markdown append-остатка: plain и анонимный html; именованные фенсы
    /// вырезаны (их добавляет block-хирургия). Html-инлайн окружён пустыми
    /// строками — без них Lute поглощает соседний текст внутрь html-блока.
    pub(crate) fn residual_md(&self) -> String {
        let mut out = String::new();
        for segment in &self.segments {
            match segment {
                InputSegment::Markdown(md) => out.push_str(md),
                InputSegment::AnonymousHtml(html) => {
                    push_html_with_blank_guards(&mut out, html);
                }
                InputSegment::Named(_) => {}
            }
        }
        out
    }

    /// Markdown rewrite-путей (`--replace` / `--replace-section` / create):
    /// тело пересобрано целиком — именованные фенсы инлайнятся парами
    /// «маркер + html» безусловно, пустые вырезаются (no-op удаления не
    /// нужно: пересобранное тело уже не содержит старую диаграмму).
    pub(crate) fn inline_md(&self) -> String {
        let mut out = String::new();
        for segment in &self.segments {
            match segment {
                InputSegment::Markdown(md) => out.push_str(md),
                InputSegment::AnonymousHtml(html) => {
                    push_html_with_blank_guards(&mut out, html);
                }
                InputSegment::Named(fence) => {
                    if let FenceBody::Html(html) = &fence.body {
                        let pair = format!("{}\n\n{html}", marker_text(&fence.name));
                        push_html_with_blank_guards(&mut out, &pair);
                    }
                }
            }
        }
        out
    }
}

/// Гарантирует пустую строку до html и после: CommonMark html-блок съедает
/// последующий непустой текст внутрь себя.
fn push_html_with_blank_guards(out: &mut String, html: &str) {
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
    out.push_str(html);
    out.push('\n');
    out.push('\n');
}

/// Блок дока, следующий за маркером (для выбора операции).
pub(crate) struct FollowingBlock {
    pub(crate) id: String,
    pub(crate) is_html: bool,
}

/// Адрес диаграммы в доке: маркер и его следующий блок.
pub(crate) struct MarkerLocation {
    pub(crate) marker_id: String,
    pub(crate) following: Option<FollowingBlock>,
}

/// План block-хирургии append-пути по одному именованному фенсу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiagramOp {
    /// Маркер есть, следом html — обновить содержимое блока.
    UpdateExisting { block_id: String, html: String },
    /// Маркер есть, следом html — удалить блок (пустой фенс).
    DeleteExisting { block_id: String },
    /// Маркер есть, следом не-html (или маркер последний) — вставить html
    /// сразу после маркера; сосед не трогается (после вставки повтор
    /// команды идемпотентно сработает как UpdateExisting).
    InsertAfterMarker { marker_id: String, html: String },
    /// Маркера нет — appendBlock пары «маркер + html» в конец (один вызов
    /// создаёт оба блока).
    AppendNewPair { name: String, html: String },
    /// Пустой фенс без диаграммы — удалять нечего.
    Nothing,
}

/// Строка состоит только из пробельных символов (пустая для CommonMark:
/// такая строка завершает html-блок и рвёт контент на мусор).
fn is_blank_line(line: &str) -> bool {
    line.trim().is_empty()
}

/// Тело фенса (строки до закрывающего ```) с валидацией: пустая строка
/// внутри непустого тела → отказ (тихий разрыв блока на мусор).
fn html_from_body(body_lines: &[String]) -> Result<FenceBody> {
    if body_lines.iter().all(|l| is_blank_line(l)) {
        return Ok(FenceBody::Empty);
    }
    if let Some(line) = body_lines.iter().find(|l| is_blank_line(l)) {
        bail!(
            "html_embed-фенс содержит пустую строку — html-блок разорвётся на мусор \
             (split на «{line:?}»); убери пустые строки из html-тела"
        );
    }
    Ok(FenceBody::Html(body_lines.join("\n")))
}

/// Разбор открывающей строки фенса: ` ```html_embed [имя] `. Имя — весь
/// остаток строки trimmed: пусто → анонимный. Возвращает None для прочих
/// строк (в том числе ```:html_embedded и прочих соседних языков).
fn embed_fence_opener(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix(FENCE_OPEN)?;
    if rest.is_empty() {
        Some(String::new())
    } else {
        rest.strip_prefix(' ').map(str::trim).map(String::from)
    }
}

/// Разбор ввода: фенсы на верхнем уровне markdown (внутри других кодовых
/// блоков не матчатся). Отказы до любых мутаций: незакрытый фенс, дубль
/// имени именованных, пустая строка в html-теле.
pub(crate) fn parse_input(md: &str) -> Result<ParsedInput> {
    let mut segments: Vec<InputSegment> = Vec::new();
    let mut named: Vec<NamedFence> = Vec::new();

    #[derive(Default)]
    struct MarkdownBuffer {
        text: String,
    }
    impl MarkdownBuffer {
        fn flush(self, segments: &mut Vec<InputSegment>) -> Self {
            if !self.text.is_empty() {
                segments.push(InputSegment::Markdown(self.text));
            }
            Self::default()
        }
        fn push_line(&mut self, line: &str) {
            self.text.push_str(line);
            self.text.push('\n');
        }
    }
    let mut plain = MarkdownBuffer::default();

    enum State {
        Top,
        InEmbed { name: String, body: Vec<String> },
        InOtherFence,
    }
    let mut state = State::Top;

    for line in md.lines() {
        match &mut state {
            State::Top => {
                if let Some(name) = embed_fence_opener(line) {
                    state = State::InEmbed {
                        name,
                        body: Vec::new(),
                    };
                    continue;
                }
                if line.trim_start().starts_with("```") {
                    state = State::InOtherFence;
                }
                plain.push_line(line);
            }
            State::InOtherFence => {
                if line.trim_start().starts_with("```") {
                    state = State::Top;
                }
                plain.push_line(line);
            }
            State::InEmbed { name, body } => {
                if line.trim_start().starts_with("```") {
                    let name = std::mem::take(name);
                    let body = std::mem::take(body);
                    let html = html_from_body(&body)?;
                    state = State::Top;
                    plain = plain.flush(&mut segments);
                    if name.is_empty() {
                        // анонимный: html инлайн; пустое тело — вырезается
                        if let FenceBody::Html(html) = html {
                            segments.push(InputSegment::AnonymousHtml(html));
                        }
                    } else {
                        if named.iter().any(|f| f.name == name) {
                            bail!(
                                "дубль имени html_embed-фенса «{name}» в одном вводе — \
                                 адресация по маркеру стала бы неоднозначной"
                            );
                        }
                        named.push(NamedFence {
                            name: name.clone(),
                            body: html,
                        });
                        segments.push(InputSegment::Named(named.last().unwrap().clone()));
                    }
                    continue;
                }
                body.push(line.to_string());
            }
        }
    }
    if let State::InEmbed { name, .. } = state {
        bail!("html_embed-фенс «{name}» не закрыт — ввод обрывается на теле фенса");
    }
    plain.flush(&mut segments);

    Ok(ParsedInput { segments })
}

/// Текст маркер-абзаца для имени диаграммы.
pub(crate) fn marker_text(name: &str) -> String {
    format!("{DIAGRAM_MARKER_PREFIX}{name}")
}

/// План операции append-пути по адресу диаграммы.
pub(crate) fn plan_append_op(location: Option<&MarkerLocation>, fence: &NamedFence) -> DiagramOp {
    match (&fence.body, location) {
        (
            FenceBody::Empty,
            Some(MarkerLocation {
                following: Some(fb),
                ..
            }),
        ) if fb.is_html => DiagramOp::DeleteExisting {
            block_id: fb.id.clone(),
        },
        (FenceBody::Empty, _) => DiagramOp::Nothing,
        (FenceBody::Html(html), None) => DiagramOp::AppendNewPair {
            name: fence.name.clone(),
            html: html.clone(),
        },
        (
            FenceBody::Html(html),
            Some(MarkerLocation {
                marker_id,
                following: Some(fb),
            }),
        ) if fb.is_html => DiagramOp::UpdateExisting {
            block_id: fb.id.clone(),
            html: html.clone(),
        },
        (FenceBody::Html(html), Some(MarkerLocation { marker_id, .. })) => {
            DiagramOp::InsertAfterMarker {
                marker_id: marker_id.clone(),
                html: html.clone(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named_fence(name: &str, body: FenceBody) -> NamedFence {
        NamedFence {
            name: name.into(),
            body,
        }
    }

    fn parse(md: &str) -> ParsedInput {
        parse_input(md).unwrap()
    }

    #[test]
    fn anonymous_fence_inlines_html_with_blank_guards() {
        let parsed = parse("текст до\n```html_embed\n<div>a</div>\n```\nтекст после\n");
        assert!(parsed
            .segments
            .iter()
            .all(|s| !matches!(s, InputSegment::Named(_))));
        assert_eq!(
            parsed.residual_md(),
            "текст до\n\n<div>a</div>\n\nтекст после\n"
        );
    }

    #[test]
    fn anonymous_html_following_plain_text_gets_blank_line_guard() {
        let parsed = parse("плотный текст\n```html_embed\n<div>a</div>\n```\n");
        assert_eq!(parsed.residual_md(), "плотный текст\n\n<div>a</div>\n\n");
    }

    #[test]
    fn named_fence_is_extracted_from_residual() {
        let parsed = parse(
            "до\n```html_embed arch\n<div>a</div>\n```\nпосле\n```html_embed\n<b>x</b>\n```\n",
        );
        assert_eq!(parsed.residual_md(), "до\nпосле\n\n<b>x</b>\n\n");
        assert_eq!(
            parsed.named_fences(),
            vec![named_fence("arch", FenceBody::Html("<div>a</div>".into()))]
        );
    }

    #[test]
    fn named_empty_fence_is_delete_marker() {
        let parsed = parse("```html_embed arch\n```");
        assert_eq!(
            parsed.named_fences(),
            vec![named_fence("arch", FenceBody::Empty)]
        );
        assert_eq!(parsed.residual_md(), "");
    }

    #[test]
    fn anonymous_empty_fence_is_cut_silently() {
        let parsed = parse("до\n```html_embed\n```\nпосле\n");
        assert_eq!(parsed.residual_md(), "до\nпосле\n");
    }

    #[test]
    fn fences_inside_other_code_block_are_literal() {
        let md = "```\n```html_embed arch\n<div>a</div>\n```\n```\n";
        let parsed = parse(md);
        assert!(parsed.named_fences().is_empty());
        assert_eq!(parsed.residual_md(), md);
    }

    #[test]
    fn html_embed_with_suffix_is_not_our_fence() {
        let md = "```html_embedded\n<div>a</div>\n```\n";
        let parsed = parse(md);
        assert!(parsed.named_fences().is_empty());
        assert_eq!(parsed.residual_md(), md);
    }

    #[test]
    fn unclosed_embed_fence_fails_loudly() {
        let err = parse_input("```html_embed arch\n<div>a</div>\n").unwrap_err();
        assert!(err.to_string().contains("не закрыт"), "{err}");
    }

    #[test]
    fn duplicate_fence_names_fail_before_any_mutation() {
        let md = "```html_embed arch\n<div>a</div>\n```\n```html_embed arch\n<div>b</div>\n```\n";
        let err = parse_input(md).unwrap_err();
        assert!(err.to_string().contains("дубль имени"), "{err}");
    }

    #[test]
    fn blank_line_inside_named_body_fails() {
        let err =
            parse_input("```html_embed arch\n<div>a</div>\n\n<div>b</div>\n```\n").unwrap_err();
        assert!(err.to_string().contains("пустую строку"), "{err}");
    }

    #[test]
    fn blank_line_inside_anonymous_body_fails() {
        let err = parse_input("```html_embed\n<div>a</div>\n\n<div>b</div>\n```\n").unwrap_err();
        assert!(err.to_string().contains("пустую строку"), "{err}");
    }

    #[test]
    fn fence_name_takes_whole_line_remainder() {
        let parsed = parse("```html_embed stack-l0 v2\n<div>a</div>\n```\n");
        assert_eq!(parsed.named_fences()[0].name, "stack-l0 v2");
    }

    #[test]
    fn indented_fence_opener_is_matched() {
        let parsed = parse("  ```html_embed arch\n<div>a</div>\n```\n");
        assert_eq!(parsed.named_fences()[0].name, "arch");
    }

    #[test]
    fn without_fences_input_is_untouched() {
        let md = "обычный текст\n\n```rust\nfn main() {}\n```\n";
        let parsed = parse(md);
        assert!(parsed.named_fences().is_empty());
        assert_eq!(parsed.residual_md(), md);
        assert_eq!(parsed.inline_md(), md);
    }

    #[test]
    fn marker_text_format() {
        assert_eq!(marker_text("arch"), "⁂ diagram:arch");
    }

    fn marker_loc(following: Option<(&str, bool)>) -> Option<MarkerLocation> {
        Some(MarkerLocation {
            marker_id: "marker-1".into(),
            following: following.map(|(id, is_html)| FollowingBlock {
                id: id.into(),
                is_html,
            }),
        })
    }

    #[test]
    fn update_when_marker_followed_by_html() {
        let fence = named_fence("arch", FenceBody::Html("<div>new</div>".into()));
        let op = plan_append_op(marker_loc(Some(("html-1", true))).as_ref(), &fence);
        assert_eq!(
            op,
            DiagramOp::UpdateExisting {
                block_id: "html-1".into(),
                html: "<div>new</div>".into()
            }
        );
    }

    #[test]
    fn insert_after_marker_when_following_block_is_code() {
        let fence = named_fence("arch", FenceBody::Html("<div>new</div>".into()));
        let op = plan_append_op(marker_loc(Some(("code-1", false))).as_ref(), &fence);
        assert_eq!(
            op,
            DiagramOp::InsertAfterMarker {
                marker_id: "marker-1".into(),
                html: "<div>new</div>".into()
            }
        );
    }

    #[test]
    fn insert_after_marker_when_marker_is_last_block() {
        let fence = named_fence("arch", FenceBody::Html("<div>new</div>".into()));
        let op = plan_append_op(marker_loc(None).as_ref(), &fence);
        assert!(matches!(op, DiagramOp::InsertAfterMarker { .. }));
    }

    #[test]
    fn append_new_pair_when_marker_absent() {
        let fence = named_fence("arch", FenceBody::Html("<div>new</div>".into()));
        assert_eq!(
            plan_append_op(None, &fence),
            DiagramOp::AppendNewPair {
                name: "arch".into(),
                html: "<div>new</div>".into()
            }
        );
    }

    #[test]
    fn delete_existing_diagram() {
        let fence = named_fence("arch", FenceBody::Empty);
        let op = plan_append_op(marker_loc(Some(("html-1", true))).as_ref(), &fence);
        assert_eq!(
            op,
            DiagramOp::DeleteExisting {
                block_id: "html-1".into()
            }
        );
    }

    #[test]
    fn delete_without_diagram_is_nothing() {
        let fence = named_fence("arch", FenceBody::Empty);
        assert_eq!(
            plan_append_op(marker_loc(None).as_ref(), &fence),
            DiagramOp::Nothing
        );
        assert_eq!(plan_append_op(None, &fence), DiagramOp::Nothing);
        let op = plan_append_op(marker_loc(Some(("code-1", false))).as_ref(), &fence);
        assert_eq!(op, DiagramOp::Nothing);
    }

    #[test]
    fn inline_replaces_named_fence_with_marker_pair() {
        let md = "до\n```html_embed arch\n<div>a</div>\n```\nпосле\n";
        assert_eq!(
            parse(md).inline_md(),
            "до\n\n⁂ diagram:arch\n\n<div>a</div>\n\nпосле\n"
        );
    }

    #[test]
    fn inline_drops_empty_named_fence() {
        let md = "до\n```html_embed arch\n```\nпосле\n";
        assert_eq!(parse(md).inline_md(), "до\nпосле\n");
    }

    #[test]
    fn inline_two_different_names_keeps_order() {
        let md = "```html_embed one\n<div>1</div>\n```\nсередина\n```html_embed two\n<div>2</div>\n```\n";
        assert_eq!(
            parse(md).inline_md(),
            "⁂ diagram:one\n\n<div>1</div>\n\nсередина\n\n⁂ diagram:two\n\n<div>2</div>\n\n"
        );
    }
}
