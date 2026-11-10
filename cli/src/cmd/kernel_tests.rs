//! Тесты kernel-проб: clap-грамматика шести сабкоманд и чистые функции
//! диспетчера (ls-маркеры). Транспорт post_probe (one-shot, error contract) —
//! wiremock-тесты в siyuan.rs.

use super::cli::{Cli, Commands, KernelSub};
use super::mock_kernel::{set_env, MockBoard, MockKernel, MockScenario};
use super::sy_dispatch::ls_lines;
use crate::siyuan::SiYuan;
use crate::siyuan_task::resolve_av;
use clap::Parser as _;
use serde_json::json;

fn parse_kernel(args: &[&str]) -> clap::error::Result<Commands> {
    let mut argv = vec!["uwuwu-cli", "kernel"];
    argv.extend_from_slice(args);
    Ok(Cli::try_parse_from(argv)?.command)
}

fn expect_kernel(command: Commands) -> KernelSub {
    let Commands::Kernel { command, .. } = command else {
        panic!("expected kernel subcommand");
    };
    command
}

#[test]
fn ping_parses() {
    assert!(matches!(
        expect_kernel(parse_kernel(&["ping"]).unwrap()),
        KernelSub::Ping
    ));
}

#[test]
fn sql_parses_stmt() {
    let KernelSub::Sql { stmt } = expect_kernel(parse_kernel(&["sql", "select 1"]).unwrap()) else {
        panic!("expected sql subcommand");
    };
    assert_eq!(stmt, "select 1");
}

#[test]
fn sql_stmt_is_single_argument() {
    // SQL кавычился в шелле; два слова — ошибка грамматики (unexpected argument)
    assert!(parse_kernel(&["sql", "select", "1"]).is_err());
}

#[test]
fn file_parses_path_and_optional_out() {
    let KernelSub::File { path, out } =
        expect_kernel(parse_kernel(&["file", "data/nb/doc.sy", "--out", "f.bin"]).unwrap())
    else {
        panic!("expected file subcommand");
    };
    assert_eq!(path, "data/nb/doc.sy");
    assert_eq!(out.as_deref(), Some("f.bin"));
}

#[test]
fn export_parses_doc_id() {
    let KernelSub::Export { doc_id } =
        expect_kernel(parse_kernel(&["export", "20261103224557-4yurg8d"]).unwrap())
    else {
        panic!("expected export subcommand");
    };
    assert_eq!(doc_id, "20261103224557-4yurg8d");
}

#[test]
fn render_parses_id_and_page() {
    let KernelSub::Render { id, page } =
        expect_kernel(parse_kernel(&["render", "20261104102403-kernel1", "--page", "2"]).unwrap())
    else {
        panic!("expected render subcommand");
    };
    assert_eq!(id, "20261104102403-kernel1");
    assert_eq!(page, 2);
}

// ——— render-проба: one-shot резолв av по id (flow на мок-ядре) ———

/// Доска сценария: нативный av-блок (data-av-id=) или legacy i-носитель (avID=).
fn board_with_carrier(legacy: bool) -> MockScenario {
    MockScenario {
        boards: vec![MockBoard {
            doc_id: "board-doc",
            av_id: "av-1",
            keys: vec![],
            legacy,
        }],
        ..MockScenario::default()
    }
}

#[tokio::test]
#[serial_test::serial]
async fn resolve_av_by_doc_id_reads_native_data_av_id() {
    let (_kernel, server) = MockKernel::start(board_with_carrier(false)).await;
    set_env(&server);
    let sy = SiYuan::from_env().unwrap();
    let (av, block) = resolve_av(&sy, "board-doc").await.unwrap();
    assert_eq!(av, "av-1");
    assert_eq!(block, "block-board-doc");
}

#[tokio::test]
#[serial_test::serial]
async fn resolve_av_by_doc_id_reads_legacy_av_id_carrier() {
    let (_kernel, server) = MockKernel::start(board_with_carrier(true)).await;
    set_env(&server);
    let sy = SiYuan::from_env().unwrap();
    let (av, block) = resolve_av(&sy, "board-doc").await.unwrap();
    assert_eq!(av, "av-1");
    assert_eq!(block, "block-board-doc");
}

#[tokio::test]
#[serial_test::serial]
async fn resolve_av_by_bare_av_id_finds_carrier_block() {
    let (_kernel, server) = MockKernel::start(board_with_carrier(false)).await;
    set_env(&server);
    let sy = SiYuan::from_env().unwrap();
    let (av, block) = resolve_av(&sy, "av-1").await.unwrap();
    assert_eq!(av, "av-1");
    assert_eq!(block, "block-board-doc");
}

#[tokio::test]
#[serial_test::serial]
async fn resolve_av_rejects_id_without_database_relation() {
    let (_kernel, server) = MockKernel::start(board_with_carrier(false)).await;
    set_env(&server);
    let sy = SiYuan::from_env().unwrap();
    let err = resolve_av(&sy, "20261104102403-stranger")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("не резолвится"), "{err}");
}

// ——— верификация маркерного шага (extract == id) ———

/// Пин верификации: блок маркерного SQL, где первый по позиции маркер
/// (`avID=`) указывает на чужой av — такая строка не принимается (без
/// верификации resolve_av вернул бы чужую пару и render отрендерил
/// не ту доску молча).
#[tokio::test]
#[serial_test::serial]
async fn resolve_av_bare_av_id_rejects_marker_row_with_foreign_extract() {
    let mut sc = board_with_carrier(false);
    sc.marker_blocks = vec![(
        "block-mixed",
        "<i avID=\"foreign-av\"></i><div data-av-id=\"av-1\"></div>",
    )];
    let (_kernel, server) = MockKernel::start(sc).await;
    set_env(&server);
    let sy = SiYuan::from_env().unwrap();
    let err = resolve_av(&sy, "av-1").await.unwrap_err();
    assert!(err.to_string().contains("не резолвится"), "{err}");
}

/// Позитивная ветка маркерного шага: extract по первому маркеру совпал
/// с запрошенным avID — пара (avID, blockID носителя) принимается.
#[tokio::test]
#[serial_test::serial]
async fn resolve_av_bare_av_id_takes_matching_marker_row() {
    let mut sc = board_with_carrier(false);
    sc.marker_blocks = vec![(
        "block-quoted",
        "<p>цитата data-av-id=\"av-1\" из документации</p>",
    )];
    let (_kernel, server) = MockKernel::start(sc).await;
    set_env(&server);
    let sy = SiYuan::from_env().unwrap();
    let (av, block) = resolve_av(&sy, "av-1").await.unwrap();
    assert_eq!(av, "av-1");
    assert_eq!(block, "block-quoted");
}

#[test]
fn timeout_flag_parses_before_and_after_subcommand() {
    for argv in [
        &["--timeout", "30", "ping"][..],
        &["ping", "--timeout", "30"][..],
    ] {
        let Commands::Kernel { timeout, .. } = parse_kernel(argv).unwrap() else {
            panic!("expected kernel subcommand");
        };
        assert_eq!(timeout, 30);
    }
}

#[test]
fn timeout_defaults_to_fast_probe() {
    let Commands::Kernel { timeout, .. } = parse_kernel(&["ping"]).unwrap() else {
        panic!("expected kernel subcommand");
    };
    assert_eq!(timeout, 10);
}

#[test]
fn ls_lines_marks_directories_with_trailing_slash() {
    let entries = json!([
        { "name": "doc.sy", "isDir": false },
        { "name": "assets", "isDir": true },
        { "name": "no-flag.png" }
    ]);
    assert_eq!(
        ls_lines(entries.as_array().unwrap()),
        vec![
            "doc.sy".to_string(),
            "assets/".to_string(),
            "no-flag.png".to_string()
        ]
    );
}

#[test]
fn ls_lines_of_empty_dir_is_empty() {
    assert!(ls_lines(&[]).is_empty());
}
