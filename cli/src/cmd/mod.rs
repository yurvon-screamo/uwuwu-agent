//! uwuwu-cli на SiYuan: единый диспетчер команд (единственный бекенд).

mod cli;
mod read_resolve;
mod sy_dispatch;

#[cfg(test)]
mod cli_tests;
#[cfg(test)]
mod guard_tests;
#[cfg(test)]
mod fallback_tests;
#[cfg(test)]
mod kernel_tests;

pub use sy_dispatch::read_markdown;

use anyhow::Result;
use clap::Parser as _;
use cli::Cli;

pub(crate) async fn run() -> Result<()> {
    crate::init_tracing();
    let cli = Cli::parse();
    sy_dispatch::dispatch(cli.command).await
}
