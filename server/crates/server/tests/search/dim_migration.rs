//! Интеграционный пин #1363: dim-guard миграции индекса. Живой Lance-том
//! с 1536-dim векторами (Gemini-эпоха) при open с клиентом 768 сносится
//! целиком (chunks + doc_meta + sha-персист), штатный sha-инкрементный sync
//! перестраивает индекс из первоисточника — без ручных операций с volume.
//! Повторный open со свежей таблицей — no-op (идемпотентность миграции).
//!
//! Крейт бинарный, lib-таргета нет: модули search подключаются
//! зеркалом tests/search/mod.rs (паттерн mock_route_coverage).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::search::semantic::{search_dims, Embedder};
use crate::search::semantic_backend::LanceSemanticBackend;
use lancedb::arrow::arrow_array::{
    FixedSizeListArray, Float32Array, Int32Array, Int64Array, LargeStringArray, RecordBatch,
};
use lancedb::arrow::arrow_schema::{DataType, Field as ArrowField, Schema as ArrowSchema};
use lancedb::connection::ConnectBuilder;
use uwuwu_usecase::{SemanticBackend, WikiRepo};

const LEGACY_DIM: i32 = 1536; // gemini-embedding-2 — прежний маршрут (#1687)
const SLUG: &str = "Experience/platform/Лимиты Gemini Embedding";
const DOC_MD: &str =
    "# Лимиты\n\nFree tier: RPD 1000, ресет — полночь Pacific. 429 несёт RetryInfo.";

// ——— фикстуры: общий бойлерплейк в super::support (паттерн semantic_slug_escape) ———

use super::support::{lance_dir, FakeWiki, FixedEmbedder, TempDirGuard};

fn legacy_schema(dim: i32) -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(vec![
        ArrowField::new("slug", DataType::LargeUtf8, false),
        ArrowField::new("idx", DataType::Int32, false),
        ArrowField::new("text", DataType::LargeUtf8, false),
        ArrowField::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(ArrowField::new("item", DataType::Float32, true)),
                dim,
            ),
            false,
        ),
    ]))
}

/// Физически сооружает volume «Gemini-эпохи»: chunks 1536-dim с одной строкой,
/// doc_meta и sha-персист — то, что остаётся на volume после отката
/// revert'ом к прежнему маршруту (сценарий герхина «миграция прозрачна»).
async fn seed_legacy_volume(path: &Path) {
    let db = ConnectBuilder::new(path.to_str().expect("utf8 path"))
        .execute()
        .await
        .expect("connect");
    db.create_empty_table("chunks", legacy_schema(LEGACY_DIM))
        .execute()
        .await
        .expect("create legacy chunks");
    let values = Float32Array::from(vec![0.5f32; LEGACY_DIM as usize]);
    let vector = FixedSizeListArray::new(
        Arc::new(ArrowField::new("item", DataType::Float32, true)),
        LEGACY_DIM,
        Arc::new(values),
        None,
    );
    let batch = RecordBatch::try_new(
        legacy_schema(LEGACY_DIM),
        vec![
            Arc::new(LargeStringArray::from(vec![SLUG.to_string()])),
            Arc::new(Int32Array::from(vec![0])),
            Arc::new(LargeStringArray::from(vec!["старый чанк".to_string()])),
            Arc::new(vector),
        ],
    )
    .expect("legacy batch");
    db.open_table("chunks")
        .execute()
        .await
        .expect("open legacy chunks")
        .add(batch)
        .execute()
        .await
        .expect("add legacy chunk");
    let meta = Arc::new(ArrowSchema::new(vec![
        ArrowField::new("slug", DataType::LargeUtf8, false),
        ArrowField::new("doc_hash", DataType::Int64, false),
    ]));
    db.create_empty_table("doc_meta", meta.clone())
        .execute()
        .await
        .expect("create legacy doc_meta");
    let meta_batch = RecordBatch::try_new(
        meta,
        vec![
            Arc::new(LargeStringArray::from(vec![SLUG.to_string()])),
            Arc::new(Int64Array::from(vec![42])),
        ],
    )
    .expect("legacy meta batch");
    db.open_table("doc_meta")
        .execute()
        .await
        .expect("open legacy doc_meta")
        .add(meta_batch)
        .execute()
        .await
        .expect("add legacy meta");
    std::fs::write(
        path.join("semantic_shas.json"),
        format!(r#"{{"{SLUG}": "sha-gemini"}}"#),
    )
    .expect("write legacy shas");
}

async fn vector_dim_of(path: &Path) -> Option<usize> {
    let db = ConnectBuilder::new(path.to_str().expect("utf8 path"))
        .execute()
        .await
        .expect("connect");
    let names = db.table_names().execute().await.expect("table names");
    assert!(names.iter().any(|n| n == "chunks"), "chunks table exists");
    let table = db.open_table("chunks").execute().await.expect("open");
    let schema = table.schema().await.expect("schema");
    schema
        .field_with_name("vector")
        .ok()
        .and_then(|f| match f.data_type() {
            DataType::FixedSizeList(_, size) => Some(*size as usize),
            _ => None,
        })
}

fn wiki_with_doc() -> Arc<dyn WikiRepo> {
    Arc::new(FakeWiki {
        docs: HashMap::from([(SLUG.to_string(), (DOC_MD.to_string(), "sha-1363".into()))]),
    })
}

// ——— тесты ———

#[tokio::test]
async fn stale_1536_volume_is_dropped_and_rebuilt_by_sync() {
    let guard = TempDirGuard(lance_dir("migrate"));
    let dir = guard.0.clone();
    seed_legacy_volume(&dir).await;

    let embeddings: Arc<dyn Embedder> = Arc::new(FixedEmbedder);
    let backend = LanceSemanticBackend::open(dir.clone(), wiki_with_doc(), None, embeddings)
        .await
        .expect("open с 768-клиентом");

    // sha-персист снесён вместе с таблицами — двойной инкремент невозможен
    assert!(
        !dir.join("semantic_shas.json").exists(),
        "sha-файл обязан быть снесён dim-guard'ом"
    );

    let indexed = backend.sync_index(10).await.expect("sync перестраивает");
    assert_eq!(indexed, 1, "док переиндексирован штатным sync");
    assert_eq!(
        vector_dim_of(&dir).await,
        Some(search_dims()),
        "таблица пересоздана под размерность активного клиента"
    );
    drop(backend);

    // повторный sync — no-op: sha-инкремент работает на свежем персисте
    let embeddings: Arc<dyn Embedder> = Arc::new(FixedEmbedder);
    let backend2 = LanceSemanticBackend::open(dir.clone(), wiki_with_doc(), None, embeddings)
        .await
        .expect("повторный open");
    let resync = backend2.sync_index(10).await.expect("resync");
    assert_eq!(resync, 0, "свежий volume не переиндексируется повторно");
    assert!(
        dir.join("semantic_shas.json").exists(),
        "новый sha-персист жив"
    );
}

#[tokio::test]
async fn fresh_768_volume_survives_reopen() {
    let guard = TempDirGuard(lance_dir("fresh"));
    let dir = guard.0.clone();
    let embeddings: Arc<dyn Embedder> = Arc::new(FixedEmbedder);
    {
        let backend = LanceSemanticBackend::open(dir.clone(), wiki_with_doc(), None, embeddings)
            .await
            .expect("open");
        backend.sync_index(10).await.expect("sync");
    }

    // контроль гард-ветки: свой dims → сноса нет, данные переживают reopen
    let embeddings: Arc<dyn Embedder> = Arc::new(FixedEmbedder);
    let backend = LanceSemanticBackend::open(dir.clone(), wiki_with_doc(), None, embeddings)
        .await
        .expect("reopen");
    assert!(
        dir.join("semantic_shas.json").exists(),
        "гард no-op: sha-персист не тронут"
    );
    let hits = backend.search("лимиты", 5).await.expect("search");
    assert!(
        hits.iter().any(|(_, slug)| slug == SLUG),
        "док пережил reopen"
    );
}
