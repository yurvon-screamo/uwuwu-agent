//! Живой e2e клиента OllamaEmbeddings (#1687), гвард UWUWU_E2E=1:
//! прямой адрес Ollama на kisa (private relay из tests недостижим), расход —
//! два запроса на прогон. Контракт: батч → 768-dim, query → один 768-вектор.

use crate::search::semantic::{embed_model, search_dims, Embedder, OllamaEmbeddings};

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
    let client = OllamaEmbeddings::new(endpoint(), embed_model().to_string());

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
        vecs.iter().all(|v| v.len() == search_dims()),
        "векторы {}-dim",
        search_dims()
    );

    let qv = client
        .embed_query("лимиты эмбеддингов")
        .await
        .expect("живой query");
    assert_eq!(qv.len(), search_dims(), "query-вектор той же размерности");
}
