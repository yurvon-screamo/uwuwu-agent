//! cli-гард входа в ревью: перевод задачи в «Ревью результата» допустим
//! только при живой мержабельности PR (mergeable + все проверки зелёные).
//! Граница — GitHub (gh читает только GitHub): не-GitHub PR и репо без CI
//! проходят с громким предупреждением, пайп не блокируется.
//! Канон — задача 20261109184134-d2x7lsn; injection point — TaskSub::Edit.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::time::Duration;

/// Канон-форма статуса, на которую вешается гард (см. TASK_STATUSES).
pub const STATUS_REVIEW_RESULT: &str = "Ревью результата";

/// Единственный хост, чью мержабельность умеет читать gh.
const GITHUB_HOST: &str = "github.com";

/// Хост PR-URL (после scheme, до path/port); без scheme — None.
pub fn url_host(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    authority.split(':').next()
}

/// (owner, repo) из github.com/OWNER/REPO/pull[s]/N — для api-пробы workflows.
/// Не-GitHub и неполные формы — None.
pub fn github_owner_repo(url: &str) -> Option<(String, String)> {
    if url_host(url)? != GITHUB_HOST {
        return None;
    }
    let path = url.split("://").nth(1)?;
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // [host, owner, repo, pull|pulls, N]
    if segments.len() < 5 || !matches!(segments[3], "pull" | "pulls") {
        return None;
    }
    Some((segments[1].to_string(), segments[2].to_string()))
}

/// Нормализованное состояние проверок из statusCheckRollup.
#[derive(Debug, PartialEq, Eq)]
pub enum RollupState {
    /// Проверки есть, все завершены и зелёные.
    Green,
    /// Завершены и не зелёные: «имя (итог)».
    Failed(Vec<String>),
    /// Незавершённые: имена.
    Pending(Vec<String>),
    /// Проверок нет — различается пробой workflows.
    Empty,
}

/// Один элемент rollup → (имя, завершён, зелёный). CheckRun узнаётся по
/// status/conclusion, StatusContext — по state. Неизвестная форма —
/// «завершён, не зелёный»: честный отказ лучше тихого прохода.
fn rollup_item(item: &Value) -> Option<(String, bool, bool)> {
    let obj = item.as_object()?;
    let name = obj
        .get("name")
        .or_else(|| obj.get("context"))
        .and_then(|n| n.as_str())
        .unwrap_or("?")
        .to_string();
    if let Some(status) = obj.get("status").and_then(|s| s.as_str()) {
        let done = status == "COMPLETED";
        let green = done && obj.get("conclusion").and_then(|c| c.as_str()) == Some("SUCCESS");
        Some((name, done, green))
    } else if let Some(state) = obj.get("state").and_then(|s| s.as_str()) {
        match state {
            "SUCCESS" => Some((name, true, true)),
            "PENDING" | "EXPECTED" => Some((name, false, false)),
            _ => Some((name, true, false)), // ERROR | FAILURE | прочее
        }
    } else {
        Some((name, true, false))
    }
}

/// Свернуть statusCheckRollup в состояние. Красный приоритетнее «идёт»:
/// починка и так потребует пуш, ожидание остальных не поможет.
pub fn rollup_state(rollup: &Value) -> RollupState {
    let items = rollup.as_array().map(Vec::as_slice).unwrap_or(&[]);
    if items.is_empty() {
        return RollupState::Empty;
    }
    let mut failed = Vec::new();
    let mut pending = Vec::new();
    for (name, done, green) in items.iter().filter_map(rollup_item) {
        if !done {
            pending.push(name);
        } else if !green {
            failed.push(name);
        }
    }
    if !failed.is_empty() {
        RollupState::Failed(failed)
    } else if !pending.is_empty() {
        RollupState::Pending(pending)
    } else {
        RollupState::Green
    }
}

/// Итог второй пробы (наличие CI-workflows в репо) — нужна только пустому
/// rollup: различает «CI не стартовал» и «репо без CI».
#[derive(Debug, PartialEq, Eq)]
pub enum WorkflowProbe {
    /// Workflows есть: пустой rollup = «CI не стартовал».
    Present,
    /// Workflows нет / Actions выключен (403,404): репо без CI.
    Absent,
    /// Проба не удалась (сеть/прочее): громкий отказ.
    Failed,
}

/// Чистый маппинг (mergeable, состояние проверок, проба workflows) → вердикт:
/// Ok(None) — чистый проход, Ok(Some(предупреждение)) — проход с оговоркой,
/// Err — отказ перевода с рецептом добивки.
pub fn evaluate(
    mergeable: &str,
    rollup: &RollupState,
    workflows: &WorkflowProbe,
) -> Result<Option<&'static str>> {
    match mergeable {
        "MERGEABLE" => {}
        "CONFLICTING" => bail!(
            "PR не мержабелен: конфликты с base-веткой — разреши их (git fetch origin \
             <base>; merge/rebase), запушь и повтори"
        ),
        "UNKNOWN" => bail!("GitHub ещё считает мержабельность PR — повтори через минуту"),
        other => bail!("неожиданный mergeable «{other}» — проверь PR руками (gh pr view)"),
    }
    match rollup {
        RollupState::Green => Ok(None),
        RollupState::Failed(items) => bail!(
            "CI красный: {} — почини и запушь; повтори после зелёного рана",
            items.join("; ")
        ),
        RollupState::Pending(names) => bail!(
            "CI ещё идёт: {} — дождись завершения (gh pr checks <URL> --watch) и повтори",
            names.join(", ")
        ),
        RollupState::Empty => match workflows {
            WorkflowProbe::Present => bail!(
                "CI не стартовал (workflows в репо есть, проверок на PR нет) — проверь \
                 Actions; повтори после зелёного рана"
            ),
            WorkflowProbe::Absent => Ok(Some(
                "предупреждение: в репо нет CI-workflows — мержабельность чеками не \
                 верифицирована; проверь готовность сам",
            )),
            WorkflowProbe::Failed => bail!(
                "не удалось выяснить наличие CI-workflows — повтори (gh api \
                 repos/…/actions/workflows)"
            ),
        },
    }
}

/// Потолок gh-вызова: полуоткрытое соединение не должно молча подвешивать
/// перевод — гард стоит на горячем пути каждой финализации.
const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// gh-вызов с потолком времени; сбой — Err (громкий отказ, не тихий вис).
async fn run_gh(args: &[&str]) -> Result<std::process::Output> {
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(args);
    let description = format!("gh {}", args.first().unwrap_or(&""));
    tokio::time::timeout(GH_TIMEOUT, cmd.output())
        .await
        .map_err(|_| {
            anyhow!(
                "{description} не ответил за {}с — повтори; при повторе проверь \
                 мержабельность руками (gh pr view)",
                GH_TIMEOUT.as_secs()
            )
        })?
        .context("запуск gh не удался (есть ли gh в PATH?)")
}

/// gh-вызов с JSON-ответом; сбой — Err с stderr (one-shot, без ретраев).
async fn gh_json(args: &[&str]) -> Result<Value> {
    let out = run_gh(args).await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "gh {} не удался: {}",
            args.first().unwrap_or(&""),
            stderr.trim()
        );
    }
    serde_json::from_slice(&out.stdout)
        .with_context(|| format!("gh {} вернул не-JSON", args.join(" ")))
}

/// Есть ли в репо CI-workflows. 403/404 трактуется как «Actions нет» —
/// Ok(false); прочие сбои — Err (громкий отказ, не тихий проход).
async fn gh_workflows_present(owner_repo: &str) -> Result<bool> {
    let out = run_gh(&["api", &format!("repos/{owner_repo}/actions/workflows")]).await?;
    if out.status.success() {
        let v: Value =
            serde_json::from_slice(&out.stdout).context("gh api workflows вернул не-JSON")?;
        return Ok(v["workflows"].as_array().is_some_and(|a| !a.is_empty()));
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("HTTP 403") || stderr.contains("HTTP 404") {
        return Ok(false);
    }
    bail!("gh api workflows не удался: {}", stderr.trim());
}

/// Гард перевода в «Ревью результата»: проверить PR, отказать или пропустить.
/// Предупреждения — в stdout (команда успешна), отказы — Err.
pub async fn gate(pr_url: &str) -> Result<()> {
    let pr_url = pr_url.trim();
    if url_host(pr_url) != Some(GITHUB_HOST) {
        println!(
            "предупреждение: PR не на GitHub ({pr_url}) — мержабельность не проверялась \
             (гард читает только GitHub); проверь готовность сам"
        );
        return Ok(());
    }
    let pr = gh_json(&[
        "pr",
        "view",
        pr_url,
        "--json",
        "mergeable,statusCheckRollup",
    ])
    .await?;
    let mergeable = pr["mergeable"].as_str().unwrap_or_default().to_string();
    let rollup = rollup_state(pr.get("statusCheckRollup").unwrap_or(&Value::Null));
    let probe = if rollup == RollupState::Empty {
        let (owner, repo) = github_owner_repo(pr_url)
            .ok_or_else(|| anyhow!("PR-URL не разобран как github.com/OWNER/REPO/…: {pr_url}"))?;
        match gh_workflows_present(&format!("repos/{owner}/{repo}/actions/workflows")).await {
            Ok(true) => WorkflowProbe::Present,
            Ok(false) => WorkflowProbe::Absent,
            Err(_) => WorkflowProbe::Failed,
        }
    } else {
        // проба нужна только пустому rollup; не-Empty ветка её не читает
        WorkflowProbe::Present
    };
    match evaluate(&mergeable, &rollup, &probe) {
        Ok(None) => Ok(()),
        Ok(Some(warning)) => {
            println!("{warning}");
            Ok(())
        }
        Err(err) => Err(err.context("гард ревью: перевод в «Ревью результата» отклонён")),
    }
}
