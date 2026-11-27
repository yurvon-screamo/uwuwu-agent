//! Живая e2e-проба слаг-гарда write-путей (#1613): изолированное приватное
//! rig-репо, которое тест сам создаёт и сносит через REST
//! (POST /user/repos → DELETE /repos/{owner}/{repo}), + живой uwuwu-server
//! (прод-писатель GitWikiWriter на GitHub Git Data API, ADR-110: HTTP-моков
//! нет). Проба ветки AppendMissing писателя важна: юнитами она не покрыта.
//!
//! Детерминизм против GitHub: Contents API отстаёт от
//! git-data (в дереве файл виден через ~2 с, через contents — до ~20 с),
//! update_ref на частых коммитах изредка даёт 422 «competing commit», поэтому:
//!   - сиды PUT до старта сервера, прогон ждёт их видимости в git/trees;
//!   - положительные чтения после записи — поллинг raw contents;
//!   - нуль-запись пинится снапшотом ref/heads до/после refusal-фазы
//!     (одна пара на фазу — включая глубокий parent-проб);
//!   - финальная целостность — blob-набор дерева против ожидаемого:
//!     паразиты («….md.md», сырые «docs/…», файлы в корне) ловятся и вне
//!     пиненых путей;
//!   - сиды 422/5xx — ограниченный ретрай; между MCP-записями — пауза;
//!     «competing commit» положительной записи — один повтор (упавший
//!     ff = ничего не записано, повтор безопасен).
//!
//! Риг сносится на любом исходе (включая упавший прогон); без права
//! удаления токена — громкая паника с командой ручного сноса. Имя
//! штамповано; репо-остатки после SIGKILL чистятся по префиксу:
//! `gh repo list --limit 200 --json name -q '.[].name' | grep ^uwuwu-rig-e2e`.
//!
//! Сценарий (то, что тестировщик гонял руками на риге):
//!
//! refusal-ветки (нуль-запись — ref-снапшот):
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
//! успехи (регресс прежнего контракта; чтения — поллингом):
//!   8. легитимный новый док → каноническая fm-шапка (raw read-back);
//!   9. новый ADR под легаси-стем «файл + папка» → запись разрешена
//!      (raw read-back содержимого);
//!   10. append к живому → тело сохранено, строка в конце (ends_with);
//!   11–12. нормализация слага («docs/…», «….md») — ответ называет
//!      нормализованный слаг с разделителем «…»; финальный blob-набор
//!      исключает двойника «….md.md» и сырые пути.
//!   13. #1653: перед каждым verdict ждём видимости нужного коммита в
//!      дереве (сначала путь, перед append — новый sha blob'а), затем
//!      повтор без fm → Carried и append → строка в конце; сверка тела
//!      отличает свежую базу от предыдущей версии. Прогон печатает
//!      статус Contents (404 / 200 / прочее) и, упражнён ли witness.
//!
//! Гейты: UWUWU_E2E=1; GH_TOKEN со scope repo (создание/запись) и delete_repo.
//! Внешнее репо не требуется: риг создаёт сам тест.
//!
//! Запуск:
//!   UWUWU_E2E=1 cargo test -p uwuwu-server --test slug_guard_live_e2e -- --nocapture

use base64::Engine as _;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

/// Верхняя граница ожидания видимости в GitHub: Contents отстаёт от
/// git-data-коммита до десятков секунд.
const WAIT_TIMEOUT: Duration = Duration::from_secs(75);
const POLL_STEP: Duration = Duration::from_secs(1);
/// Пауза между MCP-записями: митигация гонки update_ref (422 «competing
/// commit» без конкурента).
const WRITE_PAUSE: Duration = Duration::from_millis(1500);
/// Ограниченный ретрай одиночных REST-чтений на транзиентный 5xx/сетевой сбой.
const API_RETRIES: u32 = 3;
const API_RETRY_PAUSE: Duration = Duration::from_secs(2);
/// Буфер после первой видимости файла в contents: сервер читает тот же API,
/// но ответы могут идти с разных узлов — страховка на гране.
const VISIBILITY_GRACE: Duration = Duration::from_millis(1500);

fn e2e_ready() -> Option<String> {
    if std::env::var("UWUWU_E2E").ok().as_deref() != Some("1") {
        return None;
    }
    Some(
        std::env::var("GH_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .expect("GH_TOKEN обязателен (создание/запись/снос rig-репо)"),
    )
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

fn log_tail(path: &Path) -> String {
    let binding = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = binding.lines().collect();
    let start = lines.len().saturating_sub(30);
    lines[start..].join("\n")
}

struct Stand {
    dir: PathBuf,
    /// Mutex даёт `try_wait(&mut self)` через неизменяемую ссылку в mcp_call.
    server: Mutex<Child>,
    port: u16,
    bearer: String,
}

impl Drop for Stand {
    fn drop(&mut self) {
        if let Ok(mut child) = self.server.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Спавн живого сервера против rig-репо: прод-драйвер github (писатель
/// GitWikiWriter), observe-режим (тики без мутаций, MCP-тулы живы).
async fn spawn_server(dir: &Path, repo: &str) -> Stand {
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
        // Окружение запустившего не протекает в стенд: GEMINI_API_KEY включает
        // судью, UWUWU_EMBED_URL — LanceDB/FTS в каталоге стенда (#1687).
        .env_remove("GEMINI_API_KEY")
        .env_remove("UWUWU_EMBED_URL")
        .stdout(Stdio::from(log.try_clone().expect("log clone")))
        .stderr(Stdio::from(log))
        .spawn()
        .expect("spawn uwuwu-server");
    let stand = Stand {
        dir: dir.to_path_buf(),
        server: Mutex::new(server),
        port,
        bearer: bearer.into(),
    };
    let http = reqwest::Client::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
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
        .unwrap_or_else(|e| {
            let fate = match stand.server.lock().expect("server lock").try_wait() {
                Ok(Some(status)) => format!("процесс завершился: {status}"),
                Ok(None) => "процесс жив".to_string(),
                Err(err) => format!("статус недоступен: {err}"),
            };
            panic!(
                "mcp transport: {e}; сервер: {fate}\n{}",
                log_tail(&stand.dir.join("server.log"))
            );
        });
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

/// Положительная запись через MCP с одним повтором при «competing commit»:
/// упавший update_ref ff не двигает ветку (коммит без ref — ничего не
/// записано), поэтому повтор безопасен.
async fn mcp_write(
    http: &reqwest::Client,
    stand: &Stand,
    tool: &str,
    args: Value,
) -> (bool, String) {
    let (err, text) = mcp_call(http, stand, tool, args.clone()).await;
    if err && text.contains("competing commit") {
        println!("MCP {tool}: competing commit — повтор через 3 с");
        tokio::time::sleep(Duration::from_secs(3)).await;
        return mcp_call(http, stand, tool, args).await;
    }
    (err, text)
}

async fn api_call(
    http: &reqwest::Client,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> (reqwest::StatusCode, Value) {
    let mut req = http
        .request(method, format!("https://api.github.com{path}"))
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "uwuwu-rig-e2e")
        .timeout(Duration::from_secs(30));
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = match req.send().await {
        Ok(resp) => resp,
        // Сетевой сбой — транзиентный, как 5xx. Поллинг считает его «ещё не
        // видно», одиночные чтения — ретраем, записи — своим ретраем.
        Err(e) => {
            return (
                reqwest::StatusCode::SERVICE_UNAVAILABLE,
                json!({"transport_error": e.to_string()}),
            )
        }
    };
    let status = resp.status();
    let value = resp.json().await.unwrap_or(Value::Null);
    (status, value)
}

/// Ограниченный ретрай на 5xx и транспортные сбои для одиночных чтений.
/// 4xx не ретраим: это честный отказ API, а не транзиент.
async fn api_call_retry(
    http: &reqwest::Client,
    token: &str,
    method: reqwest::Method,
    path: &str,
) -> (reqwest::StatusCode, Value) {
    let mut last = (reqwest::StatusCode::SERVICE_UNAVAILABLE, Value::Null);
    for attempt in 1..=API_RETRIES {
        last = api_call(http, token, method.clone(), path, None).await;
        if !last.0.is_server_error() {
            return last;
        }
        if attempt < API_RETRIES {
            tokio::time::sleep(API_RETRY_PAUSE).await;
        }
    }
    last
}

/// Rig-репо: тест сам создаёт (POST /user/repos, приватное, auto-init) и сам
/// сносит (DELETE /repos/{owner}/{repo}) — фикстура поднимается из репо и не
/// оставляет следов. Имя только с префиксом `uwuwu-rig-e2e-`: POST создаёт,
/// чужое/существующее репо подставить нельзя.
#[derive(Clone)]
struct RigGithub {
    http: reqwest::Client,
    token: String,
    /// full_name рига («owner/uwuwu-rig-e2e-<stamp>»)
    repo: String,
    /// ветка по умолчанию из ответа создания (не хардкод «main»)
    branch: String,
    /// Проверки ответа создания: сценарий паникует уже после POST, чтобы
    /// тест успел снести репо на любом исходе.
    creation_error: Option<String>,
}

/// Если тест упал до вызова delete — громкая инструкция о ручном сносе.
struct RigLease {
    repo: String,
    deleted: bool,
}

impl Drop for RigLease {
    fn drop(&mut self) {
        if !self.deleted && !self.repo.is_empty() {
            eprintln!(
                "ВНИМАНИЕ: риг {} не снесён — удали вручную: gh api -X DELETE /repos/{}",
                self.repo, self.repo
            );
        }
    }
}

impl RigGithub {
    async fn create(http: &reqwest::Client, token: &str, stamp: &str) -> Self {
        let name = format!("uwuwu-rig-e2e-{stamp}");
        let payload = json!({
            "name": name,
            "private": true,
            "auto_init": true,
            "has_issues": false,
            "has_wiki": false,
            "has_projects": false,
        });
        let mut repo = String::new();
        let mut branch = String::new();
        let mut private_ok = false;
        let mut creation_error = None;
        for attempt in 1..=API_RETRIES {
            let (status, body) = api_call(
                http,
                token,
                reqwest::Method::POST,
                "/user/repos",
                Some(payload.clone()),
            )
            .await;
            if status.is_success() {
                repo = body["full_name"].as_str().unwrap_or("").to_string();
                branch = body["default_branch"].as_str().unwrap_or("").to_string();
                private_ok = body["private"].as_bool() == Some(true);
                break;
            }
            if !status.is_server_error() && status.as_u16() != 429 {
                creation_error = Some(format!("rig-репо не создано: HTTP {status} — {body}"));
                break;
            }
            // 5xx/429 после POST: репо мог создаться, а ответ пропал.
            // Проверяем точное имя и подхватываем его — повтор POST создал бы дубль.
            println!("создание рига: HTTP {status} — проверяю, не создан ли репо");
            let (_, user) = api_call_retry(http, token, reqwest::Method::GET, "/user").await;
            let owner = user["login"].as_str().unwrap_or("").to_string();
            let (found, existing) = api_call_retry(
                http,
                token,
                reqwest::Method::GET,
                &format!("/repos/{owner}/{name}"),
            )
            .await;
            if found.is_success() {
                repo = existing["full_name"].as_str().unwrap_or("").to_string();
                branch = existing["default_branch"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                private_ok = existing["private"].as_bool() == Some(true);
                break;
            }
            if attempt == API_RETRIES {
                creation_error = Some(format!(
                    "rig-репо не создано после {API_RETRIES} попыток: HTTP {status} — {body}"
                ));
            } else {
                tokio::time::sleep(API_RETRY_PAUSE).await;
            }
        }
        // Ни одной паники после POST: ошибка ответа уходит в сценарий, который
        // исполняется внутри spawn — teardown всё равно состоится.
        if creation_error.is_none() {
            if !repo.ends_with(&format!("/uwuwu-rig-e2e-{stamp}")) {
                creation_error = Some(format!("имя рига не совпало с ожидаемым префиксом: {repo}"));
            } else if !private_ok {
                creation_error = Some("риг обязан быть приватным".to_string());
            }
        }
        Self {
            http: http.clone(),
            token: token.to_string(),
            repo,
            branch,
            creation_error,
        }
    }

    /// Снос рига: ошибка возвращается, а не паникует — сообщение упавшего
    /// сценария не должно теряться под паникой сноса. DELETE идемпотентен,
    /// поэтому здесь ретрай на транзиентные сбои безопасен.
    async fn delete(&self) -> Result<(), String> {
        let (status, body) = api_call_retry(
            &self.http,
            &self.token,
            reqwest::Method::DELETE,
            &format!("/repos/{}", self.repo),
        )
        .await;
        if status.is_success() {
            Ok(())
        } else {
            Err(format!("HTTP {status} — {body}"))
        }
    }

    fn contents_path(&self, path: &str) -> String {
        format!("/repos/{}/contents/{path}", self.repo)
    }

    /// PUT сида с ограниченным ретраем: GitHub изредка отдаёт 409/422 (гонки
    /// ref) и таймауты шлюза (5xx). Сид — новый файл, повтор безопасен.
    async fn put_file(&self, path: &str, content: &str) {
        let payload = json!({
            "message": format!("rig e2e seed: {path}"),
            "content": base64::engine::general_purpose::STANDARD.encode(content),
            "branch": &self.branch,
        });
        let mut last = (reqwest::StatusCode::PRECONDITION_FAILED, Value::Null);
        for _attempt in 1..=3 {
            let res = api_call(
                &self.http,
                &self.token,
                reqwest::Method::PUT,
                &self.contents_path(path),
                Some(payload.clone()),
            )
            .await;
            if res.0.is_success() {
                return;
            }
            last = res;
            if !matches!(last.0.as_u16(), 409 | 422 | 500..=599) {
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        panic!(
            "сид {path} не записан (3 попытки): HTTP {} — {}",
            last.0, last.1
        );
    }

    /// Raw contents GET: None — 404 (ещё не видно либо отсутствует).
    async fn read_file(&self, path: &str) -> Option<String> {
        let (status, body) = api_call(
            &self.http,
            &self.token,
            reqwest::Method::GET,
            &self.contents_path(path),
            None,
        )
        .await;
        // 404 — ещё не видно; 5xx и transport (503) — тоже транзиент:
        // поллинг продолжится до дедлайна, а не упадёт на первом сбое.
        if status.as_u16() == 404 || status.is_server_error() || status.as_u16() == 429 {
            return None;
        }
        assert!(status.is_success(), "GET {path}: HTTP {status} — {body}");
        let b64 = body["content"]
            .as_str()
            .unwrap_or_default()
            .replace('\n', "");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .expect("base64 дока рига");
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Blob-пути дерева (git/trees, recursive). Дерево — источник для сид-
    /// ожидания и финальной целостности: от contents не зависит.
    async fn tree_blobs(&self) -> Vec<String> {
        let (status, body) = api_call(
            &self.http,
            &self.token,
            reqwest::Method::GET,
            &format!("/repos/{}/git/trees/{}?recursive=1", self.repo, self.branch),
            None,
        )
        .await;
        // Транзиентный 5xx/429 в поллинге дерева — «ещё не видно».
        if status.is_server_error() || status.as_u16() == 429 {
            return Vec::new();
        }
        assert!(
            status.is_success(),
            "дерево не прочитано: HTTP {status} — {body}"
        );
        assert_eq!(
            body["truncated"].as_bool(),
            Some(false),
            "дерево обрезано — риг не должен быть таким большим"
        );
        body["tree"]
            .as_array()
            .map(|t| {
                t.iter()
                    .filter(|e| e["type"].as_str() == Some("blob"))
                    .filter_map(|e| e["path"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// SHA ветки (git/ref/heads) — факт записи на уровне ref: сдвинулся —
    /// была запись, без зависимости от лага contents.
    async fn ref_sha(&self) -> String {
        let (status, body) = api_call_retry(
            &self.http,
            &self.token,
            reqwest::Method::GET,
            &format!("/repos/{}/git/ref/heads/{}", self.repo, self.branch),
        )
        .await;
        assert!(
            status.is_success(),
            "ref не прочитан: HTTP {status} — {body}"
        );
        body["object"]["sha"]
            .as_str()
            .expect("sha ветки")
            .to_string()
    }

    /// Ждать видимости всех сидов в дереве до старта сервера: дерево попадает
    /// в TTL-кеш list_docs первым снепшотом (лаг contents-PUT 1.8–2.7 с).
    async fn wait_tree(&self, seed_paths: &[String]) {
        let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
        loop {
            let blobs = self.tree_blobs().await;
            let missing: Vec<&String> = seed_paths.iter().filter(|p| !blobs.contains(p)).collect();
            if missing.is_empty() {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "сиды не видны в дереве за {WAIT_TIMEOUT:?}: нет {missing:?}; blobs: {blobs:?}"
            );
            tokio::time::sleep(POLL_STEP).await;
        }
    }

    /// SHA blob'а по пути в свежем дереве: None — путь ещё не виден либо
    /// ответ транзиентный (тогда поллинг продолжится).
    async fn tree_blob_sha(&self, path: &str) -> Option<String> {
        let (status, body) = api_call_retry(
            &self.http,
            &self.token,
            reqwest::Method::GET,
            &format!("/repos/{}/git/trees/{}?recursive=1", self.repo, self.branch),
        )
        .await;
        if !status.is_success() || body["truncated"].as_bool() == Some(true) {
            return None;
        }
        body["tree"]
            .as_array()?
            .iter()
            .find(|e| e["path"].as_str() == Some(path) && e["type"].as_str() == Some("blob"))?
            ["sha"]
            .as_str()
            .map(str::to_string)
    }

    /// Ждать, пока дерево увидит НОВЫЙ blob по пути (коммит перезаписи),
    /// а не только сам путь от первой записи.
    async fn wait_tree_sha(&self, path: &str, previous: &str) {
        let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
        loop {
            if let Some(sha) = self.tree_blob_sha(path).await {
                if sha != previous {
                    return;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "дерево не увидело новый blob {path} за {WAIT_TIMEOUT:?}"
            );
            tokio::time::sleep(POLL_STEP).await;
        }
    }

    /// Статус raw contents: отличать 404 (лаг) от 5xx/429 (замер ненадёжен).
    async fn contents_status(&self, path: &str) -> reqwest::StatusCode {
        let (status, _) = api_call(
            &self.http,
            &self.token,
            reqwest::Method::GET,
            &self.contents_path(path),
            None,
        )
        .await;
        status
    }

    /// Положительное чтение после записи поллингом: contents отдаёт 404 до
    /// ~20 с после git-data-коммита. Возвращает прочитанное после грэйса
    /// (страховка до серверного чтения); грэйс тоже поллится — 5xx в нём
    /// не считается исчезновением файла.
    async fn wait_file(&self, path: &str, needle: &str) -> String {
        let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
        let mut visible_at: Option<tokio::time::Instant> = None;
        loop {
            if let Some(content) = self.read_file(path).await {
                if content.contains(needle) {
                    match visible_at {
                        None => visible_at = Some(tokio::time::Instant::now()),
                        Some(at) if tokio::time::Instant::now() >= at + VISIBILITY_GRACE => {
                            return content
                        }
                        _ => {}
                    }
                } else {
                    visible_at = None;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "файл {path} (needle «{needle}») не виден за {WAIT_TIMEOUT:?}: \
                 contents отстаёт от git-data либо запись не прошла"
            );
            tokio::time::sleep(POLL_STEP).await;
        }
    }

    /// Финальная целостность: blob-набор дерева сходится к ожидаемому
    /// (паразит anywhere — красный, не только на пиненых путях).
    async fn wait_final_blobs(&self, mut expected: Vec<String>) {
        expected.sort();
        let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
        loop {
            let mut blobs = self.tree_blobs().await;
            blobs.sort();
            if blobs == expected {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "финальное дерево не сошлось за {WAIT_TIMEOUT:?}: got {blobs:?}, \
                 expected {expected:?} — паразит либо лаг"
            );
            tokio::time::sleep(POLL_STEP).await;
        }
    }
}

async fn run_scenario(dir: PathBuf, rig: RigGithub, stamp: String) {
    // Паника здесь безопасна: сценарий уже в подзадаче, внешний тест снесёт риг.
    if let Some(error) = &rig.creation_error {
        panic!("{error}");
    }
    let seed_doc = "---\ntitle: rig-article\ndate: 2026-01-01T00:00:00+09:00\nlastmod: 2026-01-01T00:00:00+09:00\n---\n\nseed body line\n";
    let seeds: Vec<String> = vec![
        format!("docs/Experience/cli/rig-{stamp}.md"),
        format!("docs/Projects/rig{stamp}/plain.md"),
        format!("docs/Projects/rig{stamp}/ADR.md"),
        format!("docs/Projects/rig{stamp}/ADR/seed.md"),
        format!("docs/Experience/rig{stamp}/deep/Experience/rig{stamp}/sfx.md"),
    ];
    for path in &seeds {
        rig.put_file(path, seed_doc).await;
        tokio::time::sleep(WRITE_PAUSE).await;
    }
    // Сервер стартует, когда дерево видит все сиды: первый list_docs — свежий.
    rig.wait_tree(&seeds).await;

    let stand = spawn_server(&dir, &rig.repo).await;
    let article = format!("cli/rig-{stamp}"); // хвост живой статьи (класс #1219)
    let write = |slug: String, content: String| {
        let http = rig.http.clone();
        let stand = &stand;
        async move {
            mcp_write(
                &http,
                stand,
                "docs_write",
                json!({"slug": slug, "content": content}),
            )
            .await
        }
    };
    let append = |slug: String| {
        let http = rig.http.clone();
        let stand = &stand;
        async move {
            mcp_write(
                &http,
                stand,
                "docs_append",
                json!({"slug": slug, "line": "e2e probe line"}),
            )
            .await
        }
    };

    // --- refusal-ветки (фича); нуль-запись — снапшотом ветки на всю фазу:
    // ref сдвинулся = была запись, без зависимости от лага contents ---
    let pre = rig.ref_sha().await;

    let (err, text) = write(article.clone(), "probe".into()).await;
    assert!(err, "хвост живой статьи должен быть отказан: {text}");
    assert!(text.contains("root-prefix"), "имя ветки: {text}");
    assert!(
        text.contains(&format!("Experience/cli/rig-{stamp}")),
        "суффикс-кандидат: {text}"
    );

    let (err, text) = write(format!("platform/unique-{stamp}"), "probe".into()).await;
    assert!(err, "чужой корень должен быть отказан: {text}");
    assert!(text.contains("root-prefix"), "имя ветки: {text}");
    assert!(
        text.contains("Experience, Projects, bot, cron, person"),
        "канонический набор в отказе: {text}"
    );

    let (err, text) = write(format!("rootfile{stamp}"), "probe".into()).await;
    assert!(err, "одно-сегментный таргет должен быть отказан: {text}");
    assert!(
        text.contains("would be a file in the wiki root"),
        "факт ветки (файл в корне): {text}"
    );

    let (err, text) = write(format!("Projects/rig{stamp}/plain/child"), "probe".into()).await;
    assert!(
        err,
        "запись под чистый файл-родитель должна быть отказана: {text}"
    );
    assert!(text.contains("parent-collision"), "имя ветки: {text}");
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

    let post = rig.ref_sha().await;
    assert_eq!(
        pre, post,
        "нуль-запись нарушена: ветка {} сдвинулась ({pre} → {post})",
        rig.branch
    );

    // --- успехи (регресс прежнего контракта); между записями пауза —
    // митигация update_ref-гонок, чтения после записей — поллинг ---
    let legit = format!("Experience/rig{stamp}/legit");
    let (err, text) = write(legit.clone(), "legit body".into()).await;
    assert!(!err, "легитимный новый док должен записаться: {text}");
    assert!(
        text.contains(&format!(
            "doc «{legit}» written — canonical fm header created"
        )),
        "факт записи с канонической шапкой: {text}"
    );
    let content = rig
        .wait_file(&format!("docs/{legit}.md"), "legit body")
        .await;
    assert!(content.starts_with("---\n"), "fm-шапка: {content}");

    tokio::time::sleep(WRITE_PAUSE).await;
    let adr = format!("Projects/rig{stamp}/ADR/new-adr");
    let (err, text) = write(adr.clone(), "adr body".into()).await;
    assert!(
        !err,
        "легаси-стем «файл + папка» должен пустить запись: {text}"
    );
    // Позитивное чтение содержимого: стем-запись состоялась по адресу.
    let content = rig.wait_file(&format!("docs/{adr}.md"), "adr body").await;
    assert!(
        content.starts_with("---\n"),
        "fm-шапка нового ADR: {content}"
    );

    tokio::time::sleep(WRITE_PAUSE).await;
    let (err, text) = append(legit.clone()).await;
    assert!(!err, "append к живому должен пройти: {text}");
    assert!(
        text.contains(&format!("appended to «{legit}»")),
        "факт: {text}"
    );
    let content = rig
        .wait_file(&format!("docs/{legit}.md"), "e2e probe line")
        .await;
    assert!(content.contains("legit body"), "тело сохранено: {content}");
    assert!(
        content.trim_end().ends_with("e2e probe line"),
        "строка в конце (не в начале): {content}"
    );

    tokio::time::sleep(WRITE_PAUSE).await;
    let norm_slug = format!("Experience/rig{stamp}/norm");
    // Разделитель «…» в ответе отличает нормализованный слаг от сырого
    // «docs/Experience/…».
    let (err, text) = write(format!("docs/{norm_slug}"), "norm body".into()).await;
    assert!(!err, "docs/-форма должна нормализоваться: {text}");
    assert!(
        text.contains(&format!(
            "doc «{norm_slug}» written — canonical fm header created"
        )),
        "ответ называет нормализованный слаг: {text}"
    );
    rig.wait_file(&format!("docs/{norm_slug}.md"), "norm body")
        .await;

    tokio::time::sleep(WRITE_PAUSE).await;
    let (err, text) = write(format!("{norm_slug}.md"), "norm v2".into()).await;
    assert!(!err, ".md-форма должна нормализоваться: {text}");
    assert!(
        text.contains(&format!(
            "doc «{norm_slug}» written — fm header carried over"
        )),
        "существующий док — быстрый путь по нормализованному слагу: {text}"
    );
    // Последняя запись — чтение назад: тело заменено, fm-шапка сохранена.
    let content = rig
        .wait_file(&format!("docs/{norm_slug}.md"), "norm v2")
        .await;
    assert!(
        content.starts_with("---\n"),
        "fm-шапка после carried: {content}"
    );
    assert!(
        !content.contains("norm body"),
        "тело заменено полностью: {content}"
    );

    // --- #1653: вердикты проверяются при выполненном предусловии фикса:
    // дерево видит нужный коммит, Contents может ещё отставать. Ожидание
    // перед каждым шагом держит проверку вне остаточного окна.
    let fresh = format!("Experience/rig{stamp}/fresh");
    let (err, text) = write(
        fresh.clone(),
        "---\ntitle: witness-original\ndate: 2026-01-01T00:00:00+09:00\n---\n\nfirst body\n".into(),
    )
    .await;
    assert!(!err, "свежая запись с fm: {text}");
    let fresh_path = format!("docs/{fresh}.md");
    rig.wait_tree(std::slice::from_ref(&fresh_path)).await;
    let first_sha = rig
        .tree_blob_sha(&fresh_path)
        .await
        .expect("blob первой записи в дереве");
    let status = rig.contents_status(&fresh_path).await;
    println!(
        "блок #1653 перед повторной записью: Contents {status} — {}",
        if status.as_u16() == 404 {
            "лаг проявился, путь свидетеля упражнён"
        } else if status.is_success() {
            "лага нет, путь свидетеля не упражнён"
        } else {
            "статус не 200/404 — замер лага недостоверен"
        }
    );
    let (err, text) = write(fresh.clone(), "second body\n".into()).await;
    assert!(!err, "повторная запись после commit'а: {text}");
    assert!(
        text.contains("fm header carried over"),
        "в окне лага ожидался Carried, а не Created: {text}"
    );
    // Для append важен коммит перезаписи: одного наличия пути в дереве
    // недостаточно — дерево может хранить blob первой версии.
    rig.wait_tree_sha(&fresh_path, &first_sha).await;
    let status = rig.contents_status(&fresh_path).await;
    println!(
        "блок #1653 перед append: Contents {status} — {}",
        if status.as_u16() == 404 {
            "лаг проявился, путь свидетеля упражнён"
        } else if status.is_success() {
            "лага нет, путь свидетеля не упражнён"
        } else {
            "статус не 200/404 — замер лага недостоверен"
        }
    );
    let (err, text) = append(fresh.clone()).await;
    assert!(!err, "append после коммита перезаписи: {text}");
    assert!(
        text.contains("appended to"),
        "ложный append-nonexistent в окне лага: {text}"
    );
    let content = rig.wait_file(&fresh_path, "e2e probe line").await;
    assert!(
        content.contains("title: witness-original"),
        "шапка потеряна при повторной записи: {content}"
    );
    assert!(
        content.contains("second body"),
        "append взял за базу предыдущую версию — свежая запись потеряна: {content}"
    );
    assert!(
        !content.contains("first body"),
        "append обновил старое тело вместо свежего: {content}"
    );
    assert!(
        content.trim_end().ends_with("e2e probe line"),
        "строка append'а не в конце: {content}"
    );

    // --- финальная целостность: blob-набор дерева в точности ожидаемый.
    // Двойники «….md.md», сырые «docs/…»-пути и корневые файлы — красные
    // даже вне пиненых путей.
    let mut expected = vec!["README.md".to_string()];
    expected.extend(seeds);
    expected.extend([
        format!("docs/{legit}.md"),
        format!("docs/{adr}.md"),
        format!("docs/{norm_slug}.md"),
        format!("docs/{fresh}.md"),
    ]);
    rig.wait_final_blobs(expected).await;
}

/// #1658: живой прогон docs_delete на том же риге — сид через MCP,
/// снос тулом, проверка absence, восстановление из бэкапа по ключу из
/// квитанции и повторный снос. Мок не засчитывается (персона 09.11).
async fn run_doc_delete_scenario(dir: std::path::PathBuf, rig: RigGithub) {
    if let Some(error) = &rig.creation_error {
        panic!("{error}");
    }
    let stand = spawn_server(&dir, &rig.repo).await;
    let http = reqwest::Client::new();
    let slug = "Projects/uwuwu/Удаляемый-1658";
    let body = "---\ntitle: Удаляемый 1658\ndate: 2026-11-26\n---\n\nживой прогон\n";

    let (err, text) = mcp_write(
        &http,
        &stand,
        "docs_write",
        json!({ "slug": slug, "content": body }),
    )
    .await;
    assert!(!err, "сид docs_write: {text}");
    rig.wait_file(&format!("docs/{slug}.md"), "живой прогон")
        .await;

    let (err, receipt) = mcp_call(&http, &stand, "docs_delete", json!({ "slug": slug })).await;
    assert!(!err, "docs_delete: {receipt}");
    assert!(
        receipt.starts_with(&format!("deleted: «{slug}»")),
        "{receipt}"
    );
    assert!(receipt.contains("\nbackup: blobs/"), "{receipt}");
    assert!(
        receipt.contains("\npost-check: absent in tree at "),
        "{receipt}"
    );

    let (err, listing) = mcp_call(
        &http,
        &stand,
        "docs_ls",
        json!({ "path": "Projects/uwuwu" }),
    )
    .await;
    assert!(!err, "docs_ls: {listing}");
    assert!(
        !listing.contains("Удаляемый-1658"),
        "док остался: {listing}"
    );
    let (err, read) = mcp_call(&http, &stand, "docs_read", json!({ "slug": slug })).await;
    assert!(err && read.contains("not found"), "docs_read: {read}");

    let key = receipt
        .lines()
        .find_map(|line| line.strip_prefix("backup: "))
        .and_then(|line| line.split_whitespace().next())
        .expect("ключ бэкапа в квитанции");
    let (err, blob_answer) = mcp_call(&http, &stand, "blob_get", json!({ "key": key })).await;
    assert!(!err, "blob_get: {blob_answer}");
    let url = blob_answer
        .lines()
        .find_map(|line| line.strip_prefix("URL (TTL 7 days): "))
        .expect("presigned GET в ответе blob_get");
    let restored = http
        .get(url)
        .send()
        .await
        .expect("GET бэкапа")
        .text()
        .await
        .expect("тело бэкапа");
    assert_eq!(restored, body, "бэкап не совпал с удалённым доком");

    let (err, written) = mcp_write(
        &http,
        &stand,
        "docs_write",
        json!({ "slug": slug, "content": restored.clone() }),
    )
    .await;
    assert!(!err, "восстановление: {written}");
    rig.wait_file(&format!("docs/{slug}.md"), "живой прогон")
        .await;
    let (err, after) = mcp_call(&http, &stand, "docs_read", json!({ "slug": slug })).await;
    assert!(
        !err && after.trim_end() == restored.trim_end(),
        "read-back: {after}"
    );

    let (err, second) = mcp_call(&http, &stand, "docs_delete", json!({ "slug": slug })).await;
    assert!(
        !err && second.starts_with("deleted:"),
        "финальный снос: {second}"
    );
    println!("docs_delete live: бэкап {key}, восстановление и повторный снос прошли");
}

#[tokio::test]
async fn docs_delete_live_e2e_backup_and_restore() {
    let Some(token) = e2e_ready() else {
        println!("e2e выключен: UWUWU_E2E=1 для живой пробы docs_delete");
        return;
    };
    for name in [
        "UWUWU_S3_ENDPOINT",
        "UWUWU_S3_REGION",
        "UWUWU_S3_BUCKET",
        "UWUWU_S3_ACCESS_KEY",
        "UWUWU_S3_SECRET",
    ] {
        if std::env::var(name).unwrap_or_default().trim().is_empty() {
            println!("e2e docs_delete пропущен: нет {name} — бэкап в Tigris недоступен");
            return;
        }
    }
    let stamp = format!(
        "{}{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
        std::process::id()
    );
    let dir = std::env::temp_dir().join(format!("uwuwu-rig-e2e-dd-{stamp}"));
    std::fs::create_dir_all(&dir).expect("rig dir");
    let http = reqwest::Client::new();
    let rig = RigGithub::create(&http, &token, &stamp).await;
    let mut lease = RigLease {
        repo: rig.repo.clone(),
        deleted: false,
    };
    let outcome = tokio::spawn(run_doc_delete_scenario(dir.clone(), rig.clone())).await;
    let scenario_error = outcome.err().map(|join| {
        join.into_panic()
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "паника без сообщения".to_string())
    });
    let log_tail = log_tail(&dir.join("server.log"));
    let _ = std::fs::remove_dir_all(&dir);
    let delete_result = rig.delete().await;
    lease.deleted = delete_result.is_ok();
    let mut problems = Vec::new();
    if let Some(msg) = scenario_error {
        problems.push(format!(
            "docs_delete e2e упал\n{log_tail}\n--- паника ---\n{msg}"
        ));
    }
    if let Err(error) = delete_result {
        problems.push(format!(
            "риг {} не снесён: {error} — снеси руками: gh api -X DELETE /repos/{}",
            rig.repo, rig.repo
        ));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n\n"));
}

#[tokio::test]
async fn slug_guard_live_e2e_main_scenario() {
    let Some(token) = e2e_ready() else {
        println!("e2e выключен: UWUWU_E2E=1 для живой пробы (риг тест создаёт сам)");
        return;
    };
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
    let http = reqwest::Client::new();

    let rig = RigGithub::create(&http, &token, &stamp).await;
    // Guard с именем рига: если между POST и сносом всё же случится unwinding,
    // информация о ручном сносе не пропадёт.
    let mut lease = RigLease {
        repo: rig.repo.clone(),
        deleted: false,
    };
    println!(
        "риг создан: {} (ветка {}, стенд {dir:?})",
        rig.repo, rig.branch
    );

    // Сценарий — в подзадаче: teardown исполняется на любом исходе. Между POST
    // и spawn нет паник: проверки ответа перенесены в сам сценарий.
    let outcome = tokio::spawn(run_scenario(dir.clone(), rig.clone(), stamp.clone())).await;
    let scenario_error = outcome.err().map(|join| {
        let payload = join.into_panic();
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&'static str>()
                    .map(|s| (*s).to_string())
            })
            .unwrap_or_else(|| "паника без сообщения".to_string())
    });
    let log_tail = log_tail(&dir.join("server.log"));
    let _ = std::fs::remove_dir_all(&dir);
    let delete_result = rig.delete().await;
    lease.deleted = delete_result.is_ok();
    if delete_result.is_ok() {
        println!("риг снесён: {}", rig.repo);
    }

    // Сначала сохраняем оба факта, затем одна громкая паника: отказ сноса
    // не заменяет сообщение упавшего сценария.
    let mut problems = Vec::new();
    if let Some(msg) = scenario_error {
        problems.push(format!(
            "сценарий рига упал (каталог стенда удалён)\n\
             --- server.log (хвост) ---\n{log_tail}\n--- паника ---\n{msg}"
        ));
    }
    if let Err(error) = delete_result {
        problems.push(format!(
            "риг {repo} не снесён: {error}\n\
             снеси руками: gh api -X DELETE /repos/{repo}",
            repo = rig.repo
        ));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n\n"));
}
