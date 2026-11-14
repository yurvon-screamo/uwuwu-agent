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

/// fm-поля тела задачи: ручная вычитка frontmatter-блока (тестовый
/// crate не видит pub(crate)-парсер стора, regex запрещён).
fn fm_field(body: &str, key: &str) -> Option<String> {
    let after_open = body.strip_prefix("---\n")?;
    let end = after_open.find("\n---\n")?;
    for line in after_open[..end].lines() {
        if let Some((k, v)) = line.split_once(": ") {
            if k.trim() == key {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// Контракт priority при create (дефолт medium — решение владельца
/// 13.11): create без флага рождает fm `priority: medium`; явный флаг
/// перекрывает; авторский `priority` во fm тела (--stdin) перекрыт не
/// бывает. Уборка проб — close (канон рига).
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
            &CreateTask {
                title: format!("[e2e] priority дефолт {stamp}"),
                task_type: TaskType("bug".into()),
                status: Some("Todo".into()),
                body_md: None,
                attachments: vec![],
                priority: None,
                autodone: None,
            },
        )
        .await
        .expect("create без флага");
    let explicit = store
        .create(
            &board,
            &CreateTask {
                title: format!("[e2e] priority high {stamp}"),
                task_type: TaskType("bug".into()),
                status: Some("Todo".into()),
                body_md: None,
                attachments: vec![],
                priority: Some("high".into()),
                autodone: None,
            },
        )
        .await
        .expect("create с high");
    let authored = store
        .create(
            &board,
            &CreateTask {
                title: format!("[e2e] priority в fm тела {stamp}"),
                task_type: TaskType("bug".into()),
                status: Some("Todo".into()),
                body_md: Some("---\npriority: low\n---\nтело с авторским fm".into()),
                attachments: vec![],
                priority: None,
                autodone: None,
            },
        )
        .await
        .expect("create с авторским fm");

    // Assert: fm каждого из трёх
    let default_body = store
        .body_get(&board, &defaulted)
        .await
        .expect("body_get дефолт");
    let explicit_body = store
        .body_get(&board, &explicit)
        .await
        .expect("body_get high");
    let authored_body = store
        .body_get(&board, &authored)
        .await
        .expect("body_get авторский");
    assert_eq!(
        fm_field(&default_body, "priority").as_deref(),
        Some("medium"),
        "create без флага: fm обязан нести priority: medium"
    );
    assert_eq!(
        fm_field(&explicit_body, "priority").as_deref(),
        Some("high"),
        "явный --priority перекрывает дефолт"
    );
    assert_eq!(
        fm_field(&authored_body, "priority").as_deref(),
        Some("low"),
        "авторский priority во fm тела перекрыт дефолтом не бывает"
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
/// 14.11, задача 1050): create без флага рождает fm `autodone: true`;
/// явный `off` перекрывает; авторский `autodone` во fm тела (--stdin)
/// перекрыт дефолтом не бывает. Уборка проб — close (канон рига).
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
            &CreateTask {
                title: format!("[e2e] autodone дефолт {stamp}"),
                task_type: TaskType("bug".into()),
                status: Some("Todo".into()),
                body_md: None,
                attachments: vec![],
                priority: None,
                autodone: None,
            },
        )
        .await
        .expect("create без флага");
    let explicit_off = store
        .create(
            &board,
            &CreateTask {
                title: format!("[e2e] autodone off {stamp}"),
                task_type: TaskType("bug".into()),
                status: Some("Todo".into()),
                body_md: None,
                attachments: vec![],
                priority: None,
                autodone: Some(false),
            },
        )
        .await
        .expect("create с off");
    let authored = store
        .create(
            &board,
            &CreateTask {
                title: format!("[e2e] autodone в fm тела {stamp}"),
                task_type: TaskType("bug".into()),
                status: Some("Todo".into()),
                body_md: Some("---\nautodone: false\n---\nтело с авторским fm".into()),
                attachments: vec![],
                priority: None,
                autodone: None,
            },
        )
        .await
        .expect("create с авторским fm");

    // Assert: fm каждого из трёх
    let default_body = store
        .body_get(&board, &defaulted)
        .await
        .expect("body_get дефолт");
    let off_body = store
        .body_get(&board, &explicit_off)
        .await
        .expect("body_get off");
    let authored_body = store
        .body_get(&board, &authored)
        .await
        .expect("body_get авторский");
    assert_eq!(
        fm_field(&default_body, "autodone").as_deref(),
        Some("true"),
        "create без флага: fm обязан нести autodone: true"
    );
    assert_eq!(
        fm_field(&off_body, "autodone").as_deref(),
        Some("false"),
        "явный --autodone off перекрывает дефолт"
    );
    assert_eq!(
        fm_field(&authored_body, "autodone").as_deref(),
        Some("false"),
        "авторский autodone во fm тела перекрыт дефолтом не бывает"
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

/// Reply-маркер (задача 61): живой write→read круг на границе GitHub —
/// comment(Some(метка)) ложит цитатный маркер в тело, thread распознаёт
/// его как reply_to, верхнеуровневый остаётся None (структура ответов
/// видна в треде). Уборка — close (канон рига).
#[tokio::test]
#[ignore = "live sandbox: UWUWU_E2E_GITHUB=1 cargo test --features e2e-github -- --ignored"]
async fn github_store_reply_marker_thread_recognition_live() {
    let store = sandbox();
    let board = board();
    let stamp = chrono::Utc::now().timestamp_millis();

    let task = store
        .create(
            &board,
            &CreateTask {
                title: format!("[e2e] reply-маркер в thread {stamp}"),
                task_type: TaskType("bug".into()),
                status: Some("Todo".into()),
                body_md: Some("живой e2e: reply-маркер задача 61".into()),
                attachments: vec![],
                priority: None,
                autodone: None,
            },
        )
        .await
        .expect("create пробы");

    store
        .comment(&board, &task, "верхнеуровневый коммент e2e", None)
        .await
        .expect("comment верхний");
    store
        .comment(&board, &task, "ответ по e2e", Some("метка-ответа"))
        .await
        .expect("comment reply");

    let entries = store.thread(&board, &task).await.expect("thread");
    let top = entries
        .iter()
        .find(|e| e.text.contains("верхнеуровневый коммент e2e"))
        .expect("верхний коммент в thread");
    assert!(top.reply_to.is_none(), "верхний без reply_to: {top:?}");
    let reply = entries
        .iter()
        .find(|e| e.text.contains("ответ по e2e"))
        .expect("reply-коммент в thread");
    assert_eq!(
        reply.reply_to.as_deref(),
        Some("метка-ответа"),
        "маркер распознан как reply_to: {reply:?}"
    );

    let closed = store
        .close(&board, &task, Some("e2e reply-маркер"))
        .await
        .expect("close пробы");
    assert_eq!(closed.status, "Done");
}
