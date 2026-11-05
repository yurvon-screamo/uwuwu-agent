//! Тесты kernel-проб: clap-грамматика шести сабкоманд и чистые функции
//! диспетчера (ls-маркеры). Транспорт post_probe (one-shot, error contract) —
//! wiremock-тесты в siyuan.rs.

use super::cli::{Cli, Commands, KernelSub};
use super::sy_dispatch::ls_lines;
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
fn render_parses_ids_and_page() {
    let KernelSub::Render { av_id, block_id, page } = expect_kernel(parse_kernel(&[
        "render",
        "20261104102403-kernel1",
        "20261104102403-block02",
        "--page",
        "2",
    ]).unwrap()) else {
        panic!("expected render subcommand");
    };
    assert_eq!(av_id, "20261104102403-kernel1");
    assert_eq!(block_id, "20261104102403-block02");
    assert_eq!(page, 2);
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
        vec!["doc.sy".to_string(), "assets/".to_string(), "no-flag.png".to_string()]
    );
}

#[test]
fn ls_lines_of_empty_dir_is_empty() {
    assert!(ls_lines(&[]).is_empty());
}
