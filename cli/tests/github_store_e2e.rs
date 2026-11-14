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

/// Полный жизненный цикл на живом sandbox-репо: create (Pv2-статус + поля)
/// → list видит → resolve по тайтлу → edit (branch в поле) → comment →
/// claim (Сессия dev в поле) → body чист от fm → close (Done закрыл issue).
#[tokio::test]
#[ignore = "live sandbox: UWUWU_E2E_GITHUB=1 cargo test --features e2e-github -- --ignored"]
async fn github_store_full_lifecycle_live() {
    let store = sandbox();
    let stamp = chrono::Utc::now().timestamp_millis();
    let title = format!("[e2e] полный цикл GitHubStore {stamp}");

    // create с ретраем на транзиентных ошибках Projects v2 (лаг платформы)
    let mut created = None;
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        }
        match store
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
        {
            Ok(task) => {
                created = Some(task);
                break;
            }
            Err(e) => {
                let text = format!("{e:#}");
                if !text.contains("Projects v2") {
                    panic!("create: {text}");
                }
            }
        }
    }
    let created = created.expect("create: попытки исчерпаны");
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
    assert!(
        !body.trim_start().starts_with("---\n"),
        "тело без fm-карточки (ADR-098): {body}"
    );
    assert!(!body.contains("dev-session:"), "клетка не в теле: {body}");

    let closed = store
        .close(&board(), &edited, Some("e2e завершён"))
        .await
        .expect("close");
    assert_eq!(closed.status, "Done");

    // verify-after-write (1127): повторный close идемпотентен — гарда
    // (PATCH → пауза → GET-сверка) обязана пройти на уже закрытом issue.
    let reclosed = store
        .close(&board(), &edited, None)
        .await
        .expect("повторный close идемпотентен, verify проходит");
    assert_eq!(reclosed.status, "Done");

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

/// Todo-проба контрактов клеток: вариации priority/autodone/body_md.
fn create_todo(
    title: &str,
    priority: Option<&str>,
    autodone: Option<bool>,
    body_md: Option<&str>,
) -> CreateTask {
    CreateTask {
        title: title.to_string(),
        task_type: TaskType("bug".into()),
        status: Some("Todo".into()),
        body_md: body_md.map(String::from),
        attachments: vec![],
        priority: priority.map(String::from),
        autodone,
    }
}

/// Контракт priority при create (дефолт medium — решение владельца
/// 13.11): create без флага рождает клетку Приоритет=medium; явный флаг
/// перекрывает. Авторский fm в теле (--stdin) — мёртвый носитель: это
/// контент, клетка получает дефолт (ADR-098, задача 1087).
/// Уборка проб — close (канон рига).
#[tokio::test]
#[ignore = "live sandbox: UWUWU_E2E_GITHUB=1 cargo test --features e2e-github -- --ignored"]
async fn github_store_create_priority_contract_live() {
    let store = sandbox();
    let board = board();
    let stamp = chrono::Utc::now().timestamp_millis();

    // Act: три создания — дефолт, явный high, авторский priority в fm тела
    let defaulted = store
        .create(
            &board,
            &create_todo(&format!("[e2e] priority дефолт {stamp}"), None, None, None),
        )
        .await
        .expect("create без флага");
    let explicit = store
        .create(
            &board,
            &create_todo(
                &format!("[e2e] priority high {stamp}"),
                Some("high"),
                None,
                None,
            ),
        )
        .await
        .expect("create с high");
    let authored = store
        .create(
            &board,
            &create_todo(
                &format!("[e2e] priority в fm тела {stamp}"),
                None,
                None,
                Some("---\npriority: low\n---\nтело с авторским fm"),
            ),
        )
        .await
        .expect("create с авторским fm");

    // Assert: клетки через resolve (норм-форма полей)
    let default_task = store
        .resolve(&board, &defaulted.title)
        .await
        .expect("resolve дефолт");
    let explicit_task = store
        .resolve(&board, &explicit.title)
        .await
        .expect("resolve high");
    let authored_task = store
        .resolve(&board, &authored.title)
        .await
        .expect("resolve авторский");
    assert_eq!(
        default_task.priority, "medium",
        "create без флага: дефолт medium"
    );
    assert_eq!(
        explicit_task.priority, "high",
        "явный --priority перекрывает дефолт"
    );
    assert_eq!(
        authored_task.priority, "medium",
        "fm тела — мёртвый носитель (ADR-098): контент, клетка = дефолт"
    );
    let authored_body = store
        .body_get(&board, &authored)
        .await
        .expect("body_get авторский");
    assert!(
        authored_body.contains("priority: low"),
        "авторский fm остаётся контентом тела: {authored_body}"
    );

    // Уборка: пробы не копятся (close — канон рига)
    for task in [&defaulted, &explicit, &authored] {
        let closed = store
            .close(&board, task, Some("e2e priority-контракт"))
            .await
            .expect("close пробы");
        assert_eq!(closed.status, "Done");
    }
}

/// Контракт autodone при create (дефолт true — решение владельца
/// 14.11, задача 1050): create без флага рождает клетку Autodone=on;
/// явный `off` перекрывает. Авторский fm в теле — мёртвый носитель
/// (ADR-098): контент, клетка получает дефолт. Уборка проб — close.
#[tokio::test]
#[ignore = "live sandbox: UWUWU_E2E_GITHUB=1 cargo test --features e2e-github -- --ignored"]
async fn github_store_create_autodone_contract_live() {
    let store = sandbox();
    let board = board();
    let stamp = chrono::Utc::now().timestamp_millis();

    // Act: три создания — дефолт, явный off, авторский autodone в fm тела
    let defaulted = store
        .create(
            &board,
            &create_todo(&format!("[e2e] autodone дефолт {stamp}"), None, None, None),
        )
        .await
        .expect("create без флага");
    let explicit_off = store
        .create(
            &board,
            &create_todo(
                &format!("[e2e] autodone off {stamp}"),
                None,
                Some(false),
                None,
            ),
        )
        .await
        .expect("create с off");
    let authored = store
        .create(
            &board,
            &create_todo(
                &format!("[e2e] autodone в fm тела {stamp}"),
                None,
                None,
                Some("---\nautodone: false\n---\nтело с авторским fm"),
            ),
        )
        .await
        .expect("create с авторским fm");

    // Assert: клетки через resolve (норм-форма полей)
    let default_task = store
        .resolve(&board, &defaulted.title)
        .await
        .expect("resolve дефолт");
    let off_task = store
        .resolve(&board, &explicit_off.title)
        .await
        .expect("resolve off");
    let authored_task = store
        .resolve(&board, &authored.title)
        .await
        .expect("resolve авторский");
    assert!(default_task.autodone, "create без флага: дефолт on");
    assert!(
        !off_task.autodone,
        "явный --autodone off перекрывает дефолт"
    );
    assert!(
        authored_task.autodone,
        "fm тела — мёртвый носитель (ADR-098): контент, клетка = дефолт on"
    );

    // Уборка: пробы не копятся (close — канон рига)
    for task in [&defaulted, &explicit_off, &authored] {
        let closed = store
            .close(&board, task, Some("e2e autodone-контракт"))
            .await
            .expect("close пробы");
        assert_eq!(closed.status, "Done");
    }
}
