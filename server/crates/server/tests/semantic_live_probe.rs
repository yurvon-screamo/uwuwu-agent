//! Живой e2e полного индекса (#1687): реальная вика uwuwu-hub (1456 md)
//! и реальные дайджесты задач (до 400) против локальной Ollama на kisa
//! (`UWUWU_EMBED_URL`, в прогоне — прямой `http://127.0.0.1:11434`).
//! Том — `<env>/scratch/semantic-live-probe` (env UWUWU_PROBE_VOLUME), не temp.
//! Вывод `--nocapture` — носитель доказательств до мержа: события
//! `semantic.index_state`, контрольные запросы, обе секции поиска.
//!
//! Один ignored-тест в таргете: `--ignored` без фильтра не поднимает
//! соседние пробы (#1687, раунд 4 Low-2).

mod search;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use search::semantic::{OllamaEmbeddings, EMBED_MODEL};
use search::semantic_backend::LanceSemanticBackend;
use serde_json::Value;
use uwuwu_usecase::{
    IssueDigest, IssueIndexSource, SearchUseCase, SemanticBackend, UsecaseError, UsecaseResult,
    WikiRepo,
};

const HUB: &str = "yurvon-screamo/uwuwu-hub";

/// Контрольные запросы (#1363, перепинаны на живые слаги 26.11). На полном
/// корпусе «фабрика…» даёт набор релевантных доков, а не один README из
/// 4-докового сида — ожидание — набор релевантных слагов, не один фиксированный.
const CONTROL: &[(&str, &[&str])] = &[
    (
        "обзор моделей эмбеддингов",
        &["Experience/llm-local/Embedding Models Overview"],
    ),
    ("тренажёр кандзи для японского", &["Projects/origa/README"]),
    (
        "фабрика задач канбан конвейер",
        &[
            "Projects/uwuwu/README",
            "Projects/uwuwu/Фабрика: диаграмма конвейера",
            "Projects/uwuwu/ADR/ADR-032: Fast-track-вход задач — untracked In Progress спавнит implement-агента; рестарты на нейтральном continuePrompt",
        ],
    ),
];
const FRESH_SLUG: &str = "Experience/platform/Проба 1687";
const FRESH_PHRASE: &str = "уникальная фраза зонда 1687 квантовый кроссворд";

/// Read-only WikiRepo поверх GitHub REST: полное дерево docs/**.
struct LiveWiki {
    token: String,
    http: reqwest::Client,
    shas: Vec<(String, String)>,
}

impl LiveWiki {
    async fn open(token: String) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .user_agent("uwuwu-live-probe")
            .build()
            .map_err(|e| format!("reqwest: {e}"))?;
        let resp = http
            .get(format!(
                "https://api.github.com/repos/{HUB}/git/trees/main?recursive=1"
            ))
            .bearer_auth(&token)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| format!("tree: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("tree: HTTP {}", resp.status()));
        }
        let body: Value = resp.json().await.map_err(|e| format!("tree json: {e}"))?;
        let shas = body["tree"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| {
                let path = e["path"].as_str()?;
                let sha = e["sha"].as_str()?;
                let ok = e["type"].as_str()? == "blob"
                    && path.starts_with("docs/")
                    && path.ends_with(".md");
                ok.then(|| {
                    (
                        path.trim_start_matches("docs/")
                            .trim_end_matches(".md")
                            .to_string(),
                        sha.to_string(),
                    )
                })
            })
            .collect();
        Ok(Self { token, http, shas })
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        let resp = self
            .http
            .get(format!("https://api.github.com{path}"))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| format!("get {path}: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("get {path}: HTTP {}", resp.status()));
        }
        resp.json().await.map_err(|e| format!("json {path}: {e}"))
    }
}

fn encode_path(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[async_trait]
impl WikiRepo for LiveWiki {
    async fn read_doc(&self, slug: &str) -> UsecaseResult<Option<String>> {
        let body = self
            .get(&format!(
                "/repos/{HUB}/contents/docs/{}",
                encode_path(&format!("{slug}.md"))
            ))
            .await
            .map_err(UsecaseError::Board)?;
        let b64 = body["content"]
            .as_str()
            .unwrap_or_default()
            .replace('\n', "");
        let bytes = B64
            .decode(b64)
            .map_err(|e| UsecaseError::Board(format!("{slug}: base64 {e}")))?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    async fn list_docs(&self, prefix: &str) -> UsecaseResult<Vec<String>> {
        Ok(self
            .shas
            .iter()
            .map(|(slug, _)| slug.clone())
            .filter(|slug| slug.starts_with(prefix))
            .collect())
    }

    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
        Ok(self.shas.clone())
    }

    async fn write_doc(&self, _slug: &str, _content: &str, _message: &str) -> UsecaseResult<()> {
        Err(UsecaseError::Board("live probe wiki is read-only".into()))
    }

    async fn append_doc(&self, _slug: &str, _line: &str, _message: &str) -> UsecaseResult<usize> {
        Err(UsecaseError::Board("live probe wiki is read-only".into()))
    }
}

/// Оверлей «свежей правки»: контроль-док с уникальной фразой поверх живой вики.
struct ProbeWiki {
    live: LiveWiki,
}

#[async_trait]
impl WikiRepo for ProbeWiki {
    async fn read_doc(&self, slug: &str) -> UsecaseResult<Option<String>> {
        if slug == FRESH_SLUG {
            return Ok(Some(format!("# Проба 1687\n\n{FRESH_PHRASE}\n")));
        }
        self.live.read_doc(slug).await
    }
    async fn list_docs(&self, prefix: &str) -> UsecaseResult<Vec<String>> {
        self.live.list_docs(prefix).await
    }
    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
        let mut shas = self.live.doc_shas().await?;
        shas.push((FRESH_SLUG.to_string(), "sha-probe-1687".to_string()));
        Ok(shas)
    }
    async fn write_doc(&self, _slug: &str, _content: &str, _message: &str) -> UsecaseResult<()> {
        Err(UsecaseError::Board("read-only".into()))
    }
    async fn append_doc(&self, _slug: &str, _line: &str, _message: &str) -> UsecaseResult<usize> {
        Err(UsecaseError::Board("read-only".into()))
    }
}

/// Живые дайджесты задач хаба (тот же источник, что прод `recent_issues(400)`).
struct LiveIssues {
    token: String,
    http: reqwest::Client,
}

#[async_trait]
impl IssueIndexSource for LiveIssues {
    async fn recent_issues(&self, limit: usize) -> UsecaseResult<Vec<IssueDigest>> {
        let mut out = Vec::new();
        for page in 1..=4 {
            let resp = self
                .http
                .get(format!(
                    "https://api.github.com/repos/{HUB}/issues?state=all&per_page=100&page={page}"
                ))
                .bearer_auth(&self.token)
                .header("Accept", "application/vnd.github+json")
                .send()
                .await
                .map_err(|e| UsecaseError::Board(format!("issues: {e}")))?;
            if !resp.status().is_success() {
                return Err(UsecaseError::Board(format!(
                    "issues: HTTP {}",
                    resp.status()
                )));
            }
            let body: Value = resp
                .json()
                .await
                .map_err(|e| UsecaseError::Board(format!("issues json: {e}")))?;
            let Some(items) = body.as_array() else { break };
            if items.is_empty() {
                break;
            }
            for item in items {
                if item.get("pull_request").is_some() {
                    continue;
                }
                let Some(number) = item["number"].as_u64() else {
                    continue;
                };
                out.push(IssueDigest {
                    number,
                    title: item["title"].as_str().unwrap_or_default().to_string(),
                    body: item["body"]
                        .as_str()
                        .unwrap_or_default()
                        .chars()
                        .take(4000)
                        .collect(),
                });
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }
}

fn embedder_from_env() -> OllamaEmbeddings {
    let url = std::env::var("UWUWU_EMBED_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .expect("UWUWU_EMBED_URL (живая проба; на kisa — http://127.0.0.1:11434)");
    OllamaEmbeddings::new(url, EMBED_MODEL.to_string())
}

fn init_json_logs() {
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::new("info"))
        .with_target(false)
        .try_init();
}

#[tokio::test]
#[ignore] // GH_TOKEN + UWUWU_EMBED_URL + UWUWU_PROBE_VOLUME + сеть (read-only)
async fn live_full_index_and_control_queries() {
    init_json_logs();
    let token = std::env::var("GH_TOKEN").expect("GH_TOKEN");
    let dir = PathBuf::from(
        std::env::var("UWUWU_PROBE_VOLUME")
            .expect("UWUWU_PROBE_VOLUME=<env>/scratch/semantic-live-probe"),
    );
    let reuse = std::env::var("UWUWU_PROBE_REUSE").as_deref() == Ok("1");
    if !reuse {
        let _ = std::fs::remove_dir_all(&dir); // полный rebuild, не частичный индекс
    }
    let wiki = Arc::new(ProbeWiki {
        live: LiveWiki::open(token.clone()).await.expect("живая вика"),
    });
    let issues = Arc::new(LiveIssues {
        token,
        http: reqwest::Client::builder()
            .user_agent("uwuwu-live-probe")
            .build()
            .expect("client"),
    });
    let backend = LanceSemanticBackend::open(
        dir,
        wiki,
        Some(issues as Arc<dyn IssueIndexSource>),
        Arc::new(embedder_from_env()),
    )
    .await
    .expect("open lance backend");

    // Полный корпус строится итерациями, как прод (19 доков за тик), только
    // крупнее шаг: один вызов упирается в бюджет тика и не доходит до задач.
    let mut total = 0usize;
    for tick in 1..=8 {
        if reuse {
            break;
        }
        let changed = backend.sync_index(400).await.expect("живой полный sync");
        eprintln!("тик {tick}: проиндексировано доков {changed}");
        total += changed;
        if changed == 0 {
            break;
        }
    }
    eprintln!("полный sync проиндексировал доков: {total} (reuse={reuse})");
    if !reuse {
        assert!(total > 1000, "полный корпус должен дать >1000 доков");
    }

    // Контрольные запросы — тот же SearchUseCase, что рендерит MCP search.
    let uc = SearchUseCase {
        semantic: Some(Arc::new(backend) as Arc<dyn SemanticBackend>),
    };
    for (query, expected) in CONTROL {
        let hits = uc.search(query, 5).await.expect("control query");
        let semantic: Vec<&str> = hits
            .iter()
            .filter(|h| h.section == "semantic")
            .map(|h| h.slug.as_str())
            .collect();
        eprintln!("контроль «{query}»: {semantic:?}");
        assert!(
            semantic.iter().take(3).any(|s| expected.contains(s)),
            "релевантный слаг из {expected:?} — в top-3, факт: {semantic:?}"
        );
    }
    let fresh = uc.search(FRESH_PHRASE, 5).await.expect("fresh query");
    let fresh_top = fresh
        .iter()
        .find(|h| h.section == "semantic")
        .map(|h| h.slug.as_str());
    eprintln!("свежая правка: {fresh_top:?}");
    assert_eq!(fresh_top, Some(FRESH_SLUG), "свежая правка — top-1");
}
