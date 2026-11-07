//! Guard батч-операций доски: многоисточниковая верификация строк перед
//! сносом, undo-журнал в воркспейсе (data/assets/undo/), сверка состава
//! доски до/после. Инцидент 04.11: индекс ядра в деградацию врёт — решение
//! о сносе принимается только при согласии независимых источников
//! (FS-файл тела через getFile ядра — прямое чтение с диска, SQL по id, полный пагинированный
//! render, док по тайтлу).

use crate::markdown::{collapse_whitespace, strip_export_wrapper};
use crate::siyuan::{FileFetch, SiYuan};
use crate::siyuan_task::{body_template, legacy_body_template, normalize_title, SyBoard, SyTask};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Каталог undo-журналов в воркспейсе (kernel-пути от корня воркспейса):
/// multi-PC — журнал доступен с любой машины, домена «локальных кэшей нет»
/// соблюдена.
pub(crate) const UNDO_DIR: &str = "data/assets/undo";

/// Проба тела строки независимым источником.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Probe {
    /// Источник подтвердил существование.
    Found,
    /// Источник подтвердил отсутствие.
    Absent,
    /// Проба не состоялась (транспорт/доступ) — состояние неизвестно.
    Failed,
    /// Проба невозможна (нет docId у detached-строки).
    NotPossible,
}

/// Вердикт guard'а над строкой. Порядок вариантов = порядок правил
/// вердикт-функции; первое совпадение выигрывает.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Verdict {
    /// Строка не резолвится в полном render (фантом деградации или уже снята).
    Phantom,
    /// Draft + FS-alive + тело дословно равно пустому шаблону: ноль
    /// юзер-контента по определению. Гейт Draft явный — Todo-строка с
    /// нетронутым костяком сюда НЕ попадает (остаётся Live).
    DraftSkeleton,
    /// Тело живо по любому источнику — строка защищена: снос только
    /// per-row ack.
    Live,
    /// FS-проба не состоялась — состояние неизвестно.
    Divergent,
    /// Draft + тела нет нигде.
    DraftDead,
    /// Тела нет нигде (статус не Draft).
    Dead,
}

impl Verdict {
    /// Снос разрешён с `--yes` без per-row ack (Draft-пустышки — исключение
    /// постановки, владелец не нужен).
    pub(crate) fn is_auto_removable(self) -> bool {
        matches!(self, Verdict::DraftSkeleton | Verdict::DraftDead)
    }

    /// Снос разрешён с `--yes` (Dead — с owner-подтверждением по процедуре
    /// dry-run → владелец → --yes).
    pub(crate) fn is_removable(self) -> bool {
        matches!(
            self,
            Verdict::DraftSkeleton | Verdict::DraftDead | Verdict::Dead
        )
    }

    /// Снос только по явному per-row ack.
    pub(crate) fn needs_live_ack(self) -> bool {
        self == Verdict::Live
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Verdict::Phantom => "Phantom",
            Verdict::DraftSkeleton => "DraftSkeleton",
            Verdict::Live => "Live",
            Verdict::Divergent => "Divergent",
            Verdict::DraftDead => "DraftDead",
            Verdict::Dead => "Dead",
        }
    }
}

/// Собранные показания источников по одной строке. Наполняется gather-ходом
/// (I/O), вердикт считается чистой функцией — тестируется вся матрица.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RowEvidence {
    /// Строка найдена в полном пагинированном render.
    pub in_render: bool,
    /// Статус строки в render == Draft.
    pub status_is_draft: bool,
    /// FS-проба .sy файла тела.
    pub fs: Probe,
    /// SQL по id тела (`type='d' and id=…`).
    pub sql: Probe,
    /// Док с тайтлом строки существует под доской (ловит сломанную
    /// привязку при живом теле).
    pub title_body_found: bool,
    /// Тело (при fs=Found и Draft) равно пустому шаблону.
    pub skeleton_match: bool,
}

/// Вердикт-функция: правила применяются по порядку, первое совпадение
/// выигрывает. Положительное доказательство живости (FS-alive, SQL-found,
/// док по тайтлу) всегда сильнее: индекс в деградацию теряет записи,
/// а не изобретает.
pub(crate) fn verdict(evidence: &RowEvidence) -> Verdict {
    if !evidence.in_render {
        return Verdict::Phantom;
    }
    let body_alive =
        evidence.fs == Probe::Found || evidence.sql == Probe::Found || evidence.title_body_found;
    // DraftSkeleton стоит ПЕРЕД Live: исключение возможно только при явном
    // гейте Draft — иначе ветка сносила бы нетронутые костяки живых задач.
    if evidence.status_is_draft && evidence.fs == Probe::Found && evidence.skeleton_match {
        return Verdict::DraftSkeleton;
    }
    if body_alive {
        return Verdict::Live;
    }
    if evidence.fs == Probe::Failed || evidence.sql == Probe::Failed {
        return Verdict::Divergent;
    }
    if evidence.status_is_draft {
        Verdict::DraftDead
    } else {
        Verdict::Dead
    }
}

/// Тело равно пустому шаблону? Экспортная обёртка снимается, тег-строка
/// `#dev/#spike/#bug` (её дописывает create) вырезается, пробельные run'ы
/// схлопываются; сравнение против всех трёх шаблонов — тип задачи живёт
/// в теле, не в строке.
/// Тело равно пустому шаблону? Экспортная обёртка снимается, тег-строка
/// `#dev/#spike/#bug` (её дописывает create) вырезается, пробельные run'ы
/// схлопываются. Сверка против нового костяка и легаси всех трёх типов
/// (тип задачи живёт в теле, не в строке): новый костяк от тайтла не
/// зависит, легаси (до 06.11, `# {title}`-блок в контенте и полный
/// dev-набор Решение/Юзкейсы/Сиквенсы) — зависит. Легаси-ветка
/// обязательна: нетронутые скелеты, созданные до смены шаблона, иначе
/// матчатся как Live → task clean теряет брошенные Draft-черновики
/// (уходят в ручной --live-режим).
pub(crate) fn body_is_skeleton(export_md: &str, title: &str) -> bool {
    let stripped = strip_export_wrapper(export_md, title);
    let without_type_tag: String = stripped
        .lines()
        .filter(|line| !matches!(line.trim(), "#dev" | "#spike" | "#bug"))
        .collect::<Vec<_>>()
        .join("\n");
    let normalized = collapse_whitespace(&without_type_tag);
    ["dev", "spike", "bug"].iter().any(|ty| {
        collapse_whitespace(&body_template(ty, "")) == normalized
            || collapse_whitespace(&legacy_body_template(title, ty)) == normalized
    })
}

// ——— undo-журнал: write-ahead в data/assets/undo/ ———

/// Одна строка доски в журнале: достаточно для восстановления
/// (тело + строка + клетки).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct JournalRow {
    pub item_id: String,
    pub title: String,
    pub status: String,
    pub branch: String,
    pub pr: String,
    /// Текстовые клетки сверх контракта задач (кроны: Schedule).
    pub extra_text_cells: Vec<(String, String)>,
    pub body_doc: Option<String>,
    pub body_hpath: String,
    /// Экспорт тела на момент сноса (None — тела не было).
    pub body_md: Option<String>,
}

/// Журнал батч-операции: пишется ДО мутаций (write-ahead).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Journal {
    pub created: String,
    pub project: String,
    /// clean | rm | cron-rm
    pub kind: String,
    pub av_id: String,
    pub board_doc: String,
    pub notebook: String,
    pub rows: Vec<JournalRow>,
}

impl Journal {
    pub(crate) fn new(project: &str, kind: &str, board: &SyBoard) -> Self {
        Self {
            created: chrono::Utc::now().to_rfc3339(),
            project: project.to_string(),
            kind: kind.to_string(),
            av_id: board.av_id.clone(),
            board_doc: board.doc_id.clone(),
            notebook: board.notebook.clone(),
            rows: Vec::new(),
        }
    }

    fn file_name(&self) -> String {
        // имя детерминировано содержимым (created, не Utc::now: повторный
        // save того же журнала даёт то же имя); колонки в RFC3339 в имени
        // файла недопустимы — переформатируем
        let stamp = chrono::DateTime::parse_from_rfc3339(&self.created)
            .map(|t| t.format("%Y%m%dT%H%M%S").to_string())
            .unwrap_or_else(|_| {
                self.created
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .collect()
            });
        format!("{stamp}-{}-{}.json", self.project, self.kind)
    }

    fn path(&self) -> String {
        format!("{UNDO_DIR}/{}", self.file_name())
    }

    /// Write-ahead: журнал обязан лечь в воркспейс ДО первой мутации.
    pub(crate) async fn save(&self, sy: &SiYuan) -> Result<String> {
        let bytes = serde_json::to_vec_pretty(self).context("journal serialize")?;
        sy.put_workspace_file(&self.path(), &bytes)
            .await
            .with_context(|| format!("undo-журнал не записан: {}", self.path()))?;
        Ok(self.path())
    }

    /// Чтение журнала по имени файла (data/assets/undo/<name>) или полному
    /// workspace-пути.
    pub(crate) async fn load(sy: &SiYuan, name_or_path: &str) -> Result<Self> {
        let path = if name_or_path.starts_with("data/") {
            name_or_path.to_string()
        } else {
            format!("{UNDO_DIR}/{name_or_path}")
        };
        match sy.fetch_workspace_file(&path).await {
            FileFetch::Bytes(bytes) => {
                serde_json::from_slice(&bytes).with_context(|| format!("журнал {path} не парсится"))
            }
            FileFetch::Missing => bail!("журнала {path} нет (список: task undo --list)"),
            FileFetch::Failed(err) => bail!("журнал {path} не прочитан: {err}"),
        }
    }

    /// Список журналов: имя файла → журнал, отсортирован по имени
    /// (ts-префикс имени = хронология). Битый журнал (частичная запись при
    /// падении) не глушится молча — предупреждение: именно его владельцу,
    /// возможно, и нужно восстановить. Директории нет (фреш-воркспейс) —
    /// журналов нет, не ошибка.
    pub(crate) async fn list(sy: &SiYuan) -> Result<Vec<(String, Journal)>> {
        let mut out = Vec::new();
        let Some(names) = sy.try_list_dir(UNDO_DIR).await? else {
            return Ok(out);
        };
        for name in names {
            if !name.ends_with(".json") {
                continue;
            }
            match Journal::load(sy, &name).await {
                Ok(journal) => out.push((name, journal)),
                Err(err) => eprintln!(
                    "предупреждение: undo-журнал {name} не читается ({err}) — в списке пропущен"
                ),
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }
}

/// Собрать показания всех источников по строке. `title_doc_ids` — результат
/// body_doc_by_title (один вызов на строку), `body_export` — экспорт тела,
/// если fs проба нашла файл и статус Draft (иначе None).
pub(crate) fn evidence_from_sources(
    task: &SyTask,
    title_doc_ids: usize,
    fs: Probe,
    sql: Probe,
    body_export: Option<&str>,
) -> RowEvidence {
    let skeleton_match = match (fs, body_export) {
        (Probe::Found, Some(export_md)) => body_is_skeleton(export_md, &task.title),
        _ => false,
    };
    RowEvidence {
        in_render: true,
        status_is_draft: task.status.eq_ignore_ascii_case("draft"),
        fs,
        sql,
        title_body_found: title_doc_ids > 0,
        skeleton_match,
    }
}

/// FS-проба тела строки: docId известен — путь по id-цепочке доски
/// (индекс и тайтлы не участвуют); detached-строка — проба невозможна.
pub(crate) async fn probe_body_fs(
    sy: &SiYuan,
    board: &SyBoard,
    chain: &[String],
    task: &SyTask,
) -> Probe {
    let Some(doc) = task.body_doc.as_deref() else {
        return Probe::NotPossible;
    };
    let ancestors: Vec<&str> = chain.iter().map(String::as_str).collect();
    let path = SiYuan::doc_file_path(&board.notebook, &ancestors, doc);
    match sy.fetch_workspace_file(&path).await {
        FileFetch::Bytes(_) => Probe::Found,
        FileFetch::Missing => Probe::Absent,
        FileFetch::Failed(err) => {
            eprintln!(
                "предупреждение: FS-проба тела {} не состоялась: {err}",
                task.item_id
            );
            Probe::Failed
        }
    }
}

/// SQL-проба тела по id: негативный ответ НЕ доказательство смерти
/// (индекс в деградацию врёт) — доказательством смерти он становится только
/// в паре с FS-Absent (вердикт-функция).
pub(crate) async fn probe_body_sql(sy: &SiYuan, task: &SyTask) -> Probe {
    let Some(doc) = task.body_doc.as_deref() else {
        return Probe::NotPossible;
    };
    let rows = sy
        .sql(&format!(
            "select id from blocks where type='d' and id='{doc}' limit 1"
        ))
        .await;
    match rows {
        Ok(rows)
            if rows
                .iter()
                .any(|r| r.get("id").and_then(|i| i.as_str()) == Some(doc)) =>
        {
            Probe::Found
        }
        Ok(_) => Probe::Absent,
        Err(err) => {
            eprintln!(
                "предупреждение: SQL-проба тела {} не состоялась: {err}",
                task.item_id
            );
            Probe::Failed
        }
    }
}

/// Контрольная проба доски: файл самого док-доски обязан существовать.
/// Мисс — резолв доски подозрителен (find_board мог соврать), все FS-пробы
/// тел были бы ложно мертвы → аборт без мутаций.
pub(crate) async fn control_probe_board(
    sy: &SiYuan,
    board: &SyBoard,
    chain: &[String],
) -> Result<()> {
    let ancestors: Vec<&str> = chain[..chain.len().saturating_sub(1)]
        .iter()
        .map(String::as_str)
        .collect();
    let path = SiYuan::doc_file_path(&board.notebook, &ancestors, &board.doc_id);
    match sy.fetch_workspace_file(&path).await {
        FileFetch::Bytes(_) => Ok(()),
        FileFetch::Missing => bail!(
            "контрольная проба доски провалена: файла доски нет на ФС ({path}) — резолв доски подозрителен, батч прерван без мутаций"
        ),
        FileFetch::Failed(err) => bail!("контрольная проба доски не состоялась: {err}"),
    }
}

/// Пост-чек: лишние пропажи тайтлов. Ожидание: после = до − план.
/// Возврат — тайтлы, пропавшие без плана (нормализованное сравнение:
/// ∕ ≡ /, регистр не важен).
pub(crate) fn extra_missing_titles(
    before: &[String],
    after: &[String],
    removed: &[String],
) -> Vec<String> {
    let removed_set: HashSet<String> = removed.iter().map(|t| normalize_title(t)).collect();
    let after_set: HashSet<String> = after.iter().map(|t| normalize_title(t)).collect();
    before
        .iter()
        .map(|t| normalize_title(t))
        .filter(|t| !after_set.contains(t) && !removed_set.contains(t))
        .collect()
}

/// Тайтл-фрагмент совпал со строкой (для per-row --live ack); семантика
/// та же, что в resolve(): равенство, префикс или суффикс itemID,
/// подстрока нормализованного тайтла.
pub(crate) fn frag_matches(frag: &str, task: &SyTask) -> bool {
    let frag = frag.trim().trim_start_matches('-');
    if frag.is_empty() {
        return false;
    }
    task.item_id == frag
        || task.item_id.starts_with(frag)
        || task.item_id.ends_with(frag)
        || normalize_title(&task.title).contains(&normalize_title(frag))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::siyuan_task::SyTask;

    fn evidence(
        in_render: bool,
        draft: bool,
        fs: Probe,
        sql: Probe,
        title_body_found: bool,
        skeleton: bool,
    ) -> RowEvidence {
        RowEvidence {
            in_render,
            status_is_draft: draft,
            fs,
            sql,
            title_body_found,
            skeleton_match: skeleton,
        }
    }

    #[test]
    fn row_not_in_render_is_phantom_regardless_of_other_sources() {
        let ev = evidence(false, true, Probe::Found, Probe::Found, true, true);
        assert_eq!(verdict(&ev), Verdict::Phantom);
    }

    #[test]
    fn draft_with_untouched_skeleton_body_is_auto_removable() {
        let ev = evidence(true, true, Probe::Found, Probe::Found, true, true);
        assert_eq!(verdict(&ev), Verdict::DraftSkeleton);
        assert!(verdict(&ev).is_auto_removable());
        assert!(verdict(&ev).is_removable());
        assert!(!verdict(&ev).needs_live_ack());
    }

    /// Кейс ревью (High): Todo-задача после триажа с нетронутым костяком —
    /// гейт Draft не пущает её в DraftSkeleton, строка защищена.
    #[test]
    fn todo_with_untouched_skeleton_body_is_protected_live() {
        let ev = evidence(true, false, Probe::Found, Probe::Found, true, true);
        assert_eq!(verdict(&ev), Verdict::Live);
        assert!(verdict(&ev).needs_live_ack());
        assert!(!verdict(&ev).is_removable());
    }

    #[test]
    fn draft_with_fs_alive_body_without_skeleton_match_is_live() {
        // FS-alive + нет скелет-совпадения: контентное тело → защита Live
        let ev = evidence(true, true, Probe::Found, Probe::Absent, false, false);
        assert_eq!(verdict(&ev), Verdict::Live);
    }

    #[test]
    fn fs_alive_overrides_sql_and_title_negatives() {
        let ev = evidence(true, false, Probe::Found, Probe::Absent, false, false);
        assert_eq!(verdict(&ev), Verdict::Live);
    }

    #[test]
    fn sql_found_is_positive_evidence_when_fs_missing() {
        // FS-miss при живом индексе: индекс врёт потерями, найденное — живо
        let ev = evidence(true, false, Probe::Absent, Probe::Found, false, false);
        assert_eq!(verdict(&ev), Verdict::Live);
    }

    #[test]
    fn title_body_found_on_detached_row_is_live() {
        // сломанная привязка при живом теле (класс P0-бага привязки)
        let ev = evidence(
            true,
            false,
            Probe::NotPossible,
            Probe::NotPossible,
            true,
            false,
        );
        assert_eq!(verdict(&ev), Verdict::Live);
    }

    #[test]
    fn failed_fs_probe_is_divergent_and_never_removable() {
        let ev = evidence(true, false, Probe::Failed, Probe::Absent, false, false);
        assert_eq!(verdict(&ev), Verdict::Divergent);
        assert!(!verdict(&ev).is_removable());
    }

    #[test]
    fn draft_without_body_anywhere_is_draft_dead_auto_removable() {
        let ev = evidence(true, true, Probe::Absent, Probe::Absent, false, false);
        assert_eq!(verdict(&ev), Verdict::DraftDead);
        assert!(verdict(&ev).is_auto_removable());
        assert!(verdict(&ev).is_removable());
    }

    #[test]
    fn non_draft_without_body_anywhere_is_dead_removable_with_yes() {
        let ev = evidence(true, false, Probe::Absent, Probe::Absent, false, false);
        assert_eq!(verdict(&ev), Verdict::Dead);
        assert!(verdict(&ev).is_removable());
        assert!(!verdict(&ev).is_auto_removable());
    }

    #[test]
    fn detached_row_without_title_body_is_dead_by_status() {
        let draft = evidence(
            true,
            true,
            Probe::NotPossible,
            Probe::NotPossible,
            false,
            false,
        );
        assert_eq!(verdict(&draft), Verdict::DraftDead);
        let live_status = evidence(
            true,
            false,
            Probe::NotPossible,
            Probe::NotPossible,
            false,
            false,
        );
        assert_eq!(verdict(&live_status), Verdict::Dead);
    }

    fn sy_task(title: &str, status: &str) -> SyTask {
        SyTask {
            item_id: "item-1".into(),
            title: title.into(),
            status: status.into(),
            branch: String::new(),
            pr: String::new(),
            schedule: String::new(),
            last_run: String::new(),
            dev_session: String::new(),
            body_doc: Some("doc-1".into()),
        }
    }

    fn skeleton_export(title: &str, ty: &str) -> String {
        format!(
            "---\ntitle: {title}\n---\n\n# {title}\n\n{}\n\n#{ty}\n",
            body_template(ty, "")
        )
    }

    /// Экспорт нетронутого скелета эпохи до 06.11: рендер-строка имени
    /// плюс легаси-костяк со своим `# {title}` heading-блоком в контенте
    /// (живая проба: у свежих Draft-скелетов той эпохи в экспорте два H1;
    /// strip_export_wrapper снимает только рендер-строку — блок остаётся).
    fn legacy_skeleton_export(title: &str, ty: &str) -> String {
        format!(
            "---\ntitle: {title}\n---\n\n# {title}\n\n{}\n\n#{ty}\n",
            legacy_body_template(title, ty)
        )
    }

    #[test]
    fn body_is_skeleton_matches_all_three_templates_with_export_wrapper_and_tag() {
        for ty in ["dev", "spike", "bug"] {
            let export = skeleton_export("таск", ty);
            assert!(
                body_is_skeleton(&export, "таск"),
                "шаблон {ty} должен совпасть"
            );
        }
    }

    #[test]
    fn body_is_skeleton_matches_legacy_skeletons_of_all_three_types() {
        for ty in ["dev", "spike", "bug"] {
            let export = legacy_skeleton_export("таск", ty);
            assert!(
                body_is_skeleton(&export, "таск"),
                "легаси-шаблон {ty} должен совпасть"
            );
        }
    }

    #[test]
    fn body_is_skeleton_rejects_body_with_user_content() {
        let export = format!(
            "---\ntitle: таск\n---\n\n# таск\n\n{}\nзаметка владельца\n",
            body_template("dev", "")
        );
        assert!(!body_is_skeleton(&export, "таск"));
    }

    #[test]
    fn body_is_skeleton_rejects_legacy_skeleton_with_extra_user_line() {
        let export = format!(
            "{}\nзаметка владельца\n",
            legacy_skeleton_export("таск", "spike")
        );
        assert!(!body_is_skeleton(&export, "таск"));
    }

    #[test]
    fn body_is_skeleton_normalizes_whitespace_runs() {
        let raw = body_template("dev", "").replace('\n', "\n\n  \n");
        let export = format!("---\ntitle: т\n---\n\n# т\n\n{raw}\n");
        assert!(body_is_skeleton(&export, "т"));
    }

    #[test]
    fn extra_missing_titles_reports_only_unplanned_losses() {
        let before = vec!["живая".into(), "снимаемая".into(), "чужая".into()];
        let after = vec!["живая".into()];
        let removed = vec!["снимаемая".into()];
        let extra = extra_missing_titles(&before, &after, &removed);
        assert_eq!(extra, vec![normalize_title("чужая")]);
    }

    #[test]
    fn extra_missing_titles_tolerates_normalized_variants() {
        let before = vec!["Пилот RU/EN".into()];
        let after = vec!["Пилот RU\u{2215}EN".into()];
        let removed = vec!["другая".into()];
        assert!(extra_missing_titles(&before, &after, &removed).is_empty());
    }

    #[test]
    fn frag_matches_by_item_id_prefix_and_title_substring() {
        let mut task = sy_task("Пилот RU\u{2215}EN: старт", "Todo");
        task.item_id = "20261104215724-iso1ra4".into();
        assert!(
            frag_matches("20261104215724-iso1ra4", &task),
            "равенство полного id"
        );
        assert!(frag_matches("20261104", &task), "префикс id");
        assert!(
            frag_matches("-iso1ra4", &task),
            "суффикс id с ведущим дефисом, как в resolve"
        );
        assert!(
            frag_matches("пилот ru/en", &task),
            "подстрока нормализованного тайтла"
        );
        assert!(!frag_matches("чужое", &task));
        assert!(!frag_matches("", &task));
    }

    #[test]
    fn journal_save_and_load_roundtrip_via_workspace_file() {
        let mut journal = Journal::new(
            "uwuwu",
            "clean",
            &SyBoard {
                doc_id: "board-doc".into(),
                av_id: "av-1".into(),
                block_id: "block-1".into(),
                hpath: "/uwuwu/Задачи uwuwu".into(),
                notebook: "20261103224312-fbrf5fp".into(),
            },
        );
        journal.rows.push(JournalRow {
            item_id: "item-1".into(),
            title: "таск".into(),
            status: "Draft".into(),
            branch: String::new(),
            pr: String::new(),
            extra_text_cells: vec![("Schedule".into(), "*/5 * * * *".into())],
            body_doc: None,
            body_hpath: "/uwuwu/Задачи uwuwu/таск".into(),
            body_md: None,
        });

        let bytes = serde_json::to_vec_pretty(&journal).unwrap();
        let parsed: Journal = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.rows.len(), 1);
        assert_eq!(parsed.rows[0].title, "таск");
        assert_eq!(
            parsed.rows[0].extra_text_cells,
            vec![("Schedule".to_string(), "*/5 * * * *".to_string())]
        );
        assert_eq!(parsed.kind, "clean");
        assert_eq!(parsed.av_id, "av-1");
        let name = parsed.file_name();
        assert!(name.ends_with("-uwuwu-clean.json"));
        assert!(
            !name.contains(':'),
            "колонки недопустимы в имени файла: {name}"
        );
        assert!(parsed.path().starts_with("data/assets/undo/"));
    }
}
