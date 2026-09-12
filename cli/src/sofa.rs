//! Stack Overflow for Agents (SOFA) — публичный контур знаний.
//!
//! Поиск встроен в `wiki search` (блок Public) и `wiki sofa <post_id>` для
//! чтения полного поста. Ключ — первый агент из ~/.sofa/credentials.json
//! (агент uwuwu-opencode, contributor/publish_directly).
//!
//! Сессия создаётся на вызов и закрывается после: CLI короткоживущий, а
//! контракт скилла запрещает персистить session_id как креду.
//! Любая сетевая/ключевая проблема деградирует мягко: локальный поиск
//! не страдает, в stderr — предупреждение.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

const BASE: &str = "https://agents.stackoverflow.com";
const CLIENT_NAME: &str = "uwuwu-cli";
const MODEL_NAME: &str = "unknown";

#[derive(Debug, Clone)]
pub struct SofaHit {
    pub id: String,
    pub content_type: String,
    pub title: String,
    pub trust: Option<i64>,
    pub url: String,
    pub excerpt: Option<String>,
}

pub struct SofaPost {
    pub title: String,
    pub content_type: String,
    pub trust: Option<i64>,
    pub body: String,
    pub replies: Vec<(String, String)>,
}

fn credentials_path() -> Result<PathBuf> {
    Ok(dirs_home()?.join(".sofa").join("credentials.json"))
}

fn dirs_home() -> Result<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .map_err(|_| anyhow!("HOME or USERPROFILE is not set"))
}

#[derive(Deserialize)]
struct Credentials {
    agents: std::collections::BTreeMap<String, AgentCred>,
}

#[derive(Deserialize)]
struct AgentCred {
    api_key: String,
}

fn load_api_key() -> Result<String> {
    let path = credentials_path()?;
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("SOFA: нет {}", path.display()))?;
    let cred: Credentials =
        serde_json::from_str(&raw).context("SOFA: битый credentials.json")?;
    cred.agents
        .values()
        .next()
        .map(|a| a.api_key.clone())
        .ok_or_else(|| anyhow!("SOFA: в credentials.json нет агентов"))
}

/// HTTP-клиент с таймаутом: SOFA не должен задерживать локальный поиск.
fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .user_agent(concat!("uwuwu-cli/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

async fn skill_digest(http: &reqwest::Client) -> Result<String> {
    let res = http.get(format!("{BASE}/skill.md")).send().await?;
    let digest = res
        .headers()
        .get("x-sofa-skill-digest")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    digest.ok_or_else(|| anyhow!("SOFA: /skill.md без X-Sofa-Skill-Digest"))
}

async fn create_session(http: &reqwest::Client, key: &str) -> Result<String> {
    let res = http
        .post(format!("{BASE}/api/sessions"))
        .header("Authorization", format!("Bearer {key}"))
        .header("X-Sofa-Client-Name", CLIENT_NAME)
        .header("X-Sofa-Model-Name", MODEL_NAME)
        .header("X-Sofa-Skill-Digest", skill_digest(http).await?)
        .send()
        .await?;
    if !res.status().is_success() {
        anyhow::bail!("SOFA: POST /api/sessions HTTP {}", res.status());
    }
    #[derive(Deserialize)]
    struct Sess {
        session_id: String,
    }
    Ok(res.json::<Sess>().await?.session_id)
}

async fn close_session(http: &reqwest::Client, key: &str, sid: &str) {
    let _ = http
        .delete(format!("{BASE}/api/sessions/{sid}"))
        .header("Authorization", format!("Bearer {key}"))
        .header("X-Sofa-Session", sid)
        .send()
        .await;
}

#[derive(Deserialize)]
struct PostsList {
    items: Vec<PostsItem>,
}

#[derive(Deserialize)]
struct PostsItem {
    id: String,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body_excerpt: Option<String>,
    #[serde(default)]
    trust_summary: Option<TrustSummary>,
}

#[derive(Deserialize)]
struct TrustSummary {
    #[serde(default)]
    score: Option<i64>,
}

fn web_url(content_type: &str, id: &str) -> String {
    let kind = match content_type {
        "playbook" => "playbooks",
        "question" => "questions",
        "blueprint" => "blueprints",
        _ => "tils",
    };
    format!("{BASE}/{kind}/{id}")
}

/// Поиск постов; ошибки наружу не роняют локальный поиск (вызывающий ловит).
pub async fn search(query: &str, k: usize) -> Result<Vec<SofaHit>> {
    let key = load_api_key()?;
    let http = client()?;
    let sid = create_session(&http, &key).await?;
    let result = async {
        let per_page = k.to_string();
        let url = format!(
            "{BASE}/api/posts?search={}&per_page={}",
            urlencode(query),
            urlencode(&per_page)
        );
        let res = http
            .get(url)
            .header("Authorization", format!("Bearer {key}"))
            .header("X-Sofa-Session", &sid)
            .send()
            .await?;
        if !res.status().is_success() {
            anyhow::bail!("SOFA: GET /api/posts HTTP {}", res.status());
        }
        let list = res.json::<PostsList>().await?;
        Ok(list
            .items
            .into_iter()
            .take(k)
            .map(|p| {
                let ctype = p.content_type.unwrap_or_else(|| "til".into());
                SofaHit {
                    url: web_url(&ctype, &p.id),
                    id: p.id,
                    content_type: ctype,
                    title: p.title.unwrap_or_else(|| "(без заголовка)".into()),
                    trust: p.trust_summary.and_then(|t| t.score),
                    excerpt: p.body_excerpt,
                }
            })
            .collect::<Vec<_>>())
    }
    .await;
    close_session(&http, &key, &sid).await;
    result
}

/// Полный пост по id (тело + реплаи).
pub async fn get(post_id: &str) -> Result<SofaPost> {
    let key = load_api_key()?;
    let http = client()?;
    let sid = create_session(&http, &key).await?;
    let result = async {
        let res = http
            .get(format!("{BASE}/api/posts/{}", urlencode(post_id)))
            .header("Authorization", format!("Bearer {key}"))
            .header("X-Sofa-Session", &sid)
            .send()
            .await?;
        if !res.status().is_success() {
            anyhow::bail!("SOFA: GET /api/posts/{post_id} HTTP {}", res.status());
        }
        let post = res.json::<PostsItemFull>().await?;
        Ok(SofaPost {
            title: post.title.unwrap_or_else(|| post_id.into()),
            content_type: post.content_type.unwrap_or_else(|| "til".into()),
            trust: post.trust_summary.and_then(|t| t.score),
            body: post.body.unwrap_or_default(),
            replies: post
                .replies
                .unwrap_or_default()
                .into_iter()
                .map(|r| (r.id, r.body.unwrap_or_default()))
                .collect(),
        })
    }
    .await;
    close_session(&http, &key, &sid).await;
    result
}

fn urlencode(s: &str) -> String {
    // post_id — UUID; на всякий случай экранируем зарезервированные символы
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[derive(Deserialize)]
struct PostsItemFull {
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    trust_summary: Option<TrustSummary>,
    #[serde(default)]
    replies: Option<Vec<ReplyFull>>,
}

#[derive(Deserialize)]
struct ReplyFull {
    id: String,
    #[serde(default)]
    body: Option<String>,
}
