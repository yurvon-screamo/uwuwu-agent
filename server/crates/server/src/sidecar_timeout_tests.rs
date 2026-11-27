//! Политика таймаута сайдкаров (#1730): общий таймаут снят, решение живёт
//! на каждом сайдкаре. semantic-sync ждёт завершения тика.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::{sidecar_ticks, Sidecar};

fn slow_sidecar(done: Arc<AtomicBool>, timeout: Option<Duration>) -> Sidecar {
    Sidecar {
        name: "slow".into(),
        timeout,
        tick: Box::new(move || {
            let done = done.clone();
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(80)).await;
                done.store(true, Ordering::SeqCst);
                Ok(())
            })
        }),
    }
}

#[tokio::test]
async fn sidecar_without_timeout_waits_for_completion() {
    // Arrange
    let done = Arc::new(AtomicBool::new(false));
    let sidecars = vec![slow_sidecar(done.clone(), None)];

    // Act
    sidecar_ticks(&sidecars)
        .await
        .expect("тик без таймаута завершается");

    // Assert
    assert!(
        done.load(Ordering::SeqCst),
        "тик без таймаута должен дождаться завершения"
    );
}

#[tokio::test]
async fn timed_out_sidecar_does_not_block_the_queue() {
    // Arrange
    let timed_out = Arc::new(AtomicBool::new(false));
    let after = Arc::new(AtomicBool::new(false));
    let after_flag = after.clone();
    let sidecars = vec![
        slow_sidecar(timed_out.clone(), Some(Duration::from_millis(10))),
        Sidecar {
            name: "after".into(),
            timeout: None,
            tick: Box::new(move || {
                let after = after_flag.clone();
                Box::pin(async move {
                    after.store(true, Ordering::SeqCst);
                    Ok(())
                })
            }),
        },
    ];

    // Act
    sidecar_ticks(&sidecars)
        .await
        .expect("таймаут одного тика не роняет очередь");

    // Assert
    assert!(
        !timed_out.load(Ordering::SeqCst),
        "прерванный тик не должен считаться завершённым"
    );
    assert!(
        after.load(Ordering::SeqCst),
        "следующий сайдкар должен выполниться после таймаута"
    );
}
