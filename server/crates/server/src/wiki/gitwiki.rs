//! Запись в вики поверх GitHub Git Data API (blob→tree→commit→ref):
//! ff-only (update_ref без force), append-семантика с read-modify-write.
//! Единый писатель — сервер (конвейер), конфликт ff громко отвергается.

use serde_json::{json, Value};
use uwuwu_usecase::{DocDeleteCommit, PostCheck, UsecaseError, UsecaseResult};

use crate::wiki::encode_path;

/// Снимок версии дока для проверки перед коммитом (#1697): путь и blob sha
/// из живого чтения. Проверяется в дереве того же head, от которого строится
/// новый tree, — ff-only update_ref закрывает оставшееся окно гонки.
#[derive(Debug, Clone, Copy)]
struct ExpectedBlob<'a> {
    path: &'a str,
    sha: &'a str,
}

pub struct GitWikiWriter {
    repo: String,
    branch: String,
    token: String,
    http: reqwest::Client,
}

impl GitWikiWriter {
    pub fn new(repo: String, branch: String, token: String) -> Self {
        Self {
            repo,
            branch,
            token,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> UsecaseResult<(reqwest::StatusCode, Value)> {
        let mut req = self
            .http
            .request(method, format!("https://api.github.com{path}"))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json");
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| UsecaseError::Board(format!("gitwiki transport {path}: {e}")))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Board(format!("gitwiki body {path}: {e}")))?;
        if let Some(fail) = crate::gherr::classify(status, &headers) {
            return Err(Self::err(format!("{path}: {fail}")));
        }
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        Ok((status, value))
    }

    fn err(msg: String) -> UsecaseError {
        UsecaseError::Board(format!("gitwiki: {msg}"))
    }

    /// Читает текущее содержимое файла; на Contents-404 сверяется со
    /// свежим деревом: None — файла нет и в дереве (#1653).
    pub async fn read(&self, path: &str) -> UsecaseResult<Option<String>> {
        let (status, body) = self
            .call(reqwest::Method::GET, &contents_url(&self.repo, path), None)
            .await?;
        if status.as_u16() == 404 {
            // #1653: append не должен отказывать ложным AppendMissing
            // по устаревшему Contents-чтению; дерево — свежий свидетель.
            return crate::wiki::witness::TreeWitness {
                http: &self.http,
                token: &self.token,
                repo: &self.repo,
                branch: &self.branch,
            }
            .read(path)
            .await;
        }
        if !status.is_success() {
            return Err(Self::err(format!("read {path}: HTTP {status}")));
        }
        let b64 = body["content"]
            .as_str()
            .unwrap_or_default()
            .replace('\n', "");
        let bytes = base64_std(&b64).ok_or_else(|| Self::err(format!("read {path}: base64")))?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// Коммит файла с сообщением (ff-only на branch).
    pub async fn commit_file(&self, path: &str, content: &str, message: &str) -> UsecaseResult<()> {
        self.commit_bytes(path, content.as_bytes(), message).await
    }

    /// Коммит бинарного файла (ассеты ботов, #1171): blob — base64
    /// encoding (git-блоб нейтрален к способу загрузки, md-текст в UI
    /// остаётся текстом), механика ref→tree→commit→ref ff — общая.
    pub async fn commit_bytes(
        &self,
        path: &str,
        content: &[u8],
        message: &str,
    ) -> UsecaseResult<()> {
        let (_, blob) = self
            .call(
                reqwest::Method::POST,
                &format!("/repos/{}/git/blobs", self.repo),
                Some(json!({
                    "content": base64_encode(content),
                    "encoding": "base64"
                })),
            )
            .await?;
        let blob_sha = blob["sha"].as_str().unwrap_or_default().to_string();
        let entry = json!({ "path": path, "mode": "100644", "type": "blob", "sha": blob_sha });
        self.commit_tree(json!([entry]), message, None).await?;
        Ok(())
    }

    /// Удаление файла: tree-entry с sha=null (Git Data API) + ff-only ref.
    /// Пост-проверка идёт по дереву нового коммита, а не по кэшу Contents.
    pub async fn delete_file(
        &self,
        path: &str,
        old_blob_sha: &str,
        message: &str,
    ) -> UsecaseResult<DocDeleteCommit> {
        let expected = ExpectedBlob {
            path,
            sha: old_blob_sha,
        };
        let commit_sha = self
            .commit_tree(tree_delete_entry(path), message, Some(expected))
            .await?;
        let post_check = self.path_absent_at(&commit_sha, path).await;
        Ok(DocDeleteCommit {
            commit_sha,
            post_check,
        })
    }

    /// ref → commit → base tree → (предпроверка снимка) → новый tree →
    /// commit → ff-only update_ref. Предпроверка идёт по дереву того же
    /// head, из которого берётся base_tree: короткого окна между проверкой
    /// и построением коммита нет (#1697).
    async fn commit_tree(
        &self,
        tree: Value,
        message: &str,
        expected: Option<ExpectedBlob<'_>>,
    ) -> UsecaseResult<String> {
        let (_, head) = self
            .call(
                reqwest::Method::GET,
                &format!("/repos/{}/git/ref/heads/{}", self.repo, self.branch),
                None,
            )
            .await?;
        let head_sha = head["object"]["sha"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if head_sha.is_empty() {
            return Err(Self::err(format!("no ref heads/{}", self.branch)));
        }
        let (_, commit) = self
            .call(
                reqwest::Method::GET,
                &format!("/repos/{}/git/commits/{head_sha}", self.repo),
                None,
            )
            .await?;
        let base_tree = commit["tree"]["sha"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if let Some(expected) = expected {
            let current_tree = self.precheck_tree(&base_tree, expected.path).await?;
            check_expected_blob(&current_tree, &expected)?;
        }
        let (_, tree_resp) = self
            .call(
                reqwest::Method::POST,
                &format!("/repos/{}/git/trees", self.repo),
                Some(json!({ "base_tree": base_tree, "tree": tree })),
            )
            .await?;
        let tree_sha = tree_resp["sha"].as_str().unwrap_or_default().to_string();
        let (_, new_commit) = self
            .call(
                reqwest::Method::POST,
                &format!("/repos/{}/git/commits", self.repo),
                Some(json!({ "message": message, "tree": tree_sha, "parents": [head_sha] })),
            )
            .await?;
        let new_sha = new_commit["sha"].as_str().unwrap_or_default().to_string();
        let (status, resp) = self
            .call(
                reqwest::Method::PATCH,
                &format!("/repos/{}/git/refs/heads/{}", self.repo, self.branch),
                Some(json!({ "sha": new_sha })),
            )
            .await?;
        let fast_forward =
            status.is_success() && resp["object"]["sha"].as_str() == Some(new_sha.as_str());
        if !fast_forward {
            // #1653: 422 сразу после своего коммита бывает и без конкурента.
            // Повтор опасен гонкой — отказ честный, без re-shot.
            return Err(UsecaseError::RaceRef(format!(
                "gitwiki: update_ref not fast-forward (HTTP {status}) — a competing commit"
            )));
        }
        Ok(new_sha)
    }

    /// Дерево коммита, от которого строится удаление. Статус проверяем
    /// сами: gherr классифицирует только 403/429, остальные ответы call()
    /// возвращает как Ok((status, Value)) — 502 с HTML-телом иначе стал бы
    /// «no tree array» вместо честного HTTP-статуса (#1697).
    async fn precheck_tree(&self, tree_sha: &str, path: &str) -> UsecaseResult<Value> {
        let url = format!(
            "/repos/{}/git/trees/{}?recursive=1",
            self.repo,
            encode_path(tree_sha)
        );
        let (status, body) = self
            .call(reqwest::Method::GET, &url, None)
            .await
            .map_err(|e| {
                UsecaseError::PrecheckUnavailable(format!("tree {tree_sha} for {path}: {e}"))
            })?;
        if !status.is_success() {
            return Err(UsecaseError::PrecheckUnavailable(format!(
                "tree {tree_sha} for {path}: HTTP {status}"
            )));
        }
        Ok(body)
    }

    /// дерево коммита — независимый от Contents-кэша свидетель удаления.
    async fn path_absent_at(&self, commit_sha: &str, path: &str) -> PostCheck {
        let result = self
            .call(
                reqwest::Method::GET,
                &format!(
                    "/repos/{}/git/trees/{}?recursive=1",
                    self.repo,
                    encode_path(commit_sha)
                ),
                None,
            )
            .await;
        let (status, tree) = match result {
            Ok(pair) => pair,
            Err(e) => return PostCheck::Unverified(e.to_string()),
        };
        if !status.is_success() || tree["truncated"].as_bool().unwrap_or(false) {
            return PostCheck::Unverified(format!(
                "post-check tree {commit_sha}: HTTP {status} (truncated: {:?})",
                tree["truncated"].as_bool()
            ));
        }
        let still_there = tree["tree"].as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|e| e["path"].as_str() == Some(path) && e["type"].as_str() == Some("blob"))
        });
        if still_there {
            PostCheck::Disagreement(format!("{path} still in tree at {commit_sha}"))
        } else {
            PostCheck::ConfirmedAbsent
        }
    }

    /// Append строки в конец существующего файла (ff-only); возвращает
    /// строк после записи (#1353). Несуществующий таргет — доменный
    /// отказ AppendMissing (#1613: append не создаёт — создание остаётся
    /// только у write-пути; писатель знает факт существования
    /// авторитетно, до записи).
    pub async fn append_line(&self, path: &str, line: &str, message: &str) -> UsecaseResult<usize> {
        let Some(current) = self.read(path).await? else {
            return Err(UsecaseError::AppendMissing(format!(
                "doc does not exist: {path}"
            )));
        };
        let next = append_to(current, line);
        self.commit_file(path, &next, message).await?;
        Ok(next.lines().count())
    }
}

/// Сверка снимка живого чтения с деревом коммита (#1697). Чистая функция:
/// ошибки дерева — PrecheckUnavailable, расхождение версии —
/// RaceRef, уже удалённый файл — DocAbsentAtRef без совета повторить.
fn check_expected_blob(tree: &Value, expected: &ExpectedBlob<'_>) -> UsecaseResult<()> {
    match crate::wiki::witness::blob_sha_for(tree, expected.path) {
        Ok(Some(actual)) if actual == expected.sha => Ok(()),
        Ok(Some(actual)) => Err(UsecaseError::RaceRef(format!(
            "gitwiki: blob sha changed for {} (read {}, at ref {})",
            expected.path, expected.sha, actual
        ))),
        Ok(None) => Err(UsecaseError::DocAbsentAtRef(format!(
            "gitwiki: {} already absent at ref (snapshot {}); no delete was committed, backup of the read version only",
            expected.path, expected.sha
        ))),
        Err(error) => Err(UsecaseError::PrecheckUnavailable(format!(
            "gitwiki: tree for {}: {error}",
            expected.path
        ))),
    }
}

/// Удаление файла в Git Data tree: `sha: null` (официальный контракт
/// https://docs.github.com/en/rest/git/trees#create-a-tree).
fn tree_delete_entry(path: &str) -> Value {
    json!([{ "path": path, "mode": "100644", "type": "blob", "sha": null }])
}

/// Строка Contents-GET: путь percent-кодируется, чтобы `#`/`?` в имени
/// файла не рвали URL (fragment/query-разделители). Чистая функция ради
/// юнит-тестов (ADR-110: HTTP-моки не вводим); инцидент #1353: сырой
/// путь с `#1248` урезался на `#` → 404 → append считал «файла нет» и
/// перезаписывал док одной строкой (d33ba05c, +1/−23).
fn contents_url(repo: &str, path: &str) -> String {
    format!("/repos/{repo}/contents/{}", encode_path(path))
}

/// Append-конкатенация: тело дока сохраняется целиком, строка — в конец
/// (гард «append не режет тело», инцидент #1353). Чистая функция —
/// юнит-фикстура «док с fm и многострочным телом».
fn append_to(mut current: String, line: &str) -> String {
    if !current.ends_with('\n') {
        current.push('\n');
    }
    current.push_str(line);
    current.push('\n');
    current
}

/// Стандартный base64-декодер (алфавит + /, с паддингом).
pub fn base64_std(input: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for ch in input.bytes() {
        if ch == b'=' || ch == b'\n' {
            continue;
        }
        let val = TABLE.iter().position(|&t| t == ch)? as u32;
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// Стандартный base64-энкодер с паддингом (blob API encoding=base64,
/// #1171): 3 байта → 4 символа, хвост добивается '='.
pub fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Красный тест инцидента #1353: `#`/`?` в пути файла — данные, не
    /// fragment/query URL; сырой путь рвёт GET (404 → append-перезапись).
    #[test]
    fn delete_tree_entry_uses_null_sha() {
        let entry = tree_delete_entry("docs/Projects/foo.md");
        assert_eq!(entry[0]["path"], "docs/Projects/foo.md");
        assert!(entry[0]["sha"].is_null(), "sha=null обязателен для сноса");
    }

    #[test]
    fn contents_url_encodes_fragment_and_query_delimiters() {
        let url = contents_url("o/r", "docs/Experience/Опыт (srv, #1248).md");
        assert!(!url.contains('#'), "сырой # рвёт URL fragment'ом: {url}");
        assert!(url.contains("%23"), "кодирование # отсутствует: {url}");
        let url = contents_url("o/r", "docs/Experience/вопрос?с.md");
        assert!(!url.contains('?'), "сырой ? уходит в query: {url}");
        assert!(url.contains("%3F"), "кодирование ? отсутствует: {url}");
    }

    #[test]
    fn contents_url_keeps_plain_ascii_path_unchanged() {
        // аски-путь без спецсимволов — как есть; кириллица кодируется
        // (%D0%96…) и GitHub декодирует её обратно в тот же файл
        assert_eq!(
            contents_url("o/r", "docs/cron/x/journal.md"),
            "/repos/o/r/contents/docs/cron/x/journal.md"
        );
        assert!(contents_url("o/r", "docs/cron/x/Журнал.md").contains("%D0%96"));
    }

    /// Гард «append не режет тело» (#1353): fm и многострочное тело
    /// сохраняются, строка дописывается в конец.
    #[test]
    fn append_keeps_fm_and_body() {
        let doc = "---\ntitle: Опыт\ndate: 2026-11-19\n---\n\n# Опыт\n\n## Форензика\nтело\n";
        let next = append_to(doc.to_string(), "строка B");
        assert!(next.starts_with("---\ntitle: Опыт"), "fm потерян: {next}");
        assert!(next.contains("## Форензика\nтело"), "тело потеряно: {next}");
        assert!(
            next.ends_with("тело\nстрока B\n"),
            "строка не в конце: {next}"
        );
    }

    #[test]
    fn append_joins_unterminated_tail_with_newline() {
        assert_eq!(append_to("без переноса".into(), "B"), "без переноса\nB\n");
    }
}

#[cfg(test)]
#[path = "gitwiki_tests.rs"]
mod precheck_tests;

#[cfg(test)]
mod base64_tests {
    use super::*;

    #[test]
    fn encode_roundtrips_with_std_decoder() {
        // round-trip энкодер ↔ существующий декодер (включая паддинги)
        for sample in [
            &b""[..],
            b"a",
            b"ab",
            b"abc",
            b"abcd",
            &[0u8, 255, 7, 128, 64, 13][..],
        ] {
            assert_eq!(base64_std(&base64_encode(sample)).as_deref(), Some(sample));
        }
        assert_eq!(base64_encode(b"abc"), "YWJj");
        assert_eq!(base64_encode(b"a"), "YQ==");
    }
}
