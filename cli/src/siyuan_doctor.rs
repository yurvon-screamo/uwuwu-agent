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

pub(crate) fn scan_kernel_log_for_waves(log: &str) -> WaveScan {
    WaveScan {
        commit_failures: log.lines().filter(|l| l.contains("commit tx failed")).count(),
        metadata_failures: log
            .lines()
            .filter(|l| l.contains("metadata is not ready"))
            .count(),
    }
}

/// Число .sy-документов ноутбука на диске (data/<box>/**).
pub(crate) fn count_sy_files(box_dir: &std::path::Path) -> std::io::Result<u64> {
    let mut count = 0u64;
    let mut stack = vec![box_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "sy") {
                count += 1;
            }
        }
    }
    Ok(count)
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
        ProbeOutcome::new(
            BOARD,
            ProbeVerdict::Skipped("kernel недоступен".into()),
        )
    });

    outcomes.push(if kernel_alive {
        fs_sql_probe(sy).await
    } else {
        ProbeOutcome::new(
            FS_SQL,
            ProbeVerdict::Skipped("kernel недоступен".into()),
        )
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
            if scan.commit_failures == 0 {
                ProbeOutcome::new(WAVE, ProbeVerdict::Clean)
            } else {
                ProbeOutcome::new(
                    WAVE,
                    ProbeVerdict::Degraded(format!(
                        "«commit tx failed» ×{} (metadata ×{}) за {since_min} мин — ядро в волне; \
                         агентам пауза 2–5 мин, рестарт по ранбуку",
                        scan.commit_failures, scan.metadata_failures
                    )),
                )
            }
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
            return ProbeOutcome::new(
                BOARD,
                ProbeVerdict::Skipped(format!("render доски: {e}")),
            )
        }
    };
    let dead = syt::dead_bound_doc_ids(sy, &rows).await.unwrap_or_default();

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
    doc_content_is_empty(&ids, &text)
        .then(|| format!("«{title}»: пустое тело (ZWJ-заглушка)"))
}

async fn fs_sql_probe(sy: &SiYuan) -> ProbeOutcome {
    let notebooks = match sy.post_probe("/api/notebook/lsNotebooks", json!({})).await {
        Ok(notebooks) => notebooks,
        Err(e) => {
            return ProbeOutcome::new(
                FS_SQL,
                ProbeVerdict::Skipped(format!("lsNotebooks: {e}")),
            )
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
        let fs_count = match count_sy_files(&box_dir) {
            Ok(count) => count,
            Err(e) => {
                mismatches.push(format!("{name}: FS недоступен ({e})"));
                continue;
            }
        };
        // id ноутбука генерируется ядром ([0-9a-z-]), кавычек не несёт.
        let stmt =
            format!("select count(*) as c from blocks where box='{id}' and type='d' and id=root_id");
        let sql_count = match sy.post_probe("/api/query/sql", json!({ "stmt": stmt })).await {
            Ok(envelope) => envelope
                .pointer("/data/0/c")
                .and_then(|c| c.as_u64())
                .unwrap_or(u64::MAX),
            Err(e) => {
                mismatches.push(format!("{name}: SQL недоступен ({e})"));
                continue;
            }
        };
        compared += 1;
        if let Some(mismatch) = fs_sql_pair_verdict(&name, fs_count, sql_count) {
            mismatches.push(mismatch);
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
        count_sy_files, doctor_exit_code, fs_sql_pair_verdict, report, scan_kernel_log_for_waves,
        ProbeOutcome, ProbeVerdict,
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
        let scan = scan_kernel_log_for_waves("I 2026/11/05 07:12:39 plugin.go:278: loaded petals\n");
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
    fn count_sy_files_walks_nested_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let box_dir = dir.path().join("box");
        std::fs::create_dir_all(box_dir.join("sub/deep")).expect("mkdir");
        for path in [
            "a.sy",
            "sub/b.sy",
            "sub/deep/c.sy",
            "notes.txt",
        ] {
            std::fs::write(box_dir.join(path), "x").expect("write");
        }
        assert_eq!(count_sy_files(&box_dir).expect("walk"), 3);
        assert!(count_sy_files(Path::new("/nonexistent-box-dir")).is_err());
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
            ProbeOutcome::new(
                "wave",
                ProbeVerdict::Skipped("docker недоступен".into()),
            ),
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
            ProbeOutcome::new(
                "wave",
                ProbeVerdict::Skipped("docker недоступен".into()),
            ),
        ];
        let text = report(&outcomes);
        assert!(text.contains("OK      kernel"));
        assert!(text.contains("DEGRADED board"));
        assert!(text.contains("SKIP    wave"));
        assert!(text.contains("[строк 5, легаси без привязки 2]"));
        assert!(text.contains("вердикт: 1"));
    }
}
