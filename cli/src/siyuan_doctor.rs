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

#[derive(Debug)]
pub(crate) enum ProbeVerdict {
    Clean,
    Degraded(String),
    Skipped(String),
}

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
const FS_SQL: &str = "fs-sql";

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
        fs_sql_probe(sy).await
    } else {
        ProbeOutcome::new(FS_SQL, ProbeVerdict::Skipped("kernel недоступен".into()))
    });

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

#[cfg(test)]
mod tests {
    use super::{
        collect_sy_ids, diff_id_lists, doctor_exit_code, fs_sql_pair_verdict, report,
        scan_kernel_log_for_waves, wave_verdict, ProbeOutcome, ProbeVerdict,
    };
    use std::path::Path;

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
}
