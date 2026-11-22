//! Живые пробы #1473 (паттерн #1385): полный контур семантики против
//! реального GitHub-хаба и реального Gemini Embedding — вручную
//! (`cargo test --test semantic_live_probe -- --ignored --nocapture`),
//! в CI не входят (квота Gemini и сеть). Хаб — read-only (tree + contents),
//! расход квоты Gemini — единицы запросов (пара доков + query-эмбеддинги).
//!
//! Том пробы сидирования стенда: env UWUWU_LIVE_LANCE_DIR (обязателен) —
//! реальные тексты вики + фиктивные векторы: полнотекст живой (текст
//! реальный, FTS не зависит от эмбеддера), семантика на таком томе честно
//! 429-отказывает — ручной MCP-прогон observe-сервера в окно исчерпания
//! квоты (главный сценарий BDD).

mod search;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use search::semantic::{GeminiEmbeddings, EMBED_MODEL};
use search::semantic_backend::LanceSemanticBackend;
use search::support::{lance_dir, FixedEmbedder, TempDirGuard};
use serde_json::Value;
use uwuwu_usecase::{AppendDocOutcome, SearchUseCase, SemanticBackend, UsecaseResult, WikiRepo};

/// Слаги живой вики для проб: латиница («Railway», «volume») и кириллица
/// («эмбеддинги», «преемпшен») в тексте — оба носителя покрываются FTS.
const SLUGS: &[&str] = &["Projects/uwuwu/Design Spec", "Projects/uwuwu/Деплоймент"];

const HUB: &str = "yurvon-screamo/uwuwu-hub";

/// Read-only WikiRepo поверх GitHub REST, ограниченный слагами проб:
/// реальное дерево и реальное содержимое, скоуп — управляемый мини-индекс
/// (прод-синк делает то же бюджет-капом). Зеркало GitHubWiki не нужно:
/// контракту проб нужны два GET, а не прод-клиент с gherr/gitwiki-связями.
struct LiveWiki {
    token: String,
    http: reqwest::Client,
    /// (slug, git blob sha) из живого дерева — только слаги проб.
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
        let shas: Vec<(String, String)> = body["tree"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|e| {
                        let path = e["path"].as_str()?;
                        let sha = e["sha"].as_str()?;
                        let ok = e["type"].as_str()? == "blob" && path.ends_with(".md");
                        ok.then(|| {
                            (
                                path.trim_start_matches("docs/")
                                    .trim_end_matches(".md")
                                    .to_string(),
                                sha.to_string(),
                            )
                        })
                    })
                    .filter(|(slug, _)| SLUGS.contains(&slug.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        if shas.len() != SLUGS.len() {
            return Err(format!(
                "дерево хаба не содержит слагов проб: нашлось {shas:?}, ждали {SLUGS:?}"
            ));
        }
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
        if !SLUGS.contains(&slug) {
            return Ok(None);
        }
        let body = self
            .get(&format!(
                "/repos/{HUB}/contents/docs/{}",
                encode_path(&format!("{slug}.md"))
            ))
            .await
            .map_err(uwuwu_usecase::UsecaseError::Board)?;
        let b64 = body["content"]
            .as_str()
            .unwrap_or_default()
            .replace('\n', "");
        let bytes = B64
            .decode(b64)
            .map_err(|e| uwuwu_usecase::UsecaseError::Board(format!("{slug}: base64 {e}")))?;
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
        Err(uwuwu_usecase::UsecaseError::Board(
            "live probe wiki is read-only".into(),
        ))
    }

    async fn append_doc(
        &self,
        _slug: &str,
        _line: &str,
        _message: &str,
    ) -> UsecaseResult<AppendDocOutcome> {
        Err(uwuwu_usecase::UsecaseError::Board(
            "live probe wiki is read-only".into(),
        ))
    }
}

fn temp_volume() -> (PathBuf, TempDirGuard) {
    let guard = TempDirGuard(lance_dir("live-probe"));
    (guard.0.clone(), guard)
}

fn embedder_from_env() -> GeminiEmbeddings {
    let key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY (живая проба)");
    GeminiEmbeddings::new(key, EMBED_MODEL.to_string())
}

/// JSON-логи как в проде: событие semantic.daily_budget_exhausted видно
/// в выводе пробы (--nocapture) — артефакт ручного прогона.
fn init_json_logs() {
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::new("info"))
        .with_target(false)
        .try_init();
}

/// Живой главный путь «живая квота» (п.1 «Проверки»): sync реальной вики
/// (реальный GitHub + реальный Gemini) строит индекс, полнотекст находит
/// латиницу и кириллицу, семантика отвечает на реальный query-эмбеддинг —
/// обе секции поиска с попаданиями. Гонять в окне живой квоты (при 429
/// честно падает с её текстом — внешнее условие, не дефект ветки).
#[tokio::test]
#[ignore] // живой прогон: GH_TOKEN + GEMINI_API_KEY + сеть (read-only)
async fn live_index_and_search_both_sections_hit() {
    let token = std::env::var("GH_TOKEN").expect("GH_TOKEN");
    let wiki = Arc::new(LiveWiki::open(token).await.expect("живая вики-фикстура"));
    let (dir, _guard) = temp_volume();
    eprintln!("том пробы: {}", dir.display());

    let backend = LanceSemanticBackend::open(
        dir,
        wiki,
        None, // задачи в пробу не берём — вики-секции достаточны
        Arc::new(embedder_from_env()),
    )
    .await
    .expect("open lance backend");

    let changed = backend.sync_index(10).await.expect("живой sync");
    eprintln!("sync проиндексировал доков: {changed}");
    assert!(
        changed >= 1,
        "живой sync должен был переиндексировать хоть один док"
    );

    // Полнотекст: однотокенная латиница (чистая индукция «Railway» из
    // деградации) — попадание из живого FTS-индекса.
    let latin = backend
        .fulltext(&["railway".to_string()], 5)
        .await
        .expect("fts latin");
    eprintln!("полнотекст «railway»: {latin:?}");
    assert!(!latin.is_empty(), "латиница обязана находиться в живом FTS");

    // Полнотекст: кириллица.
    let cyr = backend
        .fulltext(&["эмбеддинги".to_string()], 5)
        .await
        .expect("fts cyr");
    eprintln!("полнотекст «эмбеддинги»: {cyr:?}");
    assert!(!cyr.is_empty(), "кириллица обязана находиться в живом FTS");

    // Семантика: реальный query-эмбеддинг + ANN по живому индексу.
    let vec_hits = backend
        .search("деплой uwuwu-server railway volume", 5)
        .await
        .expect("vector search");
    eprintln!("семантика: {vec_hits:?}");
    assert!(
        !vec_hits.is_empty(),
        "семантика обязана отвечать на живой query"
    );

    // Usecase-сборка (тот же путь, что рендерит MCP-тул search): обе
    // секции с попаданиями в одном ответе.
    let uc = SearchUseCase {
        semantic: Some(Arc::new(backend) as Arc<dyn SemanticBackend>),
    };
    let hits = uc
        .search("Railway volume", 5)
        .await
        .expect("usecase search");
    let sections: Vec<&str> = hits.iter().map(|h| h.section).collect();
    eprintln!("секции: {sections:?}; хиты: {hits:?}");
    assert!(
        sections.contains(&"полнотекст") && sections.contains(&"семантика"),
        "обе секции должны быть с попаданиями, факт: {hits:?}"
    );
}

/// Сидирование стенда: env UWUWU_LIVE_LANCE_DIR (обязателен) — реальные
/// тексты вики + фиктивные векторы. Полнотекст по такому тому живой
/// (текст реальный, FTS от эмбеддера не зависит — попутный пин: смена
/// эмбеддера не чинит/не ломает полнотекст); query-семантика на живом
/// Gemini даст текущий исход (в окно исчерпания — 429-отказ секции).
#[tokio::test]
#[ignore] // живой прогон: GH_TOKEN + сеть (read-only, без Gemini)
async fn live_stand_volume_seeds_fulltext_from_real_wiki() {
    let dir = std::env::var("UWUWU_LIVE_LANCE_DIR")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .expect("UWUWU_LIVE_LANCE_DIR (том стенда)");
    let token = std::env::var("GH_TOKEN").expect("GH_TOKEN");
    let wiki = Arc::new(LiveWiki::open(token).await.expect("живая вики-фикстура"));

    let backend = LanceSemanticBackend::open(dir, wiki, None, Arc::new(FixedEmbedder))
        .await
        .expect("open lance backend");

    let changed = backend.sync_index(10).await.expect("сид-sync");
    eprintln!("сидирование стенда: доков {changed}");
    assert!(changed >= 1, "сид должен был проиндексировать слаги проб");

    let latin = backend
        .fulltext(&["railway".to_string()], 5)
        .await
        .expect("fts");
    eprintln!("полнотекст «railway» на сиде: {latin:?}");
    assert!(!latin.is_empty(), "полнотекст сида жив (текст реальный)");
}

/// Живой budget-stop (п.2 «Проверки»): реальный проход дерева хаба
/// (doc_shas — живой HTTP) + реальный Lance-том, счётчик дня на капе —
/// sync останавливается до первого embed-вызова (паника-эмбеддер громче
/// счётчика), в логе semantic.daily_budget_exhausted.
#[tokio::test]
#[ignore] // живой прогон: GH_TOKEN + сеть (read-only, без Gemini)
async fn live_daily_budget_exhausted_stops_sync_before_embedding() {
    use search::budget::{budget_day_now, budget_path, write_state, BudgetState, SYNC_DAILY_CAP};
    use search::semantic::Embedder;

    init_json_logs();

    struct Counting;
    #[async_trait]
    impl Embedder for Counting {
        async fn embed_docs(&self, _docs: &[(String, String)]) -> UsecaseResult<Vec<Vec<f32>>> {
            panic!("исчерпанный бюджет не должен звать embed_docs");
        }
        async fn embed_query(&self, _query: &str) -> UsecaseResult<Vec<f32>> {
            panic!("исчерпанный бюджет не должен звать embed_query");
        }
    }

    let token = std::env::var("GH_TOKEN").expect("GH_TOKEN");
    let wiki = Arc::new(LiveWiki::open(token).await.expect("живая вики-фикстура"));
    let (dir, _guard) = temp_volume();

    let backend = LanceSemanticBackend::open(dir.clone(), wiki, None, Arc::new(Counting))
        .await
        .expect("open lance backend");

    // Счётчик дня на капе (том создан open'ом — файлу есть куда писаться).
    write_state(
        &budget_path(&dir),
        &BudgetState {
            day: budget_day_now(),
            spent: SYNC_DAILY_CAP,
        },
    )
    .expect("seed budget");

    let changed = backend.sync_index(10).await.expect("sync ok");
    assert_eq!(changed, 0, "на капе живой sync ничего не индексирует");
    eprintln!("budget-stop: 0 доков, embed-вызовов не было (паника-эмбеддер молчит)");
}
