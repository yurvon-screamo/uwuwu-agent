//! Живой e2e клиента GeminiEmbeddings (#1363), гвард UWUWU_E2E=1 (культура
//! репо — образец gitea_tests): реальный generativelanguage API, расход —
//! два запроса (док-батч + query) на прогон. Без UWUWU_E2E=1 — сразу зелёный.
//! Контракт: success-батч → векторы 1536; doc/query-префиксы в теле запроса;
//! query-путь возвращает вектор той же размерности.

use crate::search::semantic::{Embedder, GeminiEmbeddings, EMBED_MODEL_DIMS};

#[tokio::test]
async fn live_gemini_client_embeds_docs_and_query() {
    if std::env::var("UWUWU_E2E").as_deref() != Ok("1") {
        return; // живой пробе нужен мандат прогонщика
    }
    let key = std::env::var("GEMINI_API_KEY").expect("UWUWU_E2E=1: нужен GEMINI_API_KEY");
    assert!(!key.trim().is_empty(), "GEMINI_API_KEY пуст");

    let client = GeminiEmbeddings::new(key, "gemini-embedding-2".to_string());

    let docs = vec![
        (
            "Experience/platform/Лимиты Gemini".to_string(),
            "RPD 1000".to_string(),
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
