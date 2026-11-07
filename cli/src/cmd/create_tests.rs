//! BDD-сценарий `task create --dry-run` (осмотр скелета тела без записи в
//! канбан): мок-ядро со счётчиком всех запросов — ноль запросов и есть
//! доказательство «в канбане НЕ создаётся ничего».

use crate::cmd::cli::TaskSub;
use crate::cmd::sy_dispatch::task_run;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn create(dry_run: bool) -> TaskSub {
    TaskSub::Create {
        title: "проба-скелета".into(),
        r#type: Some("bug".into()),
        stdin: false,
        attach: Vec::<PathBuf>::new(),
        dry_run,
    }
}

/// Ядро-счётчик: любой запрос инкрементирует (эндпоинты не смонтированы —
/// dry-run обязан до них не дойти).
async fn counting_server(kernel_calls: &Arc<AtomicUsize>) -> MockServer {
    let server = MockServer::start().await;
    let counter = Arc::clone(kernel_calls);
    Mock::given(any())
        .respond_with(move |_: &Request| {
            counter.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
        })
        .mount(&server)
        .await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    server
}

/// dry-run: Ok без --project и без живого ядра, ноль запросов к ядру.
#[tokio::test]
#[serial_test::serial]
async fn dry_run_prints_skeleton_without_touching_kernel() {
    let kernel_calls = Arc::new(AtomicUsize::new(0));
    let _server = counting_server(&kernel_calls).await;

    let result = task_run("", create(true)).await;

    assert!(
        result.is_ok(),
        "dry-run проходит: без --project и без живого ядра"
    );
    assert_eq!(
        kernel_calls.load(Ordering::SeqCst),
        0,
        "ноль запросов к ядру"
    );
}

/// Живой create без --project отказывает до обращения к ядру: dry-run ветка
/// стоит раньше project-проверки, а не вместо неё (пин позиции из ревью плана).
#[tokio::test]
#[serial_test::serial]
async fn live_create_still_requires_project_before_kernel() {
    let kernel_calls = Arc::new(AtomicUsize::new(0));
    let _server = counting_server(&kernel_calls).await;

    let err = task_run("", create(false))
        .await
        .expect_err("живой create без --project отказывает");
    assert!(err.to_string().contains("--project"), "суть отказа: {err}");
    assert_eq!(
        kernel_calls.load(Ordering::SeqCst),
        0,
        "до отказа ядро не зовётся"
    );
}
