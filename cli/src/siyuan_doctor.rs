//! uwuwu-cli doctor: read-only пробы деградации ядра SiYuan.
//!
//! Рождён из охоты на волновой класс «commit tx failed: document metadata
//! is not ready» (v3.8.9, задача 20261105075305): пробы фиксируют и саму
//! волну в логах ядра, и её следы в данных (Draft-пустышки, сироты
//! индекс↔ФС, мёртвые привязки доски).
//!
//! Exit-семантика: 0 = выполненные пробы чисты (SKIP с причиной допустим,
//! пока выполнилась хотя бы одна проба); 1 = деградация найдена — включая
//! недоступное ядро (недоступность и есть деградация); 2 = ни одна проба
//! не выполнилась.

use crate::siyuan::{doc_content_is_empty, parse_block_ids, workspace_root, SiYuan};
use crate::siyuan_task as syt;
use serde_json::json;
use std::collections::HashSet;
use std::time::{Duration, SystemTime};

#[derive(Debug)]
pub(crate) enum ProbeVerdict {
    Clean,
    Degraded(String),
    Skipped(String),
    /// Находка вне волны (осадок, наблюдение): печатается и не влияет
    /// на exit-код — реакция владельца, не сигнал кронам.
    Observed(String),
}

#[derive(Debug)]
pub(crate) struct ProbeOutcome {
    pub name: &'static str,
    pub verdict: ProbeVerdict,
    /// Не влияющий на вердикт контекст (счётчики, легаси-примечания).
    pub detail: Option<String>,
}

impl ProbeOutcome {
    fn new(name: &'static str, verdict: ProbeVerdict) -> Self {
        Self {
            name,
            verdict,
            detail: None,
        }
    }

    fn with_detail(mut self, detail: String) -> Self {
        self.detail = Some(detail);
        self
    }

    fn apply_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }
}

const KERNEL: &str = "kernel";
const WAVE: &str = "wave";
const BOARD: &str = "board";
const BOARD_ORPHANS: &str = "board-orphans";
const FS_SQL: &str = "fs-sql";
const HISTORY_DELETES: &str = "history-deletes";

/// Окно пробы history-deletes: межаудитный интервал lead (суточный
/// цикл ночного аудита). Константа — проба наблюдения, без интерфейса.
const HISTORY_DELETES_WINDOW: Duration = Duration::from_secs(24 * 3600);

pub(crate) fn doctor_exit_code(outcomes: &[ProbeOutcome]) -> i32 {
    let any_ran = outcomes
        .iter()
        .any(|o| !matches!(o.verdict, ProbeVerdict::Skipped(_)));
    if !any_ran {
        return 2;
    }
    if outcomes
        .iter()
        .any(|o| matches!(o.verdict, ProbeVerdict::Degraded(_)))
    {
        return 1;
    }
    0
}

pub(crate) fn report(outcomes: &[ProbeOutcome]) -> String {
    let mut lines = Vec::new();
    for o in outcomes {
        let verdict_line = match &o.verdict {
            ProbeVerdict::Clean => format!("OK      {}: чисто", o.name),
            ProbeVerdict::Degraded(why) => format!("DEGRADED {}: {why}", o.name),
            ProbeVerdict::Skipped(why) => format!("SKIP    {}: {why}", o.name),
            ProbeVerdict::Observed(what) => format!("OBSERVE {}: {what}", o.name),
        };
        match &o.detail {
            Some(detail) => lines.push(format!("{verdict_line} [{detail}]")),
            None => lines.push(verdict_line),
        }
    }
    match doctor_exit_code(outcomes) {
        0 => lines.push("вердикт: 0 (чисто)".into()),
        1 => lines.push("вердикт: 1 (деградация)".into()),
        _ => lines.push("вердикт: 2 (пробы не выполнились)".into()),
    }
    lines.join("\n")
}

/// Счётчик волновых сигнатур в логе ядра (docker logs siyuan-demo).
pub(crate) struct WaveScan {
    pub commit_failures: usize,
    pub metadata_failures: usize,
}

/// Порог волнового вердикта: единичные commit-отказы за окно — шум ядра,
/// не волна (DEGRADED на каждую строчку = exit 1 крона на пустом месте).
const WAVE_DEGRADED_THRESHOLD: usize = 5;

pub(crate) fn scan_kernel_log_for_waves(log: &str) -> WaveScan {
    WaveScan {
        commit_failures: log
            .lines()
            .filter(|l| l.contains("commit tx failed"))
            .count(),
        metadata_failures: log
            .lines()
            .filter(|l| l.contains("metadata is not ready"))
            .count(),
    }
}

pub(crate) fn wave_verdict(scan: &WaveScan, since_min: u64) -> (ProbeVerdict, Option<String>) {
    if scan.commit_failures == 0 {
        return (ProbeVerdict::Clean, None);
    }
    if scan.commit_failures < WAVE_DEGRADED_THRESHOLD {
        return (
            ProbeVerdict::Clean,
            Some(format!(
                "единичные tx-отказы ({} < {WAVE_DEGRADED_THRESHOLD}) — шум, не волна; при росте — деградация",
                scan.commit_failures
            )),
        );
    }
    (
        ProbeVerdict::Degraded(format!(
            "«commit tx failed» ×{} (metadata ×{}) за {since_min} мин — ядро в волне; \
             агентам пауза 2–5 мин, рестарт по ранбуку",
            scan.commit_failures, scan.metadata_failures
        )),
        None,
    )
}

/// id .sy-документов ноутбука на диске (data/<box>/**, имя без .sy).
pub(crate) fn collect_sy_ids(box_dir: &std::path::Path) -> std::io::Result<Vec<String>> {
    let mut ids = Vec::new();
    let mut stack = vec![box_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "sy") {
                ids.push(
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or_default()
                        .to_string(),
                );
            }
        }
    }
    Ok(ids)
}

/// Детальный diff списков id при расхождении счётчиков: выборка первых
/// расхождений каждого направления, хвост — счётчиком.
pub(crate) fn diff_id_lists(fs_ids: &[String], sql_ids: &[String], cap: usize) -> String {
    let fs_set: std::collections::HashSet<_> = fs_ids.iter().collect();
    let sql_set: std::collections::HashSet<_> = sql_ids.iter().collect();
    let sample = |items: Vec<&String>| -> String {
        let head: Vec<String> = items.iter().take(cap).map(|s| (*s).clone()).collect();
        let more = if items.len() > cap {
            format!(" …+{}", items.len() - cap)
        } else {
            String::new()
        };
        format!("{}{more}", head.join(", "))
    };
    let fs_only: Vec<&String> = fs_ids.iter().filter(|id| !sql_set.contains(*id)).collect();
    let sql_only: Vec<&String> = sql_ids.iter().filter(|id| !fs_set.contains(*id)).collect();
    format!(
        "только ФС [{}]; только SQL [{}]",
        sample(fs_only),
        sample(sql_only)
    )
}

/// Список id доков ноутбука из SQL: пагинация limit 500 + offset до короткой
/// страницы (count и выборки иммунны к капу ядра в 64 строки, PR #41).
async fn sql_doc_ids(sy: &SiYuan, box_id: &str) -> Result<Vec<String>, anyhow::Error> {
    const PAGE: usize = 500;
    let mut ids = Vec::new();
    let mut offset = 0usize;
    loop {
        let stmt = format!(
            "select id from blocks where box='{box_id}' and type='d' and id=root_id \
             order by id limit {PAGE} offset {offset}"
        );
        let envelope = sy
            .post_probe("/api/query/sql", json!({ "stmt": stmt }))
            .await?;
        let page: Vec<String> = envelope
            .pointer("/data")
            .and_then(|d| d.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let fetched = page.len();
        ids.extend(page);
        if fetched < PAGE {
            return Ok(ids);
        }
        offset += fetched;
    }
}

/// Вердикт FS↔SQL по паре счётчиков одного ноутбука (count(*) иммунен
/// к капу SQL-ядра в 64 строки).
pub(crate) fn fs_sql_pair_verdict(name: &str, fs_count: u64, sql_count: u64) -> Option<String> {
    (fs_count != sql_count).then(|| format!("{name}: FS {fs_count} ↔ SQL {sql_count}"))
}

pub(crate) async fn run(sy: &SiYuan, project: &str, since_min: u64) -> Vec<ProbeOutcome> {
    let mut outcomes = Vec::new();

    let kernel_alive = match sy
        .post_probe("/api/query/sql", json!({ "stmt": "select 1" }))
        .await
    {
        Ok(_) => {
            outcomes.push(ProbeOutcome::new(KERNEL, ProbeVerdict::Clean));
            true
        }
        Err(e) => {
            outcomes.push(ProbeOutcome::new(
                KERNEL,
                ProbeVerdict::Degraded(format!("kernel недоступен: {e}")),
            ));
            false
        }
    };

    outcomes.push(wave_scan_probe(since_min).await);

    outcomes.push(if kernel_alive {
        board_probe(sy, project).await
    } else {
        ProbeOutcome::new(BOARD, ProbeVerdict::Skipped("kernel недоступен".into()))
    });

    outcomes.push(if kernel_alive {
        board_orphans_probe(sy, project).await
    } else {
        ProbeOutcome::new(
            BOARD_ORPHANS,
            ProbeVerdict::Skipped("kernel недоступен".into()),
        )
    });

    outcomes.push(if kernel_alive {
        fs_sql_probe(sy).await
    } else {
        ProbeOutcome::new(FS_SQL, ProbeVerdict::Skipped("kernel недоступен".into()))
    });

    // Чисто FS-проба: вне kernel-гейта — след сноса в истории воркспейса
    // переживает смерть ядра. Реестр досок для классификации — kernel-зависимая
    // часть: недоступен → деградация к полному перечислению без разметки.
    let task_boards = if kernel_alive {
        match syt::task_board_root_ids(sy).await {
            Ok(ids) => TaskBoards::Classified(ids),
            Err(e) => TaskBoards::Unavailable(format!("реестр досок: {e}")),
        }
    } else {
        TaskBoards::Unavailable("kernel недоступен".into())
    };
    outcomes.push(history_deletes_probe(&task_boards));

    outcomes
}

async fn wave_scan_probe(since_min: u64) -> ProbeOutcome {
    let container =
        std::env::var("UWUWU_SIYUAN_CONTAINER").unwrap_or_else(|_| "siyuan-demo".into());
    let output = std::process::Command::new("docker")
        .args(["logs", &container, "--since", &format!("{since_min}m")])
        .output();
    match output {
        Err(_) => ProbeOutcome::new(
            WAVE,
            ProbeVerdict::Skipped("docker недоступен — wave-скан пропущен".into()),
        ),
        Ok(out) if !out.status.success() => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let first = stderr.lines().next().unwrap_or("без деталей");
            ProbeOutcome::new(
                WAVE,
                ProbeVerdict::Skipped(format!("docker logs неудача: {first}")),
            )
        }
        Ok(out) => {
            let log = String::from_utf8_lossy(&out.stdout);
            let scan = scan_kernel_log_for_waves(&log);
            let (verdict, detail) = wave_verdict(&scan, since_min);
            ProbeOutcome::new(WAVE, verdict).apply_detail(detail)
        }
    }
}

async fn board_probe(sy: &SiYuan, project: &str) -> ProbeOutcome {
    let board = match syt::find_board(sy, project).await {
        Ok(board) => board,
        Err(e) => {
            return ProbeOutcome::new(
                BOARD,
                ProbeVerdict::Skipped(format!("доска «{project}»: {e}")),
            )
        }
    };
    let rows = match syt::read_rows(sy, &board).await {
        Ok(rows) => rows,
        Err(e) => {
            return ProbeOutcome::new(BOARD, ProbeVerdict::Skipped(format!("render доски: {e}")))
        }
    };
    // Молчаливый default на отказе SQL = ложный Clean при живой деградации
    // («metadata is not ready» на этом вызове — и есть волновой симптом);
    // для диагностического инструмента честнее SKIP всей пробы.
    let dead = match syt::dead_bound_doc_ids(sy, &rows).await {
        Ok(dead) => dead,
        Err(e) => {
            return ProbeOutcome::new(
                BOARD,
                ProbeVerdict::Skipped(format!(
                    "мёртвые привязки не проверены ({e}); доска не верифицирована"
                )),
            )
        }
    };

    let mut findings = Vec::new();
    let mut legacy_unbound = 0usize;
    for row in &rows {
        let Some(doc_id) = row.body_doc.as_deref() else {
            // Легаси-строки миграции живут без привязки хронически (тайтл-fallback);
            // это не волновой след — примечание, не деградация.
            legacy_unbound += 1;
            continue;
        };
        if dead.contains(doc_id) {
            findings.push(format!("«{}»: мёртвая привязка {doc_id}", row.title));
            continue;
        }
        if let Some(finding) = empty_body_finding(sy, doc_id, &row.title).await {
            findings.push(finding);
        }
    }
    let legacy_note = format!("строк {}, легаси без привязки {legacy_unbound}", rows.len());
    if findings.is_empty() {
        ProbeOutcome::new(BOARD, ProbeVerdict::Clean).with_detail(legacy_note)
    } else {
        ProbeOutcome::new(BOARD, ProbeVerdict::Degraded(findings.join("; ")))
            .with_detail(legacy_note)
    }
}

/// Пустое тело по ZWJ-критерию — не Children>0 (ядро не оставляет док
/// без блока, Children>0 слеп к Draft-пустышкам).
async fn empty_body_finding(sy: &SiYuan, doc_id: &str, title: &str) -> Option<String> {
    let text = match sy.block_children_text(doc_id).await {
        Ok(text) => text,
        Err(_) => return Some(format!("«{title}»: тело {doc_id} не читается (ошибка MCP)")),
    };
    let ids = parse_block_ids(&text);
    doc_content_is_empty(&ids, &text).then(|| format!("«{title}»: пустое тело (ZWJ-заглушка)"))
}

/// Прямой ребёнок-док доски (строка SQL-выборки).
pub(crate) struct BoardChildDoc {
    pub id: String,
    pub hpath: String,
    pub updated: Option<String>,
}

/// Дата из ial дока (created ядро в SQL не отдаёт — ближайший прокси
/// возраста тела): "…updated=\"20261108010134\"…" → "2026-11-08".
pub(crate) fn doc_updated_date(ial: &str) -> Option<String> {
    let pos = ial.find("updated=\"")? + "updated=\"".len();
    let digits: String = ial[pos..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.len() < 8 {
        return None;
    }
    Some(format!(
        "{}-{}-{}",
        &digits[0..4],
        &digits[4..6],
        &digits[6..8]
    ))
}

/// Кандидаты-сироты: дети-доки без живой строки. Строка живёт либо
/// привязкой (bound_ids), либо тайтлом (row_titles — легаси/detached
/// строки без привязки; тот же фоллбек, что у CLI-резолва задач).
pub(crate) fn orphan_candidates<'a>(
    children: &'a [BoardChildDoc],
    bound_ids: &HashSet<String>,
    row_titles: &HashSet<String>,
) -> Vec<&'a BoardChildDoc> {
    children
        .iter()
        .filter(|child| {
            let title = child.hpath.rsplit('/').next().unwrap_or("");
            !bound_ids.contains(&child.id) && !row_titles.contains(&syt::normalize_title(title))
        })
        .collect()
}

/// Компактный sample сирот: «id «тайтл» (updated)» через "; ", хвост счётчиком.
pub(crate) fn orphan_sample(orphans: &[&BoardChildDoc], cap: usize) -> String {
    let head: Vec<String> = orphans
        .iter()
        .take(cap)
        .map(|child| {
            let title = child.hpath.rsplit('/').next().unwrap_or("");
            match &child.updated {
                Some(date) => format!("{} «{title}» ({date})", child.id),
                None => format!("{} «{title}»", child.id),
            }
        })
        .collect();
    let more = if orphans.len() > cap {
        format!(" …+{}", orphans.len() - cap)
    } else {
        String::new()
    };
    format!("{}{more}", head.join("; "))
}

/// Прямые дети-доки доски. Фильтр type='d' обязателен: hpath в blocks
/// несёт каждый блок (абзацы, заголовки) — без него выборка раздуется
/// до всех блоков всех детей. Пагинация limit 500/offset — иммунитет
/// к дефолт-капу ядра 64 строки.
async fn board_child_docs(
    sy: &SiYuan,
    board: &syt::SyBoard,
) -> Result<Vec<BoardChildDoc>, anyhow::Error> {
    const PAGE: usize = 500;
    let like = board.hpath.replace('\'', "''");
    let mut docs = Vec::new();
    let mut offset = 0usize;
    loop {
        let stmt = format!(
            "select id, hpath, ial from blocks where type='d' \
             and hpath like '{like}/%' and hpath not like '{like}/%/%' \
             order by id limit {PAGE} offset {offset}"
        );
        let envelope = sy
            .post_probe("/api/query/sql", json!({ "stmt": stmt }))
            .await?;
        let page: Vec<BoardChildDoc> = envelope
            .pointer("/data")
            .and_then(|d| d.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        Some(BoardChildDoc {
                            id: row.get("id")?.as_str()?.to_string(),
                            hpath: row.get("hpath")?.as_str()?.to_string(),
                            updated: row
                                .get("ial")
                                .and_then(|i| i.as_str())
                                .and_then(doc_updated_date),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let fetched = page.len();
        docs.extend(page);
        if fetched < PAGE {
            return Ok(docs);
        }
        offset += fetched;
    }
}

/// Тела-сироты: прямые дети-доки доски задач без живой строки (обратное
/// направление board-пробы). Наблюдение, не деградация: осадок миграции
/// (тела-дубли эпохи пересборок досок) живёт на досках до чистки
/// владельцем — вердикт волны портить нельзя.
async fn board_orphans_probe(sy: &SiYuan, project: &str) -> ProbeOutcome {
    let board = match syt::find_board(sy, project).await {
        Ok(board) => board,
        Err(e) => {
            return ProbeOutcome::new(
                BOARD_ORPHANS,
                ProbeVerdict::Skipped(format!("доска «{project}»: {e}")),
            )
        }
    };
    let rows = match syt::read_rows(sy, &board).await {
        Ok(rows) => rows,
        Err(e) => {
            return ProbeOutcome::new(
                BOARD_ORPHANS,
                ProbeVerdict::Skipped(format!("render доски: {e}")),
            )
        }
    };
    let children = match board_child_docs(sy, &board).await {
        Ok(children) => children,
        Err(e) => {
            return ProbeOutcome::new(
                BOARD_ORPHANS,
                ProbeVerdict::Skipped(format!("SQL детей доски: {e}")),
            )
        }
    };
    let bound_ids: HashSet<String> = rows.iter().filter_map(|r| r.body_doc.clone()).collect();
    let row_titles: HashSet<String> = rows
        .iter()
        .map(|r| syt::normalize_title(&r.title))
        .collect();
    let orphans = orphan_candidates(&children, &bound_ids, &row_titles);
    let detail = format!("детей-доков {}", children.len());
    if orphans.is_empty() {
        return ProbeOutcome::new(BOARD_ORPHANS, ProbeVerdict::Clean).with_detail(detail);
    }
    ProbeOutcome::new(
        BOARD_ORPHANS,
        ProbeVerdict::Observed(format!(
            "кандидаты-сироты (тело без строки): {} — {}",
            orphans.len(),
            orphan_sample(&orphans, 5)
        )),
    )
    .with_detail(detail)
}

async fn fs_sql_probe(sy: &SiYuan) -> ProbeOutcome {
    let notebooks = match sy.post_probe("/api/notebook/lsNotebooks", json!({})).await {
        Ok(notebooks) => notebooks,
        Err(e) => {
            return ProbeOutcome::new(FS_SQL, ProbeVerdict::Skipped(format!("lsNotebooks: {e}")))
        }
    };
    let open = notebooks
        .pointer("/data/notebooks")
        .and_then(|d| d.as_array())
        .map(|list| {
            list.iter()
                .filter(|nb| nb.get("closed").and_then(|c| c.as_bool()) != Some(true))
                .filter_map(|nb| {
                    let id = nb.get("id")?.as_str()?.to_string();
                    let name = nb.get("name")?.as_str()?.to_string();
                    Some((id, name))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let data_dir = workspace_root().join("data");
    if !data_dir.is_dir() {
        return ProbeOutcome::new(
            FS_SQL,
            ProbeVerdict::Skipped(format!(
                "нет локального воркспейса {} — FS-проба пропущена (мульти-PC)",
                data_dir.display()
            )),
        );
    }

    let mut mismatches = Vec::new();
    let mut compared = 0usize;
    for (id, name) in open {
        let box_dir = data_dir.join(&id);
        let fs_ids = match collect_sy_ids(&box_dir) {
            Ok(ids) => ids,
            Err(e) => {
                mismatches.push(format!("{name}: FS недоступен ({e})"));
                continue;
            }
        };
        // id ноутбука генерируется ядром ([0-9a-z-]), кавычек не несёт.
        let stmt = format!(
            "select count(*) as c from blocks where box='{id}' and type='d' and id=root_id"
        );
        let sql_count = match sy
            .post_probe("/api/query/sql", json!({ "stmt": stmt }))
            .await
        {
            Ok(envelope) => envelope.pointer("/data/0/c").and_then(|c| c.as_u64()),
            Err(e) => {
                mismatches.push(format!("{name}: SQL недоступен ({e})"));
                continue;
            }
        };
        let Some(sql_count) = sql_count else {
            mismatches.push(format!("{name}: SQL вернул конверт без счётчика"));
            continue;
        };
        compared += 1;
        if let Some(mismatch) = fs_sql_pair_verdict(&name, fs_ids.len() as u64, sql_count) {
            // Оператору нужна локализация сирот, не только факт: детальный
            // diff списков id (обещание approve-плана раунда 2).
            match sql_doc_ids(sy, &id).await {
                Ok(sql_ids) => {
                    let detail = diff_id_lists(&fs_ids, &sql_ids, 5);
                    mismatches.push(format!("{mismatch} — {detail}"));
                }
                Err(e) => mismatches.push(format!("{mismatch} (детальный diff недоступен: {e})")),
            }
        }
    }
    if compared == 0 {
        return ProbeOutcome::new(
            FS_SQL,
            ProbeVerdict::Skipped("нет открытых ноутбуков для сравнения".into()),
        );
    }
    if mismatches.is_empty() {
        ProbeOutcome::new(FS_SQL, ProbeVerdict::Clean)
    } else {
        ProbeOutcome::new(FS_SQL, ProbeVerdict::Degraded(mismatches.join("; ")))
    }
}

/// Событие сноса из FS-истории ядра: docId — стем .sy файла, тайтл —
/// Properties.title стаба (стаб без Content: ключи ID, Spec, Type,
/// Properties, Children); snapshot_at — mtime каталога снапшота (окно
/// сноса) для порядка сводки — свежие первыми.
#[derive(Debug)]
pub(crate) struct DeleteEvent {
    pub doc_id: String,
    pub title: Option<String>,
    pub snapshot_at: SystemTime,
    /// Id-цепочка пути снапшота от каталога снапшота до снесённого дока.
    /// Живой SQL снесённый док уже не вернёт — путь единственный свидетель
    /// принадлежности поддереву доски задач (ADR-072).
    pub path_ids: Vec<String>,
}

/// Каталог снапшота истории `<YYYY-MM-DD-HHMMSS>-<op>`; события сноса —
/// суффикс `-delete` (объектные снапшоты — `-update`, не сносы).
pub(crate) fn is_delete_snapshot_dir(name: &str) -> bool {
    name.ends_with("-delete")
}

/// Свежесть события по mtime (POSIX-время — TZ-независимо: имя каталога
/// пишет ядро в своей таймзоне, не хоста). mtime в будущем (дрейф
/// часов) — считаем свежим: наблюдение не теряет событие из-за сдвига.
pub(crate) fn delete_in_window(mtime: SystemTime, now: SystemTime, window: Duration) -> bool {
    now.duration_since(mtime)
        .map(|age| age <= window)
        .unwrap_or(true)
}

/// Тайтл снесённого дока: `Properties.title` .sy стаба. Кривой JSON или
/// отсутствие поля — None (событие без тайтла), сбор не роняем.
fn delete_title(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.pointer("/Properties/title")?
        .as_str()
        .map(String::from)
}

/// Рекурсивный сбор событий из каталога снапшота (внутри — дерево
/// ноутбук/родители/лист); chain — накопленная id-цепочка предков.
fn collect_snapshot_events(
    dir: &std::path::Path,
    snapshot_at: SystemTime,
    chain: &[String],
    events: &mut Vec<DeleteEvent>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let mut child_chain = chain.to_vec();
            child_chain.push(entry.file_name().to_string_lossy().into_owned());
            collect_snapshot_events(&path, snapshot_at, &child_chain, events)?;
        } else if path.extension().is_some_and(|ext| ext == "sy") {
            let doc_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let mut path_ids = chain.to_vec();
            path_ids.push(doc_id.clone());
            events.push(DeleteEvent {
                doc_id,
                title: delete_title(&path),
                snapshot_at,
                path_ids,
            });
        }
    }
    Ok(())
}

/// Свежие сносы первыми: порядок OBSERVE-строки читабелен и устойчив —
/// свежее событие не затерто хвостом старых.
pub(crate) fn sort_events_newest_first(events: &mut [DeleteEvent]) {
    events.sort_by_key(|e| std::cmp::Reverse(e.snapshot_at));
}

/// Сбор событий сноса из каталога истории воркспейса: каталоги
/// `*-delete` первого уровня с mtime в окне. Каждый removeDoc ядра
/// (UI, raw API, task rm — мимо CLI-guard в том числе) кладёт такой
/// каталог со снапшотами снесённого.
pub(crate) fn collect_delete_events(
    history_dir: &std::path::Path,
    now: SystemTime,
    window: Duration,
) -> std::io::Result<Vec<DeleteEvent>> {
    let mut events = Vec::new();
    for entry in std::fs::read_dir(history_dir)? {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let is_delete = entry
            .file_name()
            .to_str()
            .is_some_and(is_delete_snapshot_dir);
        let meta = entry.metadata()?;
        if !is_delete || !delete_in_window(meta.modified()?, now, window) {
            continue;
        }
        collect_snapshot_events(&entry.path(), meta.modified()?, &[], &mut events)?;
    }
    Ok(events)
}

/// Снос поддерева доски задач: доска-предок в цепочке пути снапшота.
/// Реестр — живые доски на момент прогона: снесённая доска в него не
/// попадает, её собственный снос уходит в прочий оборот — событие не
/// теряется, лишь не получает задачную метку.
pub(crate) fn is_task_board_delete(event: &DeleteEvent, boards: &HashSet<String>) -> bool {
    event.path_ids.iter().any(|id| boards.contains(id))
}

/// Реестр досок для классификации сносов. Classified — живой SQL-дискавери
/// удался; Unavailable — реестра нет (ядро мертво или сбой дискавери) —
/// деградация к полному перечислению без разметки.
pub(crate) enum TaskBoards {
    Classified(HashSet<String>),
    Unavailable(String),
}

/// Формат события в OBSERVE-строке.
fn format_event(e: &DeleteEvent) -> String {
    match &e.title {
        Some(title) => format!("{} «{title}»", e.doc_id),
        None => format!("{} (без тайтла)", e.doc_id),
    }
}

/// Полный перечень событий через "; " — деградационная ветка форматтера:
/// без классификации все события равнозначны, канал обязан работать целиком
/// (без cap: оборот 100+/сутки не срезается).
pub(crate) fn format_event_list(events: &[DeleteEvent]) -> String {
    events
        .iter()
        .map(format_event)
        .collect::<Vec<_>>()
        .join("; ")
}

/// OBSERVE-строка пробы. Classified: задачные сносы — полным перечнем
/// (канал «снос тела задачи → сводка» работает целиком, без cap), прочий
/// оборот — счётчиком. Unavailable: все события перечислением, как до
/// классификации, с честной пометкой причины.
pub(crate) fn format_delete_observation(events: &[DeleteEvent], boards: &TaskBoards) -> String {
    match boards {
        TaskBoards::Classified(boards) => {
            let task_deletes: Vec<String> = events
                .iter()
                .filter(|e| is_task_board_delete(e, boards))
                .map(format_event)
                .collect();
            let other_count = events.len() - task_deletes.len();
            let text = if task_deletes.is_empty() {
                if other_count > 0 {
                    format!("сносы тел задач за окно: нет (прочий оборот: {other_count})")
                } else {
                    "сносы тел задач за окно: нет".to_string()
                }
            } else {
                let mut text = format!(
                    "сносы тел задач за окно: {} — {}",
                    task_deletes.len(),
                    task_deletes.join("; ")
                );
                if other_count > 0 {
                    text.push_str(&format!("; прочий оборот: {other_count}"));
                }
                text
            };
            text
        }
        TaskBoards::Unavailable(reason) => format!(
            "сносы доков за окно: {} — {} (классификация недоступна: {reason})",
            events.len(),
            format_event_list(events)
        ),
    }
}

/// Сносы доков за окно из истории воркспейса `<ws>/history`. Проба
/// чисто FS: работает и при лежащем ядре, закрывает класс «снос мимо
/// CLI-guard без следа» — undo-журнал cli такие сносы не пишет.
/// Сносы — события/осадок, не деградация: OBSERVE, exit не трогает.
fn history_deletes_probe(task_boards: &TaskBoards) -> ProbeOutcome {
    let history_dir = workspace_root().join("history");
    if !history_dir.is_dir() {
        return ProbeOutcome::new(
            HISTORY_DELETES,
            ProbeVerdict::Skipped(format!(
                "нет истории ядра {} (мульти-PC или не создавалась)",
                history_dir.display()
            )),
        );
    }
    let mut events =
        match collect_delete_events(&history_dir, SystemTime::now(), HISTORY_DELETES_WINDOW) {
            Ok(events) => events,
            Err(e) => {
                return ProbeOutcome::new(
                    HISTORY_DELETES,
                    ProbeVerdict::Skipped(format!("чтение истории: {e}")),
                )
            }
        };
    sort_events_newest_first(&mut events);
    if events.is_empty() {
        return ProbeOutcome::new(HISTORY_DELETES, ProbeVerdict::Clean)
            .with_detail("сносов в окне нет".into());
    }
    ProbeOutcome::new(
        HISTORY_DELETES,
        ProbeVerdict::Observed(format_delete_observation(&events, task_boards)),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        collect_delete_events, collect_sy_ids, delete_in_window, diff_id_lists, doc_updated_date,
        doctor_exit_code, format_delete_observation, fs_sql_pair_verdict, is_delete_snapshot_dir,
        is_task_board_delete, orphan_candidates, orphan_sample, report, scan_kernel_log_for_waves,
        sort_events_newest_first, wave_verdict, BoardChildDoc, ProbeOutcome, ProbeVerdict,
        TaskBoards,
    };
    use crate::siyuan_task as syt;
    use std::collections::HashSet;
    use std::path::Path;
    use std::time::Duration;

    fn child(id: &str, hpath: &str, updated: Option<&str>) -> BoardChildDoc {
        BoardChildDoc {
            id: id.to_string(),
            hpath: hpath.to_string(),
            updated: updated.map(String::from),
        }
    }

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn doc_updated_date_formats_first_eight_digits() {
        let ial = r#"{: type="doc" updated="20261108010134" id="20261104033524-4d37yk0"}"#;
        assert_eq!(doc_updated_date(ial).as_deref(), Some("2026-11-08"));
    }

    #[test]
    fn doc_updated_date_is_none_without_field() {
        assert!(doc_updated_date(r#"{: type="doc" id="x"}"#).is_none());
    }

    #[test]
    fn orphan_candidates_lists_doc_without_row() {
        let children = vec![
            child("c-bound", "/uwuwu/Задачи uwuwu/привязанная задача", None),
            child("c-orphan", "/uwuwu/Задачи uwuwu/тело без строки", None),
        ];
        let orphans =
            orphan_candidates(&children, &set(&["c-bound"]), &set(&["привязанная задача"]));
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].id, "c-orphan");
    }

    #[test]
    fn orphan_candidates_respects_title_fallback_of_detached_rows() {
        // Легаси/detached строка живёт без привязки — её тело не сирота.
        let children = vec![child("c-legacy", "/uwuwu/Задачи uwuwu/легаси задача", None)];
        let orphans = orphan_candidates(&children, &HashSet::new(), &set(&["легаси задача"]));
        assert!(orphans.is_empty());
    }

    #[test]
    fn orphan_candidates_normalizes_slash_title_variants() {
        // Тайтл строки с ASCII-слэшем и тело с ∕ (U+2215) — одна задача;
        // row_titles строится как в пробе — через normalize_title.
        let children = vec![child(
            "c-slash",
            "/uwuwu/Задачи uwuwu/wiki_ops create: слэш в∕тайтле",
            None,
        )];
        let row_titles = HashSet::from([syt::normalize_title("wiki_ops create: слэш в/тайтле")]);
        let orphans = orphan_candidates(&children, &HashSet::new(), &row_titles);
        assert!(orphans.is_empty());
    }

    #[test]
    fn orphan_sample_cap_prints_head_and_tail_count() {
        let orphans: Vec<BoardChildDoc> = (1..=4)
            .map(|n| child(&format!("c{n}"), &format!("/b/тело{n}"), Some("2026-11-08")))
            .collect();
        let refs: Vec<&BoardChildDoc> = orphans.iter().collect();
        let text = orphan_sample(&refs, 2);
        assert!(text.contains("c1 «тело1» (2026-11-08)"), "{text}");
        assert!(text.contains("c2 «тело2» (2026-11-08)"), "{text}");
        assert!(text.contains("…+2"), "{text}");
        assert!(!text.contains("тело3"), "{text}");
    }

    /// Сироты — наблюдение: exit-код остаётся чистым (сегмент волны
    /// не портится находками осадка).
    #[test]
    fn observed_verdict_keeps_exit_code_clean() {
        let outcomes = vec![
            ProbeOutcome::new("kernel", ProbeVerdict::Clean),
            ProbeOutcome::new(
                "board-orphans",
                ProbeVerdict::Observed("кандидаты-сироты: 1 — c1 «t»".into()),
            ),
        ];
        assert_eq!(doctor_exit_code(&outcomes), 0);
        let text = report(&outcomes);
        assert!(text.contains("OBSERVE board-orphans"), "{text}");
        assert!(text.contains("вердикт: 0"), "{text}");
    }

    #[test]
    fn wave_scan_counts_both_signatures() {
        let log = "E 2026/11/05 07:02:55 transaction.go:612: commit tx failed: document metadata is not ready\n\
                   D 2026/11/05 07:03:00 websocket.go:35: websocket failed\n\
                   E 2026/11/05 07:03:00 transaction.go:612: commit tx failed: tree not found\n";
        let scan = scan_kernel_log_for_waves(log);
        assert_eq!(scan.commit_failures, 2);
        assert_eq!(scan.metadata_failures, 1);
    }

    #[test]
    fn wave_scan_clean_log_is_zero() {
        let scan =
            scan_kernel_log_for_waves("I 2026/11/05 07:12:39 plugin.go:278: loaded petals\n");
        assert_eq!(scan.commit_failures, 0);
        assert_eq!(scan.metadata_failures, 0);
    }

    #[test]
    fn fs_sql_pair_reports_only_mismatch() {
        assert!(fs_sql_pair_verdict("Projects", 511, 511).is_none());
        assert_eq!(
            fs_sql_pair_verdict("Projects", 510, 511).as_deref(),
            Some("Projects: FS 510 ↔ SQL 511")
        );
    }

    #[test]
    fn collect_sy_ids_walks_nested_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let box_dir = dir.path().join("box");
        std::fs::create_dir_all(box_dir.join("sub/deep")).expect("mkdir");
        for path in ["a.sy", "sub/b.sy", "sub/deep/c.sy", "notes.txt"] {
            std::fs::write(box_dir.join(path), "x").expect("write");
        }
        let mut ids = collect_sy_ids(&box_dir).expect("walk");
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c"]);
        assert!(collect_sy_ids(Path::new("/nonexistent-box-dir")).is_err());
    }

    #[test]
    fn diff_id_lists_samples_both_directions_with_tail_count() {
        let fs: Vec<String> = ["f1", "shared", "f2", "f3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let sql: Vec<String> = ["shared", "s1", "s2", "s3", "s4", "s5"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let text = diff_id_lists(&fs, &sql, 2);
        assert!(text.contains("только ФС [f1, f2 …+1]"), "{text}");
        assert!(text.contains("только SQL [s1, s2 …+3]"), "{text}");
    }

    #[test]
    fn diff_id_lists_empty_sides_stay_visible() {
        let text = diff_id_lists(&[], &["s1".to_string()], 5);
        assert_eq!(text, "только ФС []; только SQL [s1]");
    }

    #[test]
    fn wave_verdict_single_failures_are_noise_not_wave() {
        let scan = scan_kernel_log_for_waves("commit tx failed: x\n");
        let (verdict, detail) = wave_verdict(&scan, 30);
        assert!(matches!(verdict, ProbeVerdict::Clean));
        assert!(detail.expect("noise note").contains("шум"));
    }

    #[test]
    fn wave_verdict_threshold_crossing_is_degradation() {
        let log = "commit tx failed\n".repeat(5);
        let scan = scan_kernel_log_for_waves(&log);
        let (verdict, detail) = wave_verdict(&scan, 30);
        assert!(matches!(verdict, ProbeVerdict::Degraded(_)));
        assert!(detail.is_none());
        let ProbeVerdict::Degraded(msg) = verdict else {
            unreachable!()
        };
        assert!(msg.contains("агентам пауза 2–5 мин"));
    }

    #[test]
    fn wave_verdict_clean_log_is_clean_without_detail() {
        let scan = scan_kernel_log_for_waves("");
        let (verdict, detail) = wave_verdict(&scan, 30);
        assert!(matches!(verdict, ProbeVerdict::Clean));
        assert!(detail.is_none());
    }

    /// Недоступное ядро — деградация (exit 1), не «проба не состоялась»:
    /// маппинг зафиксирован по замечанию ревью раунда 2.
    #[test]
    fn kernel_transport_failure_maps_to_degradation_exit() {
        let outcomes = vec![ProbeOutcome::new(
            "kernel",
            ProbeVerdict::Degraded("kernel недоступен: connect refused".into()),
        )];
        assert_eq!(doctor_exit_code(&outcomes), 1);
    }

    #[test]
    fn clean_probe_with_skipped_ones_exits_zero() {
        let outcomes = vec![
            ProbeOutcome::new("kernel", ProbeVerdict::Clean),
            ProbeOutcome::new("wave", ProbeVerdict::Skipped("docker недоступен".into())),
        ];
        assert_eq!(doctor_exit_code(&outcomes), 0);
    }

    #[test]
    fn all_skipped_probes_exit_two() {
        let outcomes = vec![ProbeOutcome::new(
            "board",
            ProbeVerdict::Skipped("kernel недоступен".into()),
        )];
        assert_eq!(doctor_exit_code(&outcomes), 2);
    }

    #[test]
    fn report_marks_each_verdict_kind_and_detail() {
        let outcomes = vec![
            ProbeOutcome::new("kernel", ProbeVerdict::Clean),
            ProbeOutcome::new(
                "board",
                ProbeVerdict::Degraded("«t»: пустое тело (ZWJ-заглушка)".into()),
            )
            .with_detail("строк 5, легаси без привязки 2".into()),
            ProbeOutcome::new("wave", ProbeVerdict::Skipped("docker недоступен".into())),
        ];
        let text = report(&outcomes);
        assert!(text.contains("OK      kernel"));
        assert!(text.contains("DEGRADED board"));
        assert!(text.contains("SKIP    wave"));
        assert!(text.contains("[строк 5, легаси без привязки 2]"));
        assert!(text.contains("вердикт: 1"));
    }

    #[test]
    fn delete_snapshot_dir_matches_only_delete_suffix() {
        assert!(is_delete_snapshot_dir("2026-11-08-021321-delete"));
        assert!(!is_delete_snapshot_dir("2026-11-08-010616-update"));
        assert!(!is_delete_snapshot_dir("storage"));
    }

    #[test]
    fn delete_in_window_filters_by_age() {
        let now = std::time::SystemTime::now();
        let fresh = now - Duration::from_secs(3600);
        let stale = std::time::SystemTime::UNIX_EPOCH;
        let window = Duration::from_secs(24 * 3600);
        assert!(delete_in_window(fresh, now, window));
        assert!(!delete_in_window(stale, now, window));
        // mtime в будущем (дрейф часов) — событие свежим, не теряется.
        assert!(delete_in_window(now + Duration::from_secs(60), now, window));
    }

    /// Стаб .sy по живому набору ключей: ID, Spec, Type, Properties,
    /// Children (Content у стаба нет вовсе).
    fn write_delete_snapshot(history: &Path, dir: &str, doc_id: &str, title: Option<&str>) {
        let snapshot = history.join(dir);
        std::fs::create_dir_all(snapshot.join("nb/parents")).expect("mkdir");
        let props = match title {
            Some(t) => format!(r#"{{"title":"{t}"}}"#),
            None => r#"{}"#.to_string(),
        };
        let stub = format!(
            r#"{{"ID":"{doc_id}","Spec":1,"Type":"NodeDocument","Properties":{props},"Children":[]}}"#
        );
        std::fs::write(snapshot.join(format!("nb/parents/{doc_id}.sy")), stub).expect("write");
    }

    /// Невалидный JSON в стабе (обрыв записи ядра) — событие без тайтла,
    /// сбор не падает.
    fn write_broken_snapshot(history: &Path, dir: &str, doc_id: &str) {
        let snapshot = history.join(dir);
        std::fs::create_dir_all(snapshot.join("nb")).expect("mkdir");
        let broken = format!(r#"{{"ID":"{doc_id}"#);
        std::fs::write(snapshot.join(format!("nb/{doc_id}.sy")), broken).expect("write");
    }

    #[test]
    fn collect_delete_events_reads_stubs_of_fresh_delete_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let history = dir.path().join("history");
        write_delete_snapshot(
            &history,
            "2026-11-08-021321-delete",
            "20261104033524-4d37yk0",
            Some("SiYuan: таймер бэкапов воркспейса (rsync-restic)"),
        );
        write_delete_snapshot(
            &history,
            "2026-11-08-021322-delete",
            "20261104112902-uh1u34q",
            None,
        );
        // update-снапшот — не событие сноса.
        std::fs::create_dir_all(history.join("2026-11-08-010616-update/nb")).expect("mkdir");
        std::fs::write(
            history.join("2026-11-08-010616-update/nb/20261108000000-x.sy"),
            "{}",
        )
        .expect("write");

        let events = collect_delete_events(
            &history,
            std::time::SystemTime::now(),
            Duration::from_secs(3600),
        )
        .expect("collect");
        assert_eq!(events.len(), 2, "{events:?}");
        let lost = events
            .iter()
            .find(|e| e.doc_id == "20261104033524-4d37yk0")
            .expect("lost doc event");
        assert_eq!(
            lost.title.as_deref(),
            Some("SiYuan: таймер бэкапов воркспейса (rsync-restic)")
        );
        // Id-цепочка пути снапшота — свидетель принадлежности поддереву
        // доски (стаб в nb/parents/<id>.sy → "nb", "parents", docId).
        assert_eq!(
            lost.path_ids,
            ["nb", "parents", "20261104033524-4d37yk0"],
            "{:?}",
            lost.path_ids
        );
        let untitled = events
            .iter()
            .find(|e| e.doc_id == "20261104112902-uh1u34q")
            .expect("untitled event");
        assert!(untitled.title.is_none());
    }

    /// Фильтр окна — по mtime, не по имени каталога: старое имя при
    /// свежем mtime включается (имя пишет ядро в своей таймзоне, окно
    /// считает POSIX-время). «Вне окна» покрыто юнитом delete_in_window.
    #[test]
    fn collect_delete_events_filters_by_mtime_not_dir_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let history = dir.path().join("history");
        write_delete_snapshot(
            &history,
            "2026-01-01-000000-delete",
            "20261101000000-old",
            Some("старое имя, свежий mtime"),
        );
        std::fs::write(history.join("loose.txt"), "x").expect("write");

        let events = collect_delete_events(
            &history,
            std::time::SystemTime::now(),
            Duration::from_secs(3600),
        )
        .expect("collect");
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].doc_id, "20261101000000-old");
    }

    #[test]
    fn collect_delete_events_fails_loud_on_missing_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("history");
        assert!(collect_delete_events(
            &missing,
            std::time::SystemTime::now(),
            Duration::from_secs(3600)
        )
        .is_err());
    }

    #[test]
    fn collect_delete_events_reads_broken_stub_without_title() {
        // Кривой JSON стаба (обрыв записи) — событие сохраняется, тайтл
        // отсутствует: сбор не падает на частичной записи ядра.
        let dir = tempfile::tempdir().expect("tempdir");
        let history = dir.path().join("history");
        write_broken_snapshot(
            &history,
            "2026-11-08-021321-delete",
            "20261104033524-4d37yk0",
        );

        let events = collect_delete_events(
            &history,
            std::time::SystemTime::now(),
            Duration::from_secs(3600),
        )
        .expect("collect");
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].doc_id, "20261104033524-4d37yk0");
        assert!(events[0].title.is_none());
    }

    #[test]
    fn format_delete_observation_unavailable_lists_every_event_without_cap() {
        // Деградационная ветка (ядро мертво, реестра нет): канал «событие
        // сноса → сводка» обязан работать при реальном обороте — перечисление
        // без cap, свежий снос тела задачи не срезается (ревью: инцидент на
        // 71-й позиции из 107).
        let events: Vec<super::DeleteEvent> = (1..=70)
            .map(|n| delete_event(&format!("doc{n}"), Some(&format!("тайтл{n}")), &["nb"]))
            .chain([delete_event(
                "lost",
                Some("таймер бэкапов"),
                &["nb", "board", "lost"],
            )])
            .collect();
        let text = format_delete_observation(&events, &TaskBoards::Unavailable("offline".into()));
        assert!(text.contains("сносы доков за окно: 71"), "{text}");
        assert!(text.contains("lost «таймер бэкапов»"), "{text}");
        assert!(text.contains("doc1 «тайтл1»"), "{text}");
        assert!(text.contains("doc70 «тайтл70»"), "{text}");
        assert!(
            text.contains("классификация недоступна: offline"),
            "причина деградации названа: {text}"
        );
        assert!(!text.contains("…+"), "без cap нет хвостовой метки: {text}");
    }

    #[test]
    fn format_delete_observation_marks_task_body_delete_in_classified_mode() {
        // Инцидентный сценарий в Classified-режиме: снос тела задачи (доска
        // в цепочке пути) — отдельная видимая группа, прочий оборот — счётчик.
        let events: Vec<super::DeleteEvent> = (1..=70)
            .map(|n| delete_event(&format!("doc{n}"), Some(&format!("тайтл{n}")), &["nb"]))
            .chain([delete_event(
                "lost",
                Some("таймер бэкапов"),
                &["nb", "20261103233646-board", "lost"],
            )])
            .collect();
        let boards: HashSet<String> = ["20261103233646-board".to_string()].into_iter().collect();
        let text = format_delete_observation(&events, &TaskBoards::Classified(boards));
        assert!(text.contains("сносы тел задач за окно: 1"), "{text}");
        assert!(text.contains("lost «таймер бэкапов»"), "{text}");
        assert!(text.contains("прочий оборот: 70"), "{text}");
        assert!(!text.contains("doc70"), "прочий оборот — счётчиком: {text}");
    }

    #[test]
    fn format_delete_observation_groups_all_task_deletes_and_omits_empty_other() {
        // Все сносы задачные — «прочий оборот: 0» не шумит хвостом.
        let events = vec![
            delete_event("b1", Some("тело 1"), &["nb", "board", "b1"]),
            delete_event("b2", None, &["nb", "board", "nested", "b2"]),
        ];
        let boards: HashSet<String> = ["board".to_string()].into_iter().collect();
        let text = format_delete_observation(&events, &TaskBoards::Classified(boards));
        assert_eq!(
            text,
            "сносы тел задач за окно: 2 — b1 «тело 1»; b2 (без тайтла)"
        );
    }

    #[test]
    fn format_delete_observation_reports_no_task_deletes_with_other_counter() {
        let events = vec![
            delete_event("x1", None, &["nb", "parents", "x1"]),
            delete_event("x2", None, &["nb", "parents", "x2"]),
        ];
        let boards: HashSet<String> = ["board".to_string()].into_iter().collect();
        let text = format_delete_observation(&events, &TaskBoards::Classified(boards));
        assert_eq!(text, "сносы тел задач за окно: нет (прочий оборот: 2)");
    }

    #[test]
    fn format_delete_observation_empty_window_reports_no_deletes() {
        // Тихая доска: ни задачных, ни прочих — «нет» без счётного хвоста.
        let boards: HashSet<String> = ["board".to_string()].into_iter().collect();
        let text = format_delete_observation(&[], &TaskBoards::Classified(boards));
        assert_eq!(text, "сносы тел задач за окно: нет");
    }

    #[test]
    fn is_task_board_delete_matches_any_chain_link() {
        // Любое звено цепочки (доска-предок в середине) — задачный снос;
        // цепочка без доски реестра — прочий оборот. Лист — тоже звено
        // (юнит механики any-match; снесённая доска в живом реестре
        // отсутствует, её снос классификацией не ловится — граница плана).
        let under_board = delete_event(
            "body1",
            None,
            &["nb", "20261103233646-board", "nested", "body1"],
        );
        let boards: HashSet<String> = ["20261103233646-board".to_string()].into_iter().collect();
        assert!(is_task_board_delete(&under_board, &boards));

        let stray = delete_event("stray", None, &["nb", "parents", "stray"]);
        assert!(!is_task_board_delete(&stray, &boards));
    }

    fn delete_event(doc_id: &str, title: Option<&str>, chain: &[&str]) -> super::DeleteEvent {
        super::DeleteEvent {
            doc_id: doc_id.to_string(),
            title: title.map(String::from),
            snapshot_at: std::time::SystemTime::UNIX_EPOCH,
            path_ids: chain.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn sort_events_newest_first_puts_fresh_snapshot_ahead() {
        let now = std::time::SystemTime::now();
        let mut events = vec![
            delete_event("old", None, &["nb", "old"]),
            delete_event("fresh", None, &["nb", "fresh"]),
        ];
        events[0].snapshot_at = now - Duration::from_secs(3600);
        events[1].snapshot_at = now;
        sort_events_newest_first(&mut events);
        assert_eq!(events[0].doc_id, "fresh");
        assert_eq!(events[1].doc_id, "old");
    }
}
