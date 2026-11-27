//! Живой e2e клиента OllamaEmbeddings (#1687), гвард UWUWU_E2E=1:
//! прямой адрес Ollama на kisa (private relay из tests недостижим), расход —
//! два запроса на прогон. Контракт: батч → 768-dim, query → один 768-вектор.

use crate::search::semantic::{Embedder, OllamaEmbeddings, EMBED_MODEL, EMBED_MODEL_DIMS};

fn endpoint() -> String {
    std::env::var("UWUWU_EMBED_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .expect("UWUWU_E2E=1: задайте адрес Ollama (на kisa — http://127.0.0.1:11434)")
}

#[tokio::test]
async fn live_ollama_client_embeds_docs_and_query() {
    if std::env::var("UWUWU_E2E").as_deref() != Ok("1") {
        return;
    }
    let client = OllamaEmbeddings::new(endpoint(), EMBED_MODEL.to_string());

    let docs = vec![
        (
            "Projects/uwuwu/README".to_string(),
            "Семантика на локальных эмбеддингах".to_string(),
        ),
        (String::new(), "без тайтла".to_string()),
    ];
    let vecs = client.embed_docs(&docs).await.expect("живой док-батч");
    assert_eq!(vecs.len(), 2, "по вектору на каждый вход");
    assert!(
        vecs.iter().all(|v| v.len() == EMBED_MODEL_DIMS),
        "векторы {EMBED_MODEL_DIMS}-dim"
    );

    let qv = client
        .embed_query("лимиты эмбеддингов")
        .await
        .expect("живой query");
    assert_eq!(
        qv.len(),
        EMBED_MODEL_DIMS,
        "query-вектор той же размерности"
    );
}
