//! Disposable GitHub-риг для живых e2e-проб write-путей (ADR-110:
//! реальный REST, без HTTP-моков): репо создаётся и сносится тестом.

use base64::Engine as _;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const API: &str = "https://api.github.com";

pub async fn api(
    http: &reqwest::Client,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> (reqwest::StatusCode, Value) {
    // Транзиент GitHub (5xx/таймаут) рвал прогон не по причине теста —
    // ограниченный ретрай только на такие отказы, 4xx не повторяем.
    for attempt in 0..3 {
        let mut req = http
            .request(method.clone(), format!("{API}{path}"))
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "uwuwu-rig-e2e")
            .timeout(Duration::from_secs(30));
        if let Some(body) = &body {
            req = req.json(body);
        }
        let resp = match req.send().await {
            Ok(resp) => resp,
            Err(err) if attempt < 2 => {
                eprintln!("github transport {path}: {err} — повтор {}", attempt + 1);
                tokio::time::sleep(Duration::from_millis(700)).await;
                continue;
            }
            Err(err) => {
                eprintln!("github transport {path} после 3 попыток: {err}");
                return (reqwest::StatusCode::REQUEST_TIMEOUT, Value::Null);
            }
        };
        let status = resp.status();
        let value = resp.json().await.unwrap_or(Value::Null);
        if status.as_u16() >= 500 && attempt < 2 {
            eprintln!("github {path}: HTTP {status} — повтор {}", attempt + 1);
            tokio::time::sleep(Duration::from_millis(700)).await;
            continue;
        }
        return (status, value);
    }
    unreachable!("цикл с возвратом на последней попытке")
}

pub struct Rig {
    http: reqwest::Client,
    token: String,
    pub full: String,
    pub branch: String,
}

impl Rig {
    pub async fn create(http: reqwest::Client, token: &str) -> Self {
        let stamp = format!(
            "{}{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_millis(),
            std::process::id()
        );
        let name = format!("uwuwu-rig-e2e-{stamp}");
        let (status, body) = api(
            &http,
            token,
            reqwest::Method::POST,
            "/user/repos",
            Some(json!({"name": name, "private": true, "auto_init": true})),
        )
        .await;
        assert!(status.is_success(), "создание рига: HTTP {status} — {body}");
        assert_eq!(body["private"], json!(true), "риг обязан быть приватным");
        Self {
            full: body["full_name"].as_str().expect("full_name").to_string(),
            branch: body["default_branch"]
                .as_str()
                .unwrap_or("main")
                .to_string(),
            http,
            token: token.to_string(),
        }
    }

    /// Contents после коммита может отставать; дерево — авторитетный
    /// источник для проверки результата записи (#1653).
    pub async fn blob(&self, path: &str) -> Option<String> {
        let tree_path = format!("/repos/{}/git/trees/{}?recursive=1", self.full, self.branch);
        let (status, tree) = api(
            &self.http,
            &self.token,
            reqwest::Method::GET,
            &tree_path,
            None,
        )
        .await;
        assert!(status.is_success(), "git tree: HTTP {status} — {tree}");
        let sha = tree["tree"]
            .as_array()
            .and_then(|entries| {
                entries.iter().find(|e| {
                    e["path"].as_str() == Some(path) && e["type"].as_str() == Some("blob")
                })
            })
            .and_then(|e| e["sha"].as_str())?
            .to_string();
        let (status, blob) = api(
            &self.http,
            &self.token,
            reqwest::Method::GET,
            &format!("/repos/{}/git/blobs/{sha}", self.full),
            None,
        )
        .await;
        assert!(status.is_success(), "git blob: HTTP {status} — {blob}");
        let b64 = blob["content"]
            .as_str()
            .unwrap_or_default()
            .replace('\n', "");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("blob base64");
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub async fn wait_blob(&self, path: &str, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(40);
        while Instant::now() < deadline {
            if let Some(content) = self.blob(path).await {
                if content.contains(needle) {
                    return content;
                }
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        panic!("blob {path} не содержит {needle:?} за 40 с");
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let http = self.http.clone();
        let token = self.token.clone();
        let full = self.full.clone();
        let name = full.clone();
        let outcome = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("drop runtime");
            runtime.block_on(api(
                &http,
                &token,
                reqwest::Method::DELETE,
                &format!("/repos/{full}"),
                None,
            ))
        })
        .join();
        let (status, body) = outcome.expect("drop thread");
        if !status.is_success() {
            // Паника в Drop подменяет сообщение основного сценария:
            // сначала запасной путь через gh, потом громкая инструкция.
            let fallback = std::process::Command::new("gh")
                .args(["repo", "delete", &name, "--yes"])
                .stdout(std::process::Stdio::null())
                .status();
            if !fallback.is_ok_and(|state| state.success()) {
                eprintln!(
                    "снос рига {name} не удался: HTTP {status} — {body}; \
                     удали руками: gh repo delete {name} --yes"
                );
            }
        }
    }
}
