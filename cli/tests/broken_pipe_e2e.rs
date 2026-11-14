//! Пин «пайп stdout, закрытый потребителем (head/less), — тихий успех rc 0,
//! а не паника» (задача 1093): rust ставит SIGPIPE в SIG_IGN на старте
//! процесса, println! получает EPIPE и паникует (rc 101 + стектрейс в
//! stderr) — rc-гарды агентов трактуют это как провал команды.
//!
//! Уровень процесса, не юнит: EPIPE рождается ядром на write в пайп с
//! закрытым read-end — строковым стораджем stdout его не сымитировать.
//! Риг герметичен и без гонок: read-end закрывается ДО спавна, поэтому
//! первый же write ребёнка гарантированно уходит в EPIPE при любом
//! планировщике. Команда — `semantic status` с UWUWU_SEMANTIC_DB на
//! несуществующий путь: локальная ветка «индекс отсутствует» (3 строки
//! println!, ноль сети, ноль клона).

#![cfg(unix)]

use std::io::pipe;
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_uwuwu-cli")
}

/// Пин: закрытый потребителем stdout-пайп — тихий успех (rc 0, stderr пуст),
/// не паника 101. Потребитель получил всё, что успел прочитать, — это успех
/// команды с точки зрения rc-гардов.
#[test]
fn stdout_closed_pipe_is_quiet_success() {
    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let db = tmp.path().join("absent.db");

    let (reader, writer) = pipe().expect("pipe");
    drop(reader); // read-end закрыт до спавна: первый write ребёнка — гарантированный EPIPE

    let child = Command::new(bin())
        .args(["semantic", "status"])
        .env("UWUWU_SEMANTIC_DB", &db)
        .stdout(Stdio::from(writer))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn uwuwu-cli");

    let out = child.wait_with_output().expect("wait");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "закрытый пайп — тихий успех rc 0, а не провал команды; stderr: {stderr}"
    );
    assert!(
        stderr.is_empty(),
        "stderr должен быть пуст (без стектрейса паники); получено: {stderr}"
    );
}
