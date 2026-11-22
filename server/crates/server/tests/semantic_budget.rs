//! Интеграционный пин #1473 (срез А): суточный бюджет эмбеддингов синка —
//! sync останавливается на капе, резерв дневной квоты Gemini остаётся
//! query-эмбеддингам поисков. Носитель счётчика — semantic_budget.json
//! рядом с Lance-том; окно — полночь Pacific.
//!
//! Крейт бинарный, lib-таргета нет: модули search подключаются
//! зеркалом tests/search/mod.rs (паттерн semantic_slug_escape).

mod search;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use search::budget::{
    budget_day_now, budget_path, read_state, write_state, BudgetState, SYNC_DAILY_CAP,
};
use search::semantic::{Embedder, EMBED_MODEL_DIMS};
use search::semantic_backend::LanceSemanticBackend;
use search::support::{lance_dir, FakeWiki, TempDirGuard};
use uwuwu_usecase::{SemanticBackend, UsecaseResult};

/// Счётный мок-эмбеддер: считает вызовы embed_docs (доков) и суммарные
/// тексты (чанков) — cost реиндекса ceil(чанков/38) считается в бекенде.
struct CountingEmbedder {
    doc_calls: AtomicUsize,
    texts_total: AtomicUsize,
}

impl CountingEmbedder {
    fn new() -> Self {
        Self {
            doc_calls: AtomicUsize::new(0),
            texts_total: AtomicUsize::new(0),
        }
    }

    fn doc_calls(&self) -> usize {
        self.doc_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Embedder for CountingEmbedder {
    async fn embed_docs(&self, docs: &[(String, String)]) -> UsecaseResult<Vec<Vec<f32>>> {
        self.doc_calls.fetch_add(1, Ordering::SeqCst);
        self.texts_total.fetch_add(docs.len(), Ordering::SeqCst);
        Ok(docs
            .iter()
            .map(|_| vec![0.0f32; EMBED_MODEL_DIMS])
            .collect())
    }

    async fn embed_query(&self, _query: &str) -> UsecaseResult<Vec<f32>> {
        Ok(vec![0.0f32; EMBED_MODEL_DIMS])
    }
}

const SMALL_MD: &str = "# Док\n\nКомпактный абзац — один чанк, cost = 1.";

/// Тяжёлый док: 50 абзацев по ~1300 символов ≈ 50 чанков → cost = 2
/// (два batchEmbedContents-вызова по 38 текстов).
fn heavy_md() -> String {
    let paragraph = "а".repeat(1300);
    std::iter::repeat_n(paragraph, 50)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn wiki_with_three_docs() -> FakeWiki {
    FakeWiki {
        docs: HashMap::from([
            (
                "Experience/a".to_string(),
                (SMALL_MD.to_string(), "sha-a".into()),
            ),
            (
                "Experience/b".to_string(),
                (SMALL_MD.to_string(), "sha-b".into()),
            ),
            (
                "Experience/heavy".to_string(),
                (heavy_md(), "sha-heavy".into()),
            ),
        ]),
    }
}

async fn open_backend(
    wiki: FakeWiki,
    embedder: Arc<dyn Embedder>,
) -> (LanceSemanticBackend, TempDirGuard) {
    let guard = TempDirGuard(lance_dir("budget"));
    let backend = LanceSemanticBackend::open(guard.0.clone(), Arc::new(wiki), None, embedder)
        .await
        .expect("open lance backend");
    (backend, guard)
}

#[tokio::test]
async fn exhausted_daily_budget_stops_sync_before_any_embedding() {
    let embedder = Arc::new(CountingEmbedder::new());
    let (backend, dir) = open_backend(wiki_with_three_docs(), embedder.clone()).await;

    // Счётчик дня уже на капе — sync не должен тратить ни одного вызова.
    write_state(
        &budget_path(&dir.0),
        &BudgetState {
            day: budget_day_now(),
            spent: SYNC_DAILY_CAP,
        },
    )
    .expect("seed budget");

    let changed = backend.sync_index(19).await.expect("sync ok");

    assert_eq!(changed, 0, "на капе ничего не индексируется");
    assert_eq!(
        embedder.doc_calls(),
        0,
        "исчерпанный бюджет — ни одного embed-вызова"
    );
}

#[tokio::test]
async fn sync_spends_are_accounted_in_budget_file() {
    let embedder = Arc::new(CountingEmbedder::new());
    let (backend, dir) = open_backend(wiki_with_three_docs(), embedder.clone()).await;

    let changed = backend.sync_index(19).await.expect("sync ok");
    assert_eq!(changed, 3, "все три дока проиндексированы");

    let state = read_state(&budget_path(&dir.0));
    assert_eq!(state.day, budget_day_now());
    // 2 малых дока по 1 запросу + тяжёлый (50 чанков) = ceil(50/38) = 2 → 4.
    assert_eq!(
        state.spent, 4,
        "cost = ceil(чанков/EMBED_BATCH), факт: {state:?}"
    );

    // Повторный sync без изменений не тратит бюджет.
    let second = backend.sync_index(19).await.expect("resync ok");
    assert_eq!(second, 0);
    let after = read_state(&budget_path(&dir.0));
    assert_eq!(after.spent, 4, "неизменённый корпус бюджет не жжёт");
    assert_eq!(embedder.doc_calls(), 3);
}

#[tokio::test]
async fn stale_budget_day_resets_the_counter() {
    let embedder = Arc::new(CountingEmbedder::new());
    let (backend, dir) = open_backend(wiki_with_three_docs(), embedder.clone()).await;

    // Вчерашнее окно почти на капе — сегодня счёт с нуля, sync живёт.
    write_state(
        &budget_path(&dir.0),
        &BudgetState {
            day: "2000-01-01".to_string(),
            spent: SYNC_DAILY_CAP - 1,
        },
    )
    .expect("seed stale budget");

    let changed = backend.sync_index(19).await.expect("sync ok");

    assert_eq!(changed, 3, "суточный резет возвращает sync к работе");
    let state = read_state(&budget_path(&dir.0));
    assert_eq!(state.day, budget_day_now());
    assert_eq!(state.spent, 4);
}
