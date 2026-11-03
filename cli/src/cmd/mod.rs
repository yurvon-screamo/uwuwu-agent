//! uwuwu-cli на SiYuan: единый диспетчер команд (единственный бекенд).

mod cli;
mod sy_dispatch;

#[cfg(test)]
mod cli_tests;

use anyhow::Result;
use clap::Parser as _;
use cli::Cli;

pub(crate) async fn run() -> Result<()> {
    crate::init_tracing();
    let cli = Cli::parse();
    sy_dispatch::dispatch(cli.command).await
}
