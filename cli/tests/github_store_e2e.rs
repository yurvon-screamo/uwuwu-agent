//! Живой e2e GitHubStore: полный цикл против sandbox-репо uwuwu-hub-sandbox
//! (create → list → resolve → edit → comment → claim → close + дубль-гвардия).
//! Гейт env UWUWU_E2E_GITHUB=1 (канон e2e-ригов: только реальный прогон —
//! моки юзер-фичи не засчитывают); otherwise filtered out.
//!
//! Тайминги: list GitHub имеет лаг ~3–4s после POST (тайминговая проба
//! 13.11: 2s — пусто, 4s — видно) — между create и list пауза 8s с запасом;
//! прод-путь list'ит на независимых тиках, где лаг не чувствуется.

use uwuwu_cli::store::task_store::{CreateTask, EditTask, TaskStore};
use uwuwu_cli::store::{TaskBoard, TaskType};

const LIST_LAG: std::time::Duration = std::time::Duration::from_secs(8);

fn sandbox() -> uwuwu_cli::store::github_task::GitHubStore {
    uwuwu_cli::store::github_task::GitHubStore::new(
        std::env::var("UWUWU_E2E_GITHUB_REPO")
            .unwrap_or_else(|_| "yurvon-screamo/uwuwu-hub-sandbox".into()),
    )
}

fn board() -> TaskBoard {
    TaskBoard {
        project: "sandbox".into(),
    }
}

fn create_input(title: &str) -> CreateTask {
    CreateTask {
        title: title.to_string(),
        task_type: TaskType("bug".into()),
        status: None,
        body_md: None,
        attachments: vec![],
        priority: None,
        autodone: None,
    }
}

/// Полный жизненный цикл на живом sandbox-репо: create (frontmatter status)
/// → list видит → resolve по тайтлу → edit (branch+type) → comment →
/// claim (dev-session) → close (Done закрыл issue).
#[tokio::test]
#[ignore = "live sandbox: UWUWU_E2E_GITHUB=1 cargo test --features e2e-github -- --ignored"]
async fn github_store_full_lifecycle_live() {
    let store = sandbox();
    let stamp = chrono::Utc::now().timestamp_millis();
    let title = format!("[e2e] полный цикл GitHubStore {stamp}");

    let created = store
        .create(
            &board(),
            &CreateTask {
                title: title.clone(),
                task_type: TaskType("bug".into()),
                status: Some("Todo".into()),
                body_md: Some("живой e2e: полный цикл\n\nсекция проверки".into()),
                attachments: vec![],
                priority: None,
                autodone: None,
            },
        )
        .await
        .expect("create");
    assert_eq!(created.status, "Todo");

    tokio::time::sleep(LIST_LAG).await;
    let listed = store.list(&board()).await.expect("list");
    assert!(
        listed.iter().any(|t| t.id == created.id),
        "list не видит созданную ({} строк)",
        listed.len()
    );

    let resolved = store
        .resolve(&board(), &title)
        .await
        .expect("resolve по тайтлу");
    assert_eq!(resolved.id, created.id);

    let edited = store
        .edit(
            &board(),
            &resolved,
            &EditTask {
                branch: Some("work/e2e".into()),
                task_type: Some(TaskType("feature".into())),
                ..Default::default()
            },
        )
        .await
        .expect("edit");
    assert_eq!(edited.branch, "work/e2e");

    store
        .comment(&board(), &edited, "живой коммент e2e", None)
        .await
        .expect("comment");
    let claimed = store
        .claim(&board(), &edited, "ses_e2e_probe")
        .await
        .expect("claim");
    assert_eq!(claimed.dev_session, "ses_e2e_probe");

    let body = store.body_get(&board(), &edited).await.expect("body_get");
    assert!(body.contains("живой e2e: полный цикл"));
    assert!(body.contains("dev-session: ses_e2e_probe"));

    let closed = store
        .close(&board(), &edited, Some("e2e завершён"))
        .await
        .expect("close");
    assert_eq!(closed.status, "Done");

    // Чистка тест-артефакта: закрыт — из list (open) сам уйдёт.
    tokio::time::sleep(LIST_LAG).await;
    let after = store.list(&board()).await.expect("list after close");
    assert!(
        !after.iter().any(|t| t.id == closed.id),
        "Done-строка не должна быть в open-list"
    );
}

/// Идемпотентность create: дубль-тайтл — громкий отказ (list-first с
/// паузой ≥ лага платформы — ретрай после таймаута видит созданное).
#[tokio::test]
#[ignore = "live sandbox: UWUWU_E2E_GITHUB=1 cargo test --features e2e-github -- --ignored"]
async fn github_store_create_duplicate_is_loud() {
    let store = sandbox();
    let stamp = chrono::Utc::now().timestamp_millis();
    let title = format!("[e2e] дубль-тайтл громко {stamp}");
    store
        .create(&board(), &create_input(&title))
        .await
        .expect("первый create");
    tokio::time::sleep(LIST_LAG).await;
    let err = store
        .create(&board(), &create_input(&title))
        .await
        .expect_err("дубль должен отказать");
    assert!(format!("{err}").contains("дубль"));
}
