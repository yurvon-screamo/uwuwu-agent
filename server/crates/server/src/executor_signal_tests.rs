//! Wiring-тесты сигнала деградации executor'а (#1312): политику
//! (порог/окно/анти-повтор) покрывают юниты pipeline/executor.rs,
//! тик-факт — tick_tests.rs; здесь — связка tick_loop → владелец:
//! Some-notify доставляет поимённый ТГ с лечением, сантропорог не
//! пускает тишину в send, сбой ТГ-отправки не роняет поток, off —
//! сигнала нет (лог-only, safe default).

use std::sync::{Arc, Mutex};

use uwuwu_usecase::bots::{TelegramGateway, TgUpdate};
use uwuwu_usecase::{ExecutorLoudness, UsecaseError, UsecaseResult};

use super::executor_unavailable_signal;

const MIN: u64 = 60_000;

#[derive(Default)]
struct FakeTg {
    sent: Mutex<Vec<(i64, String)>>,
    fail: bool,
}

impl FakeTg {
    fn new(fail: bool) -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            fail,
        }
    }
}

#[async_trait::async_trait]
impl TelegramGateway for FakeTg {
    async fn get_updates(
        &self,
        _offset: Option<u64>,
        _timeout_sec: u64,
    ) -> UsecaseResult<Vec<TgUpdate>> {
        Ok(vec![])
    }

    async fn send_message(&self, chat_id: i64, text: &str) -> UsecaseResult<()> {
        if self.fail {
            return Err(UsecaseError::Telegram("sendMessage down".into()));
        }
        self.sent
            .lock()
            .expect("lock")
            .push((chat_id, text.to_string()));
        Ok(())
    }

    async fn download_file(&self, _file_id: &str) -> UsecaseResult<Vec<u8>> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn threshold_failure_sends_named_alert_with_remedy() {
    let tg = Arc::new(FakeTg::new(false));
    let notify = Some((tg.clone() as Arc<dyn TelegramGateway>, 42));
    let mut incidents = ExecutorLoudness::new(3, 60 * MIN, 60 * MIN);
    incidents.register_failure("redmi", 0);
    incidents.register_failure("redmi", MIN);

    executor_unavailable_signal(&notify, &mut incidents, "redmi", 2 * MIN).await;

    let sent = tg.sent.lock().expect("lock");
    assert_eq!(sent.len(), 1, "порог достигнут — один сигнал");
    let (chat, text) = &sent[0];
    assert_eq!(*chat, 42, "чат владельца из [notify]");
    assert!(text.contains("redmi"), "сигнал называет виновника: {text}");
    assert!(
        text.contains("rathole"),
        "текст несёт лечение (рестарт rathole-клиента): {text}"
    );
}

#[tokio::test]
async fn below_threshold_sends_nothing() {
    let tg = Arc::new(FakeTg::new(false));
    let notify = Some((tg.clone() as Arc<dyn TelegramGateway>, 42));
    let mut incidents = ExecutorLoudness::new(3, 60 * MIN, 60 * MIN);
    incidents.register_failure("redmi", 0);

    executor_unavailable_signal(&notify, &mut incidents, "redmi", MIN).await;

    assert!(
        tg.sent.lock().expect("lock").is_empty(),
        "два отказа из трёх — владелец не пингуется"
    );
}

#[tokio::test]
async fn send_failure_is_contained_and_cooldown_holds() {
    let tg = Arc::new(FakeTg::new(true));
    let notify = Some((tg.clone() as Arc<dyn TelegramGateway>, 42));
    let mut incidents = ExecutorLoudness::new(3, 60 * MIN, 60 * MIN);
    incidents.register_failure("kisa", 0);
    incidents.register_failure("kisa", MIN);

    executor_unavailable_signal(&notify, &mut incidents, "kisa", 2 * MIN).await;
    executor_unavailable_signal(&notify, &mut incidents, "kisa", 3 * MIN).await;

    assert!(
        tg.sent.lock().expect("lock").is_empty(),
        "фейковый сбой отправки не дал записи, паники нет"
    );
}

#[tokio::test]
async fn notify_off_stays_log_only() {
    let mut incidents = ExecutorLoudness::new(3, 60 * MIN, 60 * MIN);
    incidents.register_failure("redmi", 0);
    incidents.register_failure("redmi", MIN);

    executor_unavailable_signal(&None, &mut incidents, "redmi", 2 * MIN).await;

    assert!(
        incidents.register_failure("redmi", 3 * MIN).is_none(),
        "сигнал ущёл в лог, cooldown держит анти-повтор — громкость жива"
    );
}
