//! Parse-тесты грамматики `task body`: явный --stdin, конфликт с --md,
//! дефолт без флагов. Чистый clap-парсинг — без сети и env.

use super::{Cli, Commands, TaskSub};
use clap::Parser as _;

fn parse_command(args: &[&str]) -> clap::error::Result<Commands> {
    let mut argv = vec!["uwuwu-cli"];
    argv.extend_from_slice(args);
    Ok(Cli::try_parse_from(argv)?.command)
}

fn expect_body(command: Commands) -> (bool, Option<String>) {
    let Commands::Task {
        command: TaskSub::Body { stdin, md, .. },
        ..
    } = command
    else {
        panic!("expected task body subcommand");
    };
    (stdin, md)
}

#[test]
fn body_stdin_flag_parses_explicitly() {
    let command =
        parse_command(&["task", "--project", "uwuwu", "body", "abc123", "--stdin"]).unwrap();
    let (stdin, md) = expect_body(command);
    assert!(stdin);
    assert!(md.is_none());
}

#[test]
fn body_stdin_conflicts_with_md() {
    let parsed = parse_command(&[
        "task",
        "--project",
        "uwuwu",
        "body",
        "abc123",
        "--stdin",
        "--md",
        "f.md",
    ]);
    let Err(err) = parsed else {
        panic!("expected parse error for --stdin together with --md");
    };
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn body_md_parses_without_stdin() {
    let command = parse_command(&[
        "task",
        "--project",
        "uwuwu",
        "body",
        "abc123",
        "--md",
        "f.md",
    ])
    .unwrap();
    let (stdin, md) = expect_body(command);
    assert!(!stdin);
    assert_eq!(md.as_deref(), Some("f.md"));
}

#[test]
fn body_defaults_to_implicit_stdin() {
    let command = parse_command(&["task", "--project", "uwuwu", "body", "abc123"]).unwrap();
    let (stdin, md) = expect_body(command);
    assert!(!stdin);
    assert!(md.is_none());
}
