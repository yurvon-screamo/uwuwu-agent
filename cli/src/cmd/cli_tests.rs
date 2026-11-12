//! Parse-тесты грамматики `task body`: явный --stdin, конфликт с --md,
//! дефолт без флагов. Чистый clap-парсинг — без сети и env.

use super::cli::{Cli, Commands, SemanticSub, TaskSub};
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

fn expect_task_id(command: Commands) -> String {
    let Commands::Task { command, .. } = command else {
        panic!("expected task subcommand");
    };
    match command {
        TaskSub::Get { id, .. }
        | TaskSub::Edit { id, .. }
        | TaskSub::Comment { id, .. }
        | TaskSub::Thread { id, .. }
        | TaskSub::Close { id, .. }
        | TaskSub::Body { id, .. }
        | TaskSub::Rm { id } => id,
        _other => panic!("expected id-bearing task subcommand"),
    }
}

#[test]
fn hyphen_leading_id_parses_and_flag_after_id_stays_flag() {
    let command = parse_command(&["task", "get", "-Prb_FZlCA", "--project", "uwuwu"])
        .expect("id с ведущим дефисом парсится");
    let id = expect_task_id(command);
    assert_eq!(id, "-Prb_FZlCA");
    // гоча опыта VCm1mFvNdE: allow_hyphen_values не должен съесть настоящий флаг
    let command = parse_command(&[
        "task",
        "get",
        "-Prb_FZlCA",
        "--no-body",
        "--project",
        "uwuwu",
    ])
    .expect("флаг после id жив");
    let id = expect_task_id(command);
    assert_eq!(id, "-Prb_FZlCA");
}

#[test]
fn type_flag_parses_on_create_edit_and_list() {
    let command = parse_command(&[
        "task",
        "--project",
        "uwuwu",
        "create",
        "--title",
        "т",
        "--type",
        "bug",
    ])
    .unwrap();
    let Commands::Task {
        command: TaskSub::Create { r#type, .. },
        ..
    } = command
    else {
        panic!("expected task create");
    };
    assert_eq!(r#type.as_deref(), Some("bug"));

    let command =
        parse_command(&["task", "--project", "uwuwu", "edit", "abc", "--type", ""]).unwrap();
    let Commands::Task {
        command: TaskSub::Edit { r#type, .. },
        ..
    } = command
    else {
        panic!("expected task edit");
    };
    assert_eq!(r#type.as_deref(), Some(""), "пустой флаг — снятие");

    let command =
        parse_command(&["task", "--project", "uwuwu", "list", "--type", "spike"]).unwrap();
    let Commands::Task {
        command: TaskSub::List { r#type, .. },
        ..
    } = command
    else {
        panic!("expected task list");
    };
    assert_eq!(r#type.as_deref(), Some("spike"));
}

#[test]
fn status_flag_parses_on_create() {
    let command = parse_command(&[
        "task",
        "--project",
        "uwuwu",
        "create",
        "--title",
        "т",
        "--status",
        "Todo",
    ])
    .unwrap();
    let Commands::Task {
        command: TaskSub::Create { status, .. },
        ..
    } = command
    else {
        panic!("expected task create");
    };
    assert_eq!(status.as_deref(), Some("Todo"));

    // без флага — Draft-дефолт (машинный путь не активирован)
    let command = parse_command(&["task", "--project", "uwuwu", "create", "--title", "т"]).unwrap();
    let Commands::Task {
        command: TaskSub::Create { status, .. },
        ..
    } = command
    else {
        panic!("expected task create");
    };
    assert_eq!(status, None);
}

#[test]
fn clean_ids_value_parses_with_hyphen_leading_fragments() {
    let Commands::Task {
        command: TaskSub::Clean { ids, .. },
        ..
    } = parse_command(&["task", "--project", "uwuwu", "clean", "--ids", "-abc,-def"]).unwrap()
    else {
        panic!("expected task clean");
    };
    assert_eq!(ids, "-abc,-def");
}

#[test]
fn cron_list_project_flag_parses() {
    let Commands::Cron {
        command: super::cli::CronSub::List { project },
    } = parse_command(&["cron", "list", "--project", "origa"]).unwrap()
    else {
        panic!("expected cron list");
    };
    assert_eq!(project.as_deref(), Some("origa"));
}

#[test]
fn cron_list_defaults_to_uwuwu() {
    let Commands::Cron {
        command: super::cli::CronSub::List { project },
    } = parse_command(&["cron", "list"]).unwrap()
    else {
        panic!("expected cron list");
    };
    assert!(project.is_none(), "дефолт — uwuwu");
}

fn expect_undo(command: Commands) -> (Option<String>, bool) {
    let Commands::Task {
        command: TaskSub::Undo { journal, list },
        ..
    } = command
    else {
        panic!("expected task undo");
    };
    (journal, list)
}

#[test]
fn undo_list_flag_parses_documented_contract() {
    let command = parse_command(&["task", "--project", "uwuwu", "undo", "--list"])
        .expect("AGENTS.md: task undo [--list]");
    let (journal, list) = expect_undo(command);
    assert!(list, "--list = режим списка");
    assert!(journal.is_none());
}

#[test]
fn undo_list_flag_wins_over_positional() {
    let command = parse_command(&[
        "task",
        "--project",
        "uwuwu",
        "undo",
        "журнал.json",
        "--list",
    ])
    .expect("--list сильнее позиционного");
    let (journal, list) = expect_undo(command);
    assert!(list, "--list побеждает: задаём список, не восстановление");
    assert_eq!(
        journal.as_deref(),
        Some("журнал.json"),
        "позиционное сохранено (не теряется молча)"
    );
}

#[test]
fn undo_journal_alone_means_restore() {
    let command = parse_command(&["task", "--project", "uwuwu", "undo", "журнал.json"]).unwrap();
    let (journal, list) = expect_undo(command);
    assert!(!list, "без --list и с именем — восстановление");
    assert_eq!(journal.as_deref(), Some("журнал.json"));
}

fn expect_create(command: Commands) -> (Option<String>, bool) {
    let Commands::Task {
        command:
            TaskSub::Create {
                r#type: ty,
                dry_run,
                ..
            },
        ..
    } = command
    else {
        panic!("expected task create subcommand");
    };
    (ty, dry_run)
}

#[test]
fn create_dry_run_flag_parses_with_type() {
    let command = parse_command(&[
        "task",
        "--project",
        "uwuwu",
        "create",
        "--title",
        "T",
        "--type",
        "bug",
        "--dry-run",
    ])
    .expect("dry-run — флаг create, совместим с --type");
    let (ty, dry_run) = expect_create(command);
    assert!(dry_run, "--dry-run распознан");
    assert_eq!(
        ty.as_deref(),
        Some("bug"),
        "--type сохранён рядом с --dry-run"
    );
}

#[test]
fn create_without_dry_run_defaults_to_live() {
    let command = parse_command(&["task", "--project", "uwuwu", "create", "--title", "T"]).unwrap();
    let (_, dry_run) = expect_create(command);
    assert!(
        !dry_run,
        "без флага — живой create (скелет-осмотр только через --dry-run)"
    );
}

// ——— профилактика (тело задачи 20261104072048-4n1rwdt): body/edit/close —
// строгая грамматика «id + флаги, лишнее — громкая ошибка». У этих сабкоманд
// нет trailing_var_arg — clap отклоняет лишние позиционные; пинну, чтобы
// молчаливый swallow не появился здесь задним числом. ———

#[test]
fn edit_rejects_extra_positional() {
    let Err(err) = parse_command(&[
        "task",
        "--project",
        "uwuwu",
        "edit",
        "abc",
        "--status",
        "Review",
        "лишний",
    ]) else {
        panic!("лишний позиционный — ошибка грамматики");
    };
    assert_eq!(
        err.kind(),
        clap::error::ErrorKind::UnknownArgument,
        "громкая ошибка, не молчаливый swallow: {err}"
    );
}

#[test]
fn close_rejects_extra_positional() {
    let Err(err) = parse_command(&["task", "--project", "uwuwu", "close", "abc", "лишний"])
    else {
        panic!("лишний позиционный — ошибка грамматики");
    };
    assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument, "{err}");
}

#[test]
fn body_rejects_extra_positional() {
    let Err(err) = parse_command(&[
        "task",
        "--project",
        "uwuwu",
        "body",
        "abc",
        "--replace",
        "лишний",
    ]) else {
        panic!("лишний позиционный — ошибка грамматики");
    };
    assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument, "{err}");
}

// ——— search / semantic: грамматика always-on dual-режима (20261104072044) ———

#[test]
fn search_defaults_are_dual_mode_without_semantic_flag() {
    let Commands::Search {
        query,
        limit,
        sofa,
        threshold,
    } = parse_command(&["search", "тема"]).unwrap()
    else {
        panic!("expected search");
    };
    assert_eq!(query, "тема");
    assert_eq!(limit, 10, "limit — на метод");
    assert_eq!(sofa, 0, "SOFA opt-in, дефолт выкл");
    assert!((threshold - 0.35).abs() < 1e-9, "порог косинуса семантики");
}

#[test]
fn search_semantic_flag_is_rejected() {
    // Решение владельца: поиск универсальный, отдельного флага нет.
    assert!(parse_command(&["search", "тема", "--semantic"]).is_err());
}

#[test]
fn semantic_subcommands_parse() {
    let Commands::Semantic { command } = parse_command(&["semantic", "rebuild"]).unwrap() else {
        panic!("expected semantic");
    };
    assert!(matches!(command, SemanticSub::Rebuild { resume: false }));
    let Commands::Semantic { command } =
        parse_command(&["semantic", "rebuild", "--resume"]).unwrap()
    else {
        panic!("expected semantic");
    };
    assert!(matches!(command, SemanticSub::Rebuild { resume: true }));
    let Commands::Semantic { command } = parse_command(&["semantic", "status"]).unwrap() else {
        panic!("expected semantic");
    };
    assert!(matches!(command, SemanticSub::Status));
    let Commands::Semantic { command } = parse_command(&["semantic", "sync"]).unwrap() else {
        panic!("expected semantic");
    };
    assert!(matches!(command, SemanticSub::Sync));
}

#[test]
fn version_flag_prints_semver_revision_and_build_date() {
    use clap::error::ErrorKind;

    let error = match Cli::try_parse_from(["uwuwu-cli", "--version"]) {
        Ok(_) => panic!("--version must not parse into a command"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), ErrorKind::DisplayVersion);

    let rendered = error.render().to_string();
    let version_line = rendered
        .lines()
        .find(|line| line.contains(env!("CARGO_PKG_VERSION")))
        .expect("version line present")
        .to_string();
    let metadata = version_line
        .split('(')
        .nth(1)
        .expect("parenthesised build metadata")
        .trim_end_matches(')');

    let mut tokens = metadata.split_whitespace();
    let revision = tokens.next().expect("revision token");
    let build_date = tokens.next().expect("build date token");
    // Hex revision or `unknown` fallback from build.rs; `-dirty` suffix when
    // the tree was dirty. Tag-like names (e.g. `v0.5.0` on an exact tag) are
    // also valid describe output — hyphen-separated parts stay alphanumeric.
    let is_alphanumeric_revision = revision
        .split('-')
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric()));
    assert!(
        revision == "unknown" || is_alphanumeric_revision,
        "unexpected revision format: {revision}"
    );
    assert!(
        is_iso_date(build_date),
        "unexpected date format: {build_date}"
    );
}

fn is_iso_date(s: &str) -> bool {
    s.len() == 10
        && s.as_bytes()[4] == b'-'
        && s.as_bytes()[7] == b'-'
        && s.chars().filter(|c| *c != '-').all(|c| c.is_ascii_digit())
}

/// projects печатает имена ноутбуков воркспейса — help не должен обещать
/// перечень проектов и должен указывать штатный путь перечня (задача
/// 20261109132340-nje6zb4).
#[test]
fn projects_help_describes_notebooks_not_projects() {
    let cmd = <Cli as clap::CommandFactory>::command();
    let projects = cmd
        .find_subcommand("projects")
        .expect("projects subcommand exists");
    let about = projects
        .get_about()
        .expect("projects about задан")
        .to_string();
    let low = about.to_lowercase();
    assert!(
        !low.contains("list all projects"),
        "help не должен обещать перечень проектов: {about}"
    );
    assert!(
        low.contains("ноутбук"),
        "help должен описывать фактическую семантику (ноутбуки воркспейса): {about}"
    );
    assert!(
        low.contains("docs ls"),
        "help должен указывать штатный путь перечня проектов (docs ls \"Projects\"): {about}"
    );
}

/// docs ls help описывает ноутбучную ветку резолва — путь, на который
/// ведёт help projects и AGENTS.md; неполный help = интерфейсный контракт
/// не назван (задача 20261110080223-au2sr76, ADR-081).
#[test]
fn docs_ls_help_describes_notebook_fallback() {
    let cmd = <Cli as clap::CommandFactory>::command();
    let ls = cmd
        .find_subcommand("docs")
        .expect("docs subcommand exists")
        .find_subcommand("ls")
        .expect("docs ls subcommand exists");
    let about = ls
        .get_long_about()
        .or_else(|| ls.get_about())
        .expect("docs ls about задан")
        .to_string();
    let low = about.to_lowercase();
    assert!(
        low.contains("ноутбук"),
        "help docs ls должен описывать ноутбучную ветку резолва (ADR-081): {about}"
    );
    assert!(
        low.contains("не резолвится"),
        "help docs ls должен явно закрывать составной префикс «Projects/<проект>» — не обещать: {about}"
    );
}
