//! uwuwu-cli: доступ агентам к платформе uwuwu. Бекенд — uwuwu-hub
//! (приватный GitHub-репо: issues = задачи, docs/ = вики), срез замены
//! бекенда 20261112155302-ttyp35g (история носителей: AFFiNE → hub).

pub mod cmd;
pub(crate) mod out;
pub mod path;
pub mod semantic;
pub mod store;

/// Публичный URL платформы (ссылки в выдаче).
pub fn ui_url() -> String {
    std::env::var("UWUWU_HUB_PUBLIC_URL")
        .unwrap_or_else(|_| "https://github.com/yurvon-screamo/uwuwu-hub".into())
}

pub(crate) fn init_tracing() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "uwuwu_cli=info,warn".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Entry point for the binary crate: parse args and dispatch (see `cmd`).
pub async fn run() -> anyhow::Result<()> {
    cmd::run().await
}
