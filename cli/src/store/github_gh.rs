//! GitHub-транспорт GitHubStore: вызовы `gh api`/`gh graphql` как subprocess.
//!
//! gh уже авторизован на всех машинах контура (OAuth scopes: repo, project,
//! user), несёт `--attach` для медиа — REST/GraphQL покрывают всё остальное.
//! Безопасность: аргументы передаются argv-списком (не shell-строкой),
//! тело мутаций — stdin.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Запуск gh с таймаутом: argv-списком, stdin опционален, stdout — JSON.
/// Не-200 и GraphQL errors — громкий отказ с хвостом ответа.
pub async fn gh_json(args: &[&str], stdin_body: Option<&str>) -> Result<Value> {
    let raw = gh_raw(args, stdin_body).await?;
    if raw.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&raw)
        .with_context(|| format!("gh {:?}: не-JSON ответ: {}", &args[..3], clip(&raw)))
}

/// Размер страницы gh-листингов (per_page GitHub, максимум 100).
pub(crate) const GH_PAGE_SIZE: usize = 100;

/// Полная пагинация gh-листингов — канон ADR-020 (класс «молчаливый кап
/// листингов»: первая сотня тихо скрывала хвост инвентаря, баг 1086).
/// Цикл до короткой страницы; дрейф-дубли между страницами (конкурентные
/// мутации во время обхода) дедупятся по `id`, элементы без `id` проходят
/// как есть; кап 100 страниц → громкий `bail!` — тихая усечка запрещена.
/// `fetch(page)` отдаёт одну страницу элементов (для листингов без пагинации
/// сервер отдаёт всё разом — короткая страница терминация сработает сразу).
pub(crate) async fn gh_pages(
    mut fetch: impl AsyncFnMut(usize) -> Result<Vec<Value>>,
) -> Result<Vec<Value>> {
    const PAGES_CAP: usize = 100;
    let mut all = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for page in 1..=PAGES_CAP {
        let items = fetch(page).await?;
        let fetched = items.len();
        for item in items {
            match item.get("id").and_then(Value::as_u64) {
                Some(id) if seen.insert(id) => all.push(item),
                Some(_) => {}
                None => all.push(item),
            }
        }
        if fetched < GH_PAGE_SIZE {
            return Ok(all);
        }
    }
    bail!(
        "листинг: кап {PAGES_CAP} страниц ({} элементов, {GH_PAGE_SIZE}/стр) — \
         выборка неполна, тихая усечка запрещена (ADR-020); сузь фильтр",
        all.len()
    )
}

/// GraphQL-мутация/запрос одной строкой + переменные (serde map).
/// gh graphql --input - читает {query, variables} JSON со stdin.
pub async fn gh_graphql(query: &str, variables: &Value) -> Result<Value> {
    let payload = serde_json::json!({ "query": query, "variables": variables });
    let raw = gh_raw(
        &["api", "graphql", "--input", "-"],
        Some(&payload.to_string()),
    )
    .await?;
    let parsed: Value = serde_json::from_str(&raw)
        .with_context(|| format!("gh graphql: не-JSON ответ: {}", clip(&raw)))?;
    if let Some(errors) = parsed.get("errors") {
        if !errors.is_null() {
            bail!("graphql errors: {errors}");
        }
    }
    parsed
        .get("data")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("graphql: нет data в ответе"))
}

/// Сырой stdout gh (для non-JSON путей: --jq уходит, парсим на месте).
/// stdin_body → args дополняются `--input -`: gh api БЕЗ --input игнорирует
/// stdin, тело мутации молча терялось (422 links/0/schema — краш-баг среза C,
/// пойман на живом импорте мигратора до прод-переключения).
async fn gh_raw(args: &[&str], stdin_body: Option<&str>) -> Result<String> {
    let mut full: Vec<&str> = args.to_vec();
    if stdin_body.is_some() {
        full.extend(["--input", "-"]);
    }
    let mut cmd = Command::new("gh");
    cmd.args(&full)
        .stdin(if stdin_body.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .context("gh не запустился — есть ли gh в PATH?")?;
    if let Some(body) = stdin_body {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(body.as_bytes()).await?;
            stdin.shutdown().await?;
            drop(stdin);
        }
    }
    // Таймаут: зависший gh не вешает демона (Common ревью кода)
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        child.wait_with_output(),
    )
    .await
    .context("gh превысил таймаут 120s")?
    .context("gh прервался без ответа")?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "gh {:?} упал ({}): {}",
            &args[..args.len().min(3)],
            out.status,
            clip(&stderr)
        );
    }
    Ok(stdout)
}

fn clip(s: &str) -> String {
    let head: String = s.chars().take(300).collect();
    head
}
