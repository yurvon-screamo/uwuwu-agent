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

/// Канон резолва + write-контракт --base (склейка семьи 1097):
/// закрытая строка резолвится по номеру (1048); числоподобный номер выше
/// substring тайтла чужой строки (1088); снапшот task get (обёртка +
/// хвостовой \n, U+200B в теле) — легитимный --base (978-2/1096/1100);
/// Pv2-отказ edit громкий и не трогает fm — ложного «изменена» нет (1020).
/// Уборка — close (канон рига).
#[tokio::test]
#[ignore = "live sandbox: UWUWU_E2E_GITHUB=1 cargo test --features e2e-github -- --ignored"]
async fn github_store_resolve_and_base_contract_live() {
    let store = sandbox();
    let board = board();
    let stamp = chrono::Utc::now().timestamp_millis();

    // (1) жертва А: создать → закрыть → точный номер становится вне list
    let title_a = format!("[e2e] резолв закрытой по номеру {stamp}");
    let a = store
        .create(
            &board,
            &CreateTask {
                title: title_a.clone(),
                ..create_input(&title_a)
            },
        )
        .await
        .expect("create A");
    store.close(&board, &a, None).await.expect("close A");
    tokio::time::sleep(LIST_LAG).await;

    // (2) приманка Б: open, тайтл содержит номер А как substring
    // (до фикса resolve(номер А) перебивался этим substring — 1088)
    let title_b = format!("[e2e] кейс {} в тайтле-приманке", a.id);
    let b = store
        .create(
            &board,
            &CreateTask {
                title: title_b.clone(),
                status: Some("Todo".into()),
                ..create_input(&title_b)
            },
        )
        .await
        .expect("create B");
    tokio::time::sleep(LIST_LAG).await;

    let got = store
        .resolve(&board, &a.id)
        .await
        .expect("resolve по номеру закрытой строки");
    assert_eq!(
        got.id, a.id,
        "номер закрытой строки перебит substring (1048/1088)"
    );
    assert_eq!(got.status, "Done", "закрытая резолвится (state → Done)");

    // (3) --base: снапшот task get-формы (обёртка + хвостовой \n) матчится
    let b_resolved = store.resolve(&board, &b.id).await.expect("resolve Б");
    let body = store
        .body_get(&board, &b_resolved)
        .await
        .expect("body_get Б");
    let snapshot = format!(
        "Задача:   {title_b}\nСтатус:   Todo\nСтрока:   issue #{}\n\n{body}\n",
        b.id
    );
    let new_body =
        "---\nstatus: Todo\npriority: medium\nautodone: true\n---\nтело с \u{200b}zero-width\n";
    store
        .body_replace(&board, &b_resolved, new_body, &snapshot)
        .await
        .expect("replace со снапшотом task get (обёртка срезается, U+200B не мешает)");

    // прежний снапшот против нового тела — громкий отказ свежести
    let err = store
        .body_replace(&board, &b_resolved, "ещё тело\n", &snapshot)
        .await
        .expect_err("устаревший снапшот должен отказать");
    assert!(format!("{err}").contains("тело изменилось"), "got: {err}");

    // (4) false-success пин (1020): Pv2-отказ — громкий, fm не тронут
    let err = store
        .edit(
            &board,
            &b_resolved,
            &EditTask {
                status: Some("Не-Существует-Опция".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("несуществующая опция Status должна отказать");
    let text = format!("{err:#}");
    assert!(text.contains("опция Status"), "got: {text}");
    let body_after = store
        .body_get(&board, &b_resolved)
        .await
        .expect("body после отказа");
    assert_eq!(
        fm_field(&body_after, "status").as_deref(),
        Some("Todo"),
        "Pv2-отказ не должен трогать fm-зеркало (носители согласованы)"
    );

    // (4b) валидный edit --status: Pv2 + fm-зеркало согласованы (1039 —
    // «cli-вид статуса врёт»): fm в теле обновлён синхронно с Pv2
    store
        .edit(
            &board,
            &b_resolved,
            &EditTask {
                status: Some("In Progress".into()),
                ..Default::default()
            },
        )
        .await
        .expect("edit с валидным статусом");
    let body_moved = store
        .body_get(&board, &b_resolved)
        .await
        .expect("body после edit статуса");
    assert_eq!(
        fm_field(&body_moved, "status").as_deref(),
        Some("In Progress"),
        "fm-зеркало статуса отстаёт от Pv2 (регрессия 1039)"
    );

    // (5) уборка: обе строки закрыты (из open-list уйдут сами)
    store
        .close(&board, &b_resolved, None)
        .await
        .expect("close Б");
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

/// Проба create в канон-форме контракт-тестов рига: Todo-статус, bug-тип,
/// параметризуемые body_md/priority/autodone; паника expect — live-тест.
async fn create_probe(
    store: &uwuwu_cli::store::github_task::GitHubStore,
    title: String,
    body_md: Option<String>,
    priority: Option<String>,
    autodone: Option<bool>,
) -> uwuwu_cli::store::Task {
    store
        .create(
            &board(),
            &CreateTask {
                title,
                task_type: TaskType("bug".into()),
                status: Some("Todo".into()),
                body_md,
                attachments: vec![],
                priority,
                autodone,
            },
        )
        .await
        .expect("create пробы")
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
    let defaulted = create_probe(
        &store,
        format!("[e2e] priority дефолт {stamp}"),
        None,
        None,
        None,
    )
    .await;
    let explicit = create_probe(
        &store,
        format!("[e2e] priority high {stamp}"),
        None,
        Some("high".into()),
        None,
    )
    .await;
    let authored = create_probe(
        &store,
        format!("[e2e] priority в fm тела {stamp}"),
        Some("---\npriority: low\n---\nтело с авторским fm".into()),
        None,
        None,
    )
    .await;

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
    let defaulted = create_probe(
        &store,
        format!("[e2e] autodone дефолт {stamp}"),
        None,
        None,
        None,
    )
    .await;
    let explicit_off = create_probe(
        &store,
        format!("[e2e] autodone off {stamp}"),
        None,
        None,
        Some(false),
    )
    .await;
    let authored = create_probe(
        &store,
        format!("[e2e] autodone в fm тела {stamp}"),
        Some("---\nautodone: false\n---\nтело с авторским fm".into()),
        None,
        None,
    )
    .await;

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
