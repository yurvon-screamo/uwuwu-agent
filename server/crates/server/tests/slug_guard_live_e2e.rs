//! Живая e2e-проба слаг-гарда write-путей (#1613): изолированное приватное
//! rig-репо + живой uwuwu-server (прод-писатель GitWikiWriter на GitHub
//! Git Data API, ADR-110: HTTP-моков нет). Проба ветки AppendMissing
//! писателя — требование ревью #1613: юнитами она не покрыта.
//!
//! Сценарий (то, что тестировщик гонял руками на риге):
//!
//! refusal-ветки, каждая с контролем нуля записей (raw contents GET → 404):
//!   1. write по хвосту живой статьи → root-prefix + суффикс-кандидат (#1219);
//!   2. write в неизвестный корень → root-prefix с каноническим набором (#1575);
//!   3. одно-сегментный таргет → root-prefix «файл в корне вики»;
//!   4. write под чистый файл-родитель → parent-collision (глубокий путь тоже);
//!   5. write по вложенному корнеимени → suffix-collision;
//!   6. append по хвосту живой статьи → append-nonexistent + кандидат;
//!   7. append по несуществующему уникальному слагу → append-nonexistent
//!      со строкой-лекарством «create it via docs_write» (живая ветка
//!      GitWikiWriter::append_line → AppendMissing);
//!
//! успехи (регресс прежнего контракта):
//!   8. легитимный новый док → каноническая fm-шапка;
//!   9. новый ADR под легаси-стем «файл + папка» → запись разрешена;
//!   10. append к живому → тело сохранено, строка в конце;
//!   11–12. нормализация слага («docs/…», «….md») — запись по
//!      нормализованному пути, двойник «….md.md» не создан.
//!
//! Гейты: UWUWU_E2E=1 и UWUWU_E2E_HUB_REPO=<изолированное rig-репо>
//! (защита: прод-хаб отказом), GH_TOKEN с доступом на запись в rig-репо.
//! Rig-репо disposable: сидируется уникальными по рану слагами, снос — руками.
//!
//! Запуск:
//!   UWUWU_E2E=1 UWUWU_E2E_HUB_REPO=<owner/rig-repo> \
//!     cargo test -p uwuwu-server --test slug_guard_live_e2e -- --nocapture

use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;

fn e2e_ready() -> Option<(String, String)> {
    if std::env::var("UWUWU_E2E").ok().as_deref() != Some("1") {
        return None;
    }
    let repo = std::env::var("UWUWU_E2E_HUB_REPO")
        .ok()
        .filter(|s| !s.trim().is_empty())?;
    if repo == "yurvon-screamo/uwuwu-hub" {
        panic!("UWUWU_E2E_HUB_REPO указывает на прод-хаб — риг обязан быть изолированным disposable-репо");
    }
    let token = std::env::var("GH_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .expect("GH_TOKEN обязателен (запись в rig-репо)");
    Some((repo, token))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("bind :0")
        .local_addr()
        .expect("local_addr")
        .port()
}

fn bin_exe(name: &str) -> PathBuf {
    std::env::var(format!("CARGO_BIN_EXE_{name}"))
        .unwrap_or_else(|_| {
            panic!("CARGO_BIN_EXE_{name} не задан — собери бинари (cargo build --bins)")
        })
        .into()
}

struct Stand {
    server: Child,
    port: u16,
    bearer: String,
}

impl Drop for Stand {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
    }
}

/// Спавн живого сервера против rig-репо: прод-драйвер github (писатель
/// GitWikiWriter), observe-режим (тики без мутаций, MCP-тулы живы).
async fn spawn_server(dir: &std::path::Path, repo: &str) -> Stand {
    let port = free_port();
    let bearer = "rig-e2e-bearer";
    let config = format!(
        "mode = \"observe\"\npoll_sec = 3600\nport = {port}\n\
         sweep_days = 0\nenv_gc_days = 0\n\n\
         [hub]\nrepo = \"{repo}\"\n\n\
         [executor]\nname = \"mock\"\nurl = \"http://127.0.0.1:1\"\n\n\
         [policy]\ncup = 1\n\n[policy.peak]\nenabled = false\nstart_min = 360\nend_min = 600\n\n\
         [[project]]\nname = \"rig\"\nboard = \"rig\"\nrepo = \"{repo}\"\n\
         checkout = \"{dir}/checkout\"\nenv_root = \"{dir}\"\n",
        port = port,
        repo = repo,
        dir = dir.display()
    );
    std::fs::write(dir.join("config.toml"), config).expect("config");
    let log = std::fs::File::create(dir.join("server.log")).expect("server log");
    let server = Command::new(bin_exe("uwuwu-server"))
        .env("UWUWU_SERVER_CONFIG", dir.join("config.toml"))
        .env("UWUWU_SERVER_STATE", dir.join("state.json"))
        .env("UWUWU_SERVER_BEARER", bearer)
        .env("UWUWU_HUB_REPO", repo)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::from(log.try_clone().expect("log clone")))
        .stderr(Stdio::from(log))
        .spawn()
        .expect("spawn uwuwu-server");
    let stand = Stand {
        server,
        port,
        bearer: bearer.into(),
    };
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Ok(resp) = http
            .get(format!("http://127.0.0.1:{port}/health"))
            .header("Authorization", format!("Bearer {bearer}"))
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            if resp.status().is_success() {
                return stand;
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    panic!(
        "сервер рига не поднялся\n{}",
        log_tail(&dir.join("server.log"))
    );
}

fn log_tail(path: &std::path::Path) -> String {
    let binding = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = binding.lines().collect();
    let start = lines.len().saturating_sub(30);
    lines[start..].join("\n")
}

/// MCP tools/call против живого сервера → (isError, текст).
async fn mcp_call(
    http: &reqwest::Client,
    stand: &Stand,
    tool: &str,
    args: Value,
) -> (bool, String) {
    let resp = http
        .post(format!("http://127.0.0.1:{}/mcp", stand.port))
        .header("Authorization", format!("Bearer {}", stand.bearer))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": tool, "arguments": args}
        }))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .expect("mcp transport");
    let body: Value = resp.json().await.expect("mcp json");
    let result = body
        .get("result")
        .unwrap_or_else(|| panic!("нет result в ответе MCP: {body}"));
    let text = result["content"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    (result["isError"].as_bool().unwrap_or(false), text)
}

/// Raw GitHub REST против rig-репо: PUT (сид) / GET (None — 404).
struct RigGithub<'a> {
    http: &'a reqwest::Client,
    token: &'a str,
    repo: &'a str,
}

impl RigGithub<'_> {
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (reqwest::StatusCode, Value) {
        let mut req = self
            .http
            .request(
                method,
                format!("https://api.github.com/repos/{}/contents/{path}", self.repo),
            )
            .bearer_auth(self.token)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "uwuwu-rig-e2e")
            .timeout(Duration::from_secs(30));
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.expect("github transport");
        let status = resp.status();
        let value: Value = resp.json().await.unwrap_or(Value::Null);
        (status, value)
    }

    async fn put_file(&self, path: &str, content: &str) {
        let (status, body) = self
            .call(
                reqwest::Method::PUT,
                path,
                Some(json!({
                    "message": format!("rig e2e seed: {path}"),
                    "content": base64::engine::general_purpose::STANDARD.encode(content),
                    "branch": "main"
                })),
            )
            .await;
        assert!(
            status.is_success(),
            "сид {path} не записан: HTTP {status} — {body}"
        );
    }

    async fn read_file(&self, path: &str) -> Option<String> {
        let (status, body) = self.call(reqwest::Method::GET, path, None).await;
        if status.as_u16() == 404 {
            return None;
        }
        assert!(status.is_success(), "GET {path}: HTTP {status} — {body}");
        let b64 = body["content"]
            .as_str()
            .unwrap_or_default()
            .replace('\n', "");
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .expect("base64 сид-дока");
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

#[tokio::test]
async fn slug_guard_live_e2e_main_scenario() {
    let Some((repo, token)) = e2e_ready() else {
        println!("e2e выключен: UWUWU_E2E=1 и UWUWU_E2E_HUB_REPO=<rig-репо> для живой пробы");
        return;
    };
    let http = reqwest::Client::new();
    let stamp = format!(
        "{}{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
        std::process::id()
    );
    let dir = std::env::temp_dir().join(format!("uwuwu-rig-e2e-{stamp}"));
    std::fs::create_dir_all(&dir).expect("rig dir");

    // Сид ДО старта сервера: дерево попадает в TTL-кеш свежим.
    let seed_doc = "---\ntitle: rig-article\ndate: 2026-01-01T00:00:00+09:00\nlastmod: 2026-01-01T00:00:00+09:00\n---\n\nseed body line\n";
    let rig = RigGithub {
        http: &http,
        token: &token,
        repo: &repo,
    };
    rig.put_file(&format!("docs/Experience/cli/rig-{stamp}.md"), seed_doc)
        .await;
    rig.put_file(&format!("docs/Projects/rig{stamp}/plain.md"), seed_doc)
        .await;
    rig.put_file(&format!("docs/Projects/rig{stamp}/ADR.md"), seed_doc)
        .await;
    rig.put_file(&format!("docs/Projects/rig{stamp}/ADR/seed.md"), seed_doc)
        .await;
    rig.put_file(
        &format!("docs/Experience/rig{stamp}/deep/Experience/rig{stamp}/sfx.md"),
        seed_doc,
    )
    .await;

    let stand = spawn_server(&dir, &repo).await;
    let article = format!("cli/rig-{stamp}"); // хвост живой статьи (класс #1219)
    let write = |slug: String, content: String| {
        let http = &http;
        let stand = &stand;
        async move {
            mcp_call(
                http,
                stand,
                "docs_write",
                json!({"slug": slug, "content": content}),
            )
            .await
        }
    };
    let append = |slug: String| {
        let http = &http;
        let stand = &stand;
        async move {
            mcp_call(
                http,
                stand,
                "docs_append",
                json!({"slug": slug, "line": "e2e probe line"}),
            )
            .await
        }
    };
    let absent = |path: String| {
        let rig = &rig;
        async move {
            assert!(
                rig.read_file(&path).await.is_none(),
                "нуль-запись нарушена: {path} существует после refusal-пробы"
            );
        }
    };

    // --- refusal-ветки (фича), каждая с контролем нуля записей ---
    let (err, text) = write(article.clone(), "probe".into()).await;
    assert!(err, "хвост живой статьи должен быть отказан: {text}");
    assert!(text.contains("root-prefix"), "имя ветки: {text}");
    assert!(
        text.contains(&format!("Experience/cli/rig-{stamp}")),
        "суффикс-кандидат: {text}"
    );
    absent(format!("docs/{article}.md")).await;

    let (err, text) = write(format!("platform/unique-{stamp}"), "probe".into()).await;
    assert!(err, "чужой корень должен быть отказан: {text}");
    assert!(text.contains("root-prefix"), "имя ветки: {text}");
    assert!(
        text.contains("Experience, Projects, bot, cron, person"),
        "канонический набор в отказе: {text}"
    );
    absent(format!("docs/platform/unique-{stamp}.md")).await;

    let (err, text) = write(format!("rootfile{stamp}"), "probe".into()).await;
    assert!(err, "одно-сегментный таргет должен быть отказан: {text}");
    assert!(
        text.contains("would be a file in the wiki root"),
        "факт ветки (файл в корне): {text}"
    );
    absent(format!("docs/rootfile{stamp}.md")).await;

    let (err, text) = write(format!("Projects/rig{stamp}/plain/child"), "probe".into()).await;
    assert!(
        err,
        "запись под чистый файл-родитель должна быть отказана: {text}"
    );
    assert!(text.contains("parent-collision"), "имя ветки: {text}");
    absent(format!("docs/Projects/rig{stamp}/plain/child.md")).await;
    let (err, text) = write(
        format!("Projects/rig{stamp}/plain/deep/deeper/child"),
        "probe".into(),
    )
    .await;
    assert!(err, "глубокий путь под файлом должен быть отказан: {text}");
    assert!(text.contains("parent-collision"), "имя ветки: {text}");

    let (err, text) = write(format!("Experience/rig{stamp}/sfx"), "probe".into()).await;
    assert!(err, "вложенное корнеимя должно быть отказано: {text}");
    assert!(text.contains("suffix-collision"), "имя ветки: {text}");
    assert!(
        text.contains(&format!(
            "Experience/rig{stamp}/deep/Experience/rig{stamp}/sfx"
        )),
        "кандидат суффикса: {text}"
    );
    absent(format!("docs/Experience/rig{stamp}/sfx.md")).await;

    let (err, text) = append(article.clone()).await;
    assert!(
        err,
        "append по хвосту чужой статьи должен быть отказан: {text}"
    );
    assert!(text.contains("append-nonexistent"), "имя ветки: {text}");
    assert!(
        text.contains("create it via docs_write"),
        "лекарство: {text}"
    );
    assert!(
        text.contains(&format!("Experience/cli/rig-{stamp}")),
        "кандидат суффикса: {text}"
    );
    absent(format!("docs/cli/rig-{stamp}.md")).await;

    // Живая ветка прод-писателя: GitWikiWriter::append_line → AppendMissing.
    let ghost = format!("Experience/rig{stamp}/ghost-unique");
    let (err, text) = append(ghost).await;
    assert!(err, "append к несуществующему должен быть отказан: {text}");
    assert!(text.contains("append-nonexistent"), "имя ветки: {text}");
    assert!(
        text.contains("create it via docs_write"),
        "лекарство: {text}"
    );
    assert!(
        !text.contains("suffix candidates"),
        "уникальный слаг — кандидатов нет: {text}"
    );
    absent(format!("docs/Experience/rig{stamp}/ghost-unique.md")).await;

    // --- успехи (регресс прежнего контракта) ---
    let legit = format!("Experience/rig{stamp}/legit");
    let (err, text) = write(legit.clone(), "legit body".into()).await;
    assert!(!err, "легитимный новый док должен записаться: {text}");
    assert!(text.contains("canonical fm header created"), "факт: {text}");
    let content = rig
        .read_file(&format!("docs/{legit}.md"))
        .await
        .expect("легитимный док записан");
    assert!(content.starts_with("---\n"), "fm-шапка: {content}");

    let (err, text) = write(
        format!("Projects/rig{stamp}/ADR/new-adr"),
        "adr body".into(),
    )
    .await;
    assert!(
        !err,
        "легаси-стем «файл + папка» должен пустить запись: {text}"
    );
    absent(format!("docs/Projects/rig{stamp}/ADR/new-adr.md.md")).await;

    let (err, text) = append(legit.clone()).await;
    assert!(!err, "append к живому должен пройти: {text}");
    assert!(
        text.contains(&format!("appended to «{legit}»")),
        "факт: {text}"
    );
    let content = rig
        .read_file(&format!("docs/{legit}.md"))
        .await
        .expect("док после append");
    assert!(content.contains("legit body"), "тело сохранено: {content}");
    assert!(
        content.contains("e2e probe line"),
        "строка в конце: {content}"
    );

    // --- нормализация слага ---
    let (err, text) = write(
        format!("docs/Experience/rig{stamp}/norm"),
        "norm body".into(),
    )
    .await;
    assert!(!err, "docs/-форма должна нормализоваться: {text}");
    assert!(
        text.contains(&format!("Experience/rig{stamp}/norm")),
        "ответ называет нормализованный слаг: {text}"
    );
    let (err, text) = write(format!("Experience/rig{stamp}/norm.md"), "norm v2".into()).await;
    assert!(!err, ".md-форма должна нормализоваться: {text}");
    assert!(
        text.contains("carried"),
        "существующий док — быстрый путь: {text}"
    );
    absent(format!("docs/Experience/rig{stamp}/norm.md.md")).await;

    std::fs::remove_dir_all(&dir).ok();
}
