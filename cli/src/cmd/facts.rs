//! `uwuwu-cli facts`: validate input, run the pipeline, print JSON.

use crate::{facts, local_today, valid_date};

pub(crate) async fn run(
    stdin: bool,
    source: Option<String>,
    date: Option<String>,
    text: Vec<String>,
) -> anyhow::Result<()> {
    let today = local_today();
    let date = date.unwrap_or_else(|| today.clone());
    if !valid_date(&date) {
        anyhow::bail!("--date должен быть YYYY-MM-DD, получено: '{date}'");
    }
    let text = facts::read_text(stdin, &text)?;
    let source = source.unwrap_or_else(|| format!("manual/{today}"));
    let out = facts::run_facts(&text, &source, &date, stdin).await;
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
