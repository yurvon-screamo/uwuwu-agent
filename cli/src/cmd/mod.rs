//! Диспетчер CLI. Бекенд — uwuwu-hub (GitHub issues + Projects v2 +
//! docs md), срез F задачи 20261112155302-ttyp35g. легаси-слой вырезан
//! целиком.

pub(crate) mod cli;
pub(crate) mod hub_docs;
pub(crate) mod hub_task;
pub(crate) mod run;

#[cfg(test)]
mod parser_tests;

use anyhow::Result;
use clap::Parser as _;
use cli::Cli;

pub(crate) async fn run() -> Result<()> {
    crate::init_tracing();
    let cli = Cli::parse();
    run::dispatch(cli.command).await
}
