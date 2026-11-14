//! Грамматика флага `--autodone` на create/edit-поверхности (задача
//! 1050, дефолт on): парсер — единственный слой, который не проходит
//! ни юниты store (fm-конструирование), ни живой e2e-риг (зовёт
//! store.create напрямую, минуя clap). Регрессия атрибутов флага
//! (num_args/default_missing_value/тип) ломает пользовательский путь
//! до store — здесь она валится.

use crate::cmd::cli::{Cli, Commands, TaskSub};
use clap::{CommandFactory as _, Parser as _};

fn parse(args: &[&str]) -> Cli {
    let mut full: Vec<&str> = vec!["uwuwu-cli"];
    full.extend_from_slice(args);
    Cli::try_parse_from(full).expect("парсер не отказал")
}

fn create_autodone(cli: Cli) -> Option<String> {
    match cli.command {
        Commands::Task { command, .. } => match command {
            TaskSub::Create { autodone, .. } => autodone,
            _ => panic!("ожидали TaskSub::Create"),
        },
        _ => panic!("ожидали Commands::Task"),
    }
}

fn edit_autodone(cli: Cli) -> Option<String> {
    match cli.command {
        Commands::Task { command, .. } => match command {
            TaskSub::Edit { autodone, .. } => autodone,
            _ => panic!("ожидали TaskSub::Edit"),
        },
        _ => panic!("ожидали Commands::Task"),
    }
}

#[test]
fn grammar_is_valid() {
    Cli::command().debug_assert();
}

#[test]
fn create_without_flag_parses_to_none() {
    // None — store-дефолт on; флаг не ложится в CreateTask вообще.
    let cli = parse(&["task", "create", "--title", "t"]);
    assert_eq!(create_autodone(cli), None);
}

#[test]
fn create_bare_flag_parses_to_on() {
    let cli = parse(&["task", "create", "--title", "t", "--autodone"]);
    assert_eq!(create_autodone(cli).as_deref(), Some("on"));
}

#[test]
fn create_explicit_values_pass_through() {
    let on = parse(&["task", "create", "--title", "t", "--autodone", "on"]);
    assert_eq!(create_autodone(on).as_deref(), Some("on"));
    let off = parse(&["task", "create", "--title", "t", "--autodone", "off"]);
    assert_eq!(create_autodone(off).as_deref(), Some("off"));
}

#[test]
fn edit_flag_keeps_create_parity() {
    let bare = parse(&["task", "edit", "123", "--autodone"]);
    assert_eq!(edit_autodone(bare).as_deref(), Some("on"));
    let off = parse(&["task", "edit", "123", "--autodone", "off"]);
    assert_eq!(edit_autodone(off).as_deref(), Some("off"));
}
