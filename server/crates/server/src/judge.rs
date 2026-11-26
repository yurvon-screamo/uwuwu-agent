//! Адаптер судьи (#1364): Gemini Decision 1 (`gemini-decision-1`, Google
//! Decisions API free tier) за портом `DecisionJudge`. Контракт ответа —
//! answers-map (choice+probabilities+confidence, noul), сверен живой
//! пробой 20.11 и таблицей #1362. 429 — RetryInfo из тела (парсер переехал
//! сюда из клиента эмбеддингов вместе с константами #1687), полоса вызовов —
//! локальный скользящий лимит (квоты волны общие, TPM-пик массовой тишины
//! не долбим).

use crate::github::HTTP_TIMEOUT;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use uwuwu_domain::{JudgeStatus, JudgeVerdict};
use uwuwu_usecase::{slot_key, status_name, Clock, DecisionJudge, JudgeContext};
use uwuwu_usecase::{UsecaseError, UsecaseResult};

const API_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/decisions";
const MODEL_DEFAULT: &str = "gemini-decision-1";
/// Тест-шов стенда (прецедент UWUWU_OPENCODE_GATE): мок-decisions e2e.
const ENV_ENDPOINT: &str = "UWUWU_JUDGE_URL";
/// Таймаут вызова: латенс живой пробы ~0.6 с — запас на хвосты квот.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Окно скользящего лимита полосы вызовов.
const RPM_WINDOW_MS: u64 = 60_000;

/// Кап бэкоффа на 429 (RetryInfo может обещать и больше — не тише прод-тика).
pub const RATE_LIMIT_CAP_SECS: u64 = 15 * 60;
/// Дефолт бэкоффа 429 без RetryInfo в теле.
pub const RATE_LIMIT_DEFAULT_SECS: u64 = 300;

/// RetryInfo из тела 429: `details[]` с `retryDelay` ("26s", "1m").
/// Возвращает секунды; без RetryInfo — None (вызывающая сторона ставит дефолт).
pub(crate) fn parse_retry_delay(body: &serde_json::Value) -> Option<u64> {
    let err = body.get("error")?;
    for d in err.get("details")?.as_array()? {
        let kind = d.get("@type")?.as_str()?;
        if !kind.ends_with("RetryInfo") {
            continue;
        }
        let raw = d.get("retryDelay")?.as_str()?;
        let secs = if raw.ends_with('s') {
            raw.trim_end_matches('s').parse::<f64>().ok()?.ceil() as u64
        } else if raw.ends_with('m') {
            (raw.trim_end_matches('m').parse::<f64>().ok()? * 60.0).ceil() as u64
        } else {
            return None;
        };
        return Some(secs);
    }
    None
}

/// Судья на Gemini Decision 1. Ключ — env `GEMINI_API_KEY`; нет ключа —
/// конструктор None (контур выключен, тик живёт старой механикой).
pub struct GeminiJudge {
    http: reqwest::Client,
    key: String,
    model: String,
    endpoint: String,
    /// ms вызовов последней минуты (полоса — квоты тира, волна #1362/#1363).
    calls: Arc<std::sync::Mutex<VecDeque<u64>>>,
    rpm_limit: usize,
    clock: Arc<dyn Clock>,
}

impl GeminiJudge {
    /// None — ключ не задан (громкая конфигурация логируется вызывающим).
    pub fn from_env(
        model: Option<String>,
        rpm_limit: usize,
        clock: Arc<dyn Clock>,
    ) -> Option<Self> {
        let key = match std::env::var("GEMINI_API_KEY") {
            Ok(k) if !k.trim().is_empty() => k,
            _ => return None,
        };
        let endpoint = std::env::var(ENV_ENDPOINT).unwrap_or_else(|_| API_ENDPOINT.to_string());
        Some(Self {
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .timeout(HTTP_TIMEOUT)
                .build()
                .expect("reqwest client"),
            key,
            model: model.unwrap_or_else(|| MODEL_DEFAULT.to_string()),
            endpoint,
            calls: Arc::new(std::sync::Mutex::new(VecDeque::new())),
            rpm_limit,
            clock,
        })
    }

    /// Полоса вызовов: N/мин скользящим окном. Превышение — Err (тик
    /// падает в fallback; тихое ожидание внутри тика недопустимо).
    fn rpm_slot(&self) -> UsecaseResult<()> {
        let now = self.clock.now_ms();
        let mut calls = self.calls.lock().unwrap();
        while calls
            .front()
            .is_some_and(|ms| now.saturating_sub(*ms) > RPM_WINDOW_MS)
        {
            calls.pop_front();
        }
        if calls.len() >= self.rpm_limit {
            return Err(UsecaseError::Session(format!(
                "judge: полоса вызовов исчерпана ({}/мин) — тик пережидает интервалом",
                self.rpm_limit
            )));
        }
        calls.push_back(now);
        Ok(())
    }

    /// Русскоязычный промпт из контекста — форма смоука 20.11 (2/2 верных
    /// вердикта на кейсах #1248/#1101): разделы ЗАДАЧА/КОНТЕКСТ/ТЕЛО/
    /// КОММЕНТЫ/ХВОСТЫ СЕССИЙ/ВОПРОС.
    fn prompt(ctx: &JudgeContext) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "ЗАДАЧА #{} «{}» — статус «{}», стадия «{}».",
            ctx.number,
            ctx.title,
            status_name(ctx.status),
            slot_key(ctx.slot)
        );
        if let Some(branch) = &ctx.branch {
            let _ = writeln!(s, "Ветка: {branch}.");
        }
        if let Some(pr) = &ctx.pr_url {
            let _ = writeln!(s, "PR: {pr}.");
        }
        let restart = match ctx.restart_recent_min {
            Some(min) => format!(
                " сервер перезапущен {min} мин назад (каунтеры могут быть из-под рестарта);"
            ),
            None => String::new(),
        };
        if ctx.cap_point {
            // Кап-точка: сталл-гейт не звучал, тишина/idle не меряются —
            // ноль в промпте модель читала как «сессия активна прямо
            // сейчас» (ложный сигнал в решающем промпте; ревью раунда 2).
            let _ = writeln!(
                s,
                "КОНТЕКСТ КОНВЕЙЕРА: точка — кап ревью-раундов, тишина \
                 stage-сессии не меряется; вызовов судьи без смены статуса: \
                 {}; подряд request_changes: {};{restart} механика: кап = 4 \
                 подряд request_changes — выбор между паркингом (цикл не \
                 сходится, нужен владелец) и ещё одним раундом.",
                ctx.judge_calls_no_progress, ctx.review_rounds,
            );
        } else {
            let _ = writeln!(
                s,
                "КОНТЕКСТ КОНВЕЙЕРА: тишина stage-сессии {} мин; ход завершён: {}; \
                 вызовов судьи без смены статуса: {}; подряд request_changes: {};{restart} \
                 механика: автопинг при тишине 60+ мин, эскалация владельцу после \
                 пингов; кап ревью-раундов = 4 подряд request_changes.",
                ctx.silence_min,
                if ctx.idle { "да" } else { "нет" },
                ctx.judge_calls_no_progress,
                ctx.review_rounds,
            );
        }
        if !ctx.body.is_empty() {
            let _ = writeln!(s, "\nТЕЛО ЗАДАЧИ (усечено):\n{}", ctx.body);
        }
        if !ctx.comments.is_empty() {
            s.push_str("\nКОММЕНТЫ (от новых к старым, усечены, возраст — человеческий):\n");
            for (ms, body) in &ctx.comments {
                let _ = writeln!(s, "- [{}] {body}", human_age(*ms, ctx.now_ms));
            }
        }
        if !ctx.sessions.is_empty() {
            s.push_str("\nХВОСТЫ СЕССИЙ (последний ассистентский ответ):\n");
            for tail in &ctx.sessions {
                let _ = writeln!(
                    s,
                    "- [{}] (ход завершён: {}): {}",
                    slot_key(tail.slot),
                    if tail.finished { "да" } else { "нет" },
                    tail.text
                );
            }
        }
        let _ = writeln!(
            s,
            "\nВОПРОС: в какой статус перевести задачу на этом тике? Done и мерж — \
             вне полномочий (вариантов нет). «hold» — ничего не делать. Признаки \
             шума (тишина ниже порога эскалации, свежий рестарт сервера, массовые \
             эскалации, активное ревью в ленте) — не буди владельца; признаки \
             реального зависания (ход умер, слоты простаивают, тишина глубока) — \
             эскалируй."
        );
        s
    }

    fn questions_json() -> serde_json::Value {
        serde_json::json!({
            "next_status": {
                "type": "choice",
                "instructions": "В какой статус перевести эту задачу прямо сейчас?",
                "criteria": {
                    "In Progress": "Агент работает/должен продолжить — перенести в In Progress и пингануть агента",
                    "Design": "План сырой/нужна переработка плана — вернуть в Design",
                    "Ревью плана": "План готов — отправить на ревью плана",
                    "Ревью результата": "Код готов — отправить на ревью результата",
                    "Тестирование": "Ревью пройдено — отправить тестировщику",
                    "Needs attention": "Реально нужен разбор владельца — эскалировать",
                    "hold": "Оставить как есть — ничего не делать на этом тике"
                }
            },
            "owner_needed": {
                "type": "noul",
                "instructions": "Вмешательство владельца реально необходимо прямо сейчас?",
                "criteria": {
                    "true_description": "владелец нужен — без него не сдвинуть",
                    "false_description": "владелец не нужен — конвейер сам разберётся"
                }
            }
        })
    }
}

/// Возраст метки человеческим языком (модель читает «3 ч назад», не epoch).
fn human_age(then_ms: u64, now_ms: u64) -> String {
    let mins = now_ms.saturating_sub(then_ms) / 60_000;
    match mins {
        0 => "только что".into(),
        1..=59 => format!("{mins} мин назад"),
        m @ 60..=1439 => format!("{} ч назад", m / 60),
        m => format!("{} д назад", m / 1440),
    }
}

#[async_trait]
impl DecisionJudge for GeminiJudge {
    async fn decide(&self, ctx: &JudgeContext) -> UsecaseResult<JudgeVerdict> {
        self.rpm_slot()?;
        let body = serde_json::json!({
            "model": self.model,
            "input": [{ "type": "text", "text": Self::prompt(ctx) }],
            "questions": Self::questions_json(),
        });
        let resp = self
            .http
            .post(&self.endpoint)
            .header("x-goog-api-key", &self.key)
            .json(&body)
            .timeout(CALL_TIMEOUT)
            .send()
            .await
            .map_err(|e| UsecaseError::Session(format!("judge: сеть: {e}")))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Session(format!("judge: тело ответа: {e}")))?;
        if status == 429 {
            // RetryInfo из тела — единый парсер с семантикой #1363; спать
            // внутри тика нельзя — структурный Err (fallback тика сработает,
            // следующий вызов — после интервала судьи; обработчики Err
            // читают задержку из варианта, не из человеческой строки)
            let parsed: serde_json::Value =
                serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            let secs = parse_retry_delay(&parsed)
                .unwrap_or(RATE_LIMIT_DEFAULT_SECS)
                .min(RATE_LIMIT_CAP_SECS);
            return Err(UsecaseError::RateLimited {
                service: "judge".into(),
                retry_after_secs: Some(secs),
            });
        }
        if !(200..300).contains(&status) {
            return Err(UsecaseError::Session(format!(
                "judge: HTTP {status}: {}",
                text.chars().take(300).collect::<String>()
            )));
        }
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| UsecaseError::Session(format!("judge: не JSON: {e}")))?;
        let answers = parsed
            .get("answers")
            .ok_or_else(|| UsecaseError::Session("judge: нет answers в ответе".into()))?;
        let next = answers
            .get("next_status")
            .ok_or_else(|| UsecaseError::Session("judge: нет next_status".into()))?;
        let choice = next
            .get("choice")
            .and_then(|c| c.as_str())
            .ok_or_else(|| UsecaseError::Session("judge: нет choice".into()))?;
        let status = JudgeStatus::parse(choice).ok_or_else(|| {
            UsecaseError::Session(format!("judge: неизвестный choice «{choice}»"))
        })?;
        let confidence = next
            .get("confidence")
            .and_then(|c| c.as_f64())
            .unwrap_or(0.0);
        let owner_needed = answers
            .get("owner_needed")
            .and_then(|o| o.get("noul"))
            .and_then(|n| n.as_f64())
            .unwrap_or(0.0);
        let tokens = parsed
            .get("usage")
            .and_then(|u| u.get("total_input_tokens"))
            .and_then(|t| t.as_u64())
            .unwrap_or_default();
        tracing::info!(
            event = "judge.call",
            number = ctx.number,
            tokens = tokens,
            confidence = format!("{confidence:.2}"),
            owner_needed = format!("{owner_needed:.2}"),
            "Судья: вызов состоялся"
        );
        Ok(JudgeVerdict {
            status,
            owner_needed,
            confidence,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uwuwu_usecase::JudgeSessionTail;

    /// Полоса вызовов: N+1-й вызов в окне — Err (квоты тира).
    #[test]
    fn rpm_limit_blocks_excess_calls() {
        struct FrozenClock;
        impl Clock for FrozenClock {
            fn utc_minute(&self) -> uwuwu_domain::UtcMinute {
                uwuwu_domain::UtcMinute {
                    minute_of_day: 100,
                    weekday: 3,
                }
            }
            fn now_ms(&self) -> u64 {
                1_000_000
            }
        }
        let judge = GeminiJudge {
            http: reqwest::Client::new(),
            key: "test".into(),
            model: MODEL_DEFAULT.into(),
            endpoint: API_ENDPOINT.into(),
            calls: Arc::new(std::sync::Mutex::new(VecDeque::new())),
            rpm_limit: 2,
            clock: Arc::new(FrozenClock),
        };
        assert!(judge.rpm_slot().is_ok());
        assert!(judge.rpm_slot().is_ok());
        assert!(judge.rpm_slot().is_err(), "третий вызов в окне — отказ");
    }

    /// Промпт несёт все разделы контекста (форма смоука 20.11).
    #[test]
    fn prompt_covers_all_context_sections() {
        let ctx = JudgeContext {
            board: "uwuwu".into(),
            number: 42,
            title: "Т".into(),
            status: uwuwu_domain::Status::InProgress,
            slot: uwuwu_domain::Slot::Dev,
            silence_min: 40,
            idle: true,
            judge_calls_no_progress: 1,
            review_rounds: 0,
            branch: Some("work/task-42".into()),
            pr_url: None,
            restart_recent_min: Some(6),
            body: "тело".into(),
            comments: vec![(1u64, "коммент".into())],
            now_ms: 10_000_000,
            cap_point: false,
            sessions: vec![JudgeSessionTail {
                slot: uwuwu_domain::Slot::Dev,
                text: "ответ".into(),
                finished: true,
            }],
        };
        let p = GeminiJudge::prompt(&ctx);
        for section in [
            "ЗАДАЧА #42",
            "КОНТЕКСТ КОНВЕЙЕРА",
            "сервер перезапущен 6 мин назад",
            "ТЕЛО ЗАДАЧИ",
            "КОММЕНТЫ",
            "ХВОСТЫ СЕССИЙ",
            "ВОПРОС",
            "In Progress",
        ] {
            assert!(p.contains(section), "промпт должен нести «{section}»");
        }
    }

    /// Кап-точка: промпт не сообщает ложную тишину («0 мин, ход завершён:
    /// нет» читалось моделью как «сессия активна сейчас») — честная
    /// формулировка капа (ревью раунда 2, L).
    #[test]
    fn prompt_at_cap_point_reports_no_fake_silence() {
        let ctx = JudgeContext {
            board: "uwuwu".into(),
            number: 42,
            title: "Т".into(),
            status: uwuwu_domain::Status::InProgress,
            slot: uwuwu_domain::Slot::Dev,
            silence_min: 0,
            idle: false,
            judge_calls_no_progress: 2,
            review_rounds: 5,
            branch: None,
            pr_url: None,
            restart_recent_min: None,
            body: "тело".into(),
            comments: vec![],
            now_ms: 10_000_000,
            cap_point: true,
            sessions: vec![],
        };
        let p = GeminiJudge::prompt(&ctx);
        assert!(
            p.contains("кап ревью-раундов, тишина stage-сессии не меряется"),
            "кап-точка названа честно"
        );
        assert!(
            !p.contains("тишина stage-сессии 0 мин"),
            "ложная тишина не попадает в промпт"
        );
        assert!(!p.contains("ход завершён: нет"), "ложный idle не в промпте");
    }

    /// One-shot HTTP-мок границы Decisions API: локальный листенер, один
    /// ответ — endpoint подставляется полем структуры, env-швы не мутируются
    /// (параллельные тесты не гоняются за переменные окружения).
    fn one_shot(status_line: &'static str, body: String) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut sock, _) = listener.accept().unwrap();
            // запрос читаем до конца заголовков — reqwest ждёт записи ответа
            // в живой сокет; тело не читаем (ответ не зависит от него)
            let mut buf = vec![0u8; 16 * 1024];
            let mut got = 0usize;
            while got < buf.len() {
                let n = match sock.read(&mut buf[got..]) {
                    Ok(n) => n,
                    Err(_) => break,
                };
                if n == 0 {
                    break;
                }
                got += n;
                if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let resp = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes());
        });
        format!("http://{addr}/decisions")
    }

    struct FrozenClock;
    impl Clock for FrozenClock {
        fn utc_minute(&self) -> uwuwu_domain::UtcMinute {
            uwuwu_domain::UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            1_000_000
        }
    }

    fn judge_at(endpoint: String) -> GeminiJudge {
        GeminiJudge {
            http: reqwest::Client::new(),
            key: "test".into(),
            model: MODEL_DEFAULT.into(),
            endpoint,
            calls: Arc::new(std::sync::Mutex::new(VecDeque::new())),
            rpm_limit: 100,
            clock: Arc::new(FrozenClock),
        }
    }

    fn judge_ctx() -> JudgeContext {
        JudgeContext {
            board: "uwuwu".into(),
            number: 42,
            title: "Т".into(),
            status: uwuwu_domain::Status::ReviewResult,
            slot: uwuwu_domain::Slot::ResultReview,
            silence_min: 40,
            idle: true,
            judge_calls_no_progress: 0,
            review_rounds: 0,
            branch: Some("work/task-42".into()),
            pr_url: None,
            restart_recent_min: None,
            body: "тело".into(),
            comments: vec![],
            now_ms: 10_000_000,
            cap_point: false,
            sessions: vec![],
        }
    }

    /// Успешный ответ: answers-map (choice + confidence + noul) читается
    /// структурно — контракт Decisions API (сверен живой пробой 20.11).
    #[tokio::test]
    async fn decide_parses_answers_map_verdict() {
        let body = serde_json::json!({
            "answers": {
                "next_status": {
                    "choice": "Тестирование",
                    "confidence": 0.8,
                    "probabilities": {"Тестирование": 0.8}
                },
                "owner_needed": {"noul": 0.9}
            },
            "usage": {"total_input_tokens": 512}
        })
        .to_string();
        let url = one_shot("HTTP/1.1 200 OK", body);
        let v = judge_at(url).decide(&judge_ctx()).await.unwrap();
        assert_eq!(v.status, JudgeStatus::Testing);
        assert!((v.confidence - 0.8).abs() < 1e-9, "confidence из ответа");
        assert!((v.owner_needed - 0.9).abs() < 1e-9, "noul из ответа");
    }

    /// 429 с RetryInfo в теле — структурный RateLimited с задержкой из
    /// ответа (объединённый парсер с семантикой #1363), не текстовая ошибка.
    #[tokio::test]
    async fn decide_429_maps_to_structural_rate_limited() {
        let body = serde_json::json!({
            "error": {
                "code": 429,
                "message": "quota exceeded",
                "details": [
                    {"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "26s"}
                ]
            }
        })
        .to_string();
        let url = one_shot("HTTP/1.1 429 Too Many Requests", body);
        let err = judge_at(url).decide(&judge_ctx()).await.unwrap_err();
        match err {
            UsecaseError::RateLimited {
                service,
                retry_after_secs,
            } => {
                assert_eq!(service, "judge");
                assert_eq!(retry_after_secs, Some(26), "RetryInfo 26s → 26 сек");
            }
            other => panic!("ожидался структурный RateLimited, получен: {other:?}"),
        }
    }

    /// 429 без RetryInfo в теле — дефолтный бэкофф контура (не паника и
    /// не текстовый Err: обработчики Err читают задержку из варианта).
    #[tokio::test]
    async fn decide_429_without_retry_info_defaults_backoff() {
        let body = "not a google error body".to_string();
        let url = one_shot("HTTP/1.1 429 Too Many Requests", body);
        let err = judge_at(url).decide(&judge_ctx()).await.unwrap_err();
        match err {
            UsecaseError::RateLimited {
                service,
                retry_after_secs,
            } => {
                assert_eq!(service, "judge");
                assert_eq!(retry_after_secs, Some(RATE_LIMIT_DEFAULT_SECS));
            }
            other => panic!("ожидался структурный RateLimited, получен: {other:?}"),
        }
    }

    #[test]
    fn retry_delay_minutes_and_absent() {
        let mins = serde_json::json!({
            "error": {"details": [{"@type": "google.rpc.RetryInfo", "retryDelay": "2m"}]}
        });
        assert_eq!(parse_retry_delay(&mins), Some(120));
        let none = serde_json::json!({"error": {"message": "no details"}});
        assert_eq!(parse_retry_delay(&none), None);
    }

    #[test]
    fn retry_delay_fractional_duration_rounds_up() {
        let frac = serde_json::json!({
            "error": {"details": [{"@type": "google.rpc.RetryInfo", "retryDelay": "26.5s"}]}
        });
        assert_eq!(parse_retry_delay(&frac), Some(27));
        let frac_min = serde_json::json!({
            "error": {"details": [{"@type": "google.rpc.RetryInfo", "retryDelay": "1.5m"}]}
        });
        assert_eq!(parse_retry_delay(&frac_min), Some(90));
    }
}
