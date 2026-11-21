//! Адаптер судьи (#1364): Gemini Decision 1 (`gemini-decision-1`, Google
//! Decisions API free tier) за портом `DecisionJudge`. Контракт ответа —
//! answers-map (choice+probabilities+confidence, noul), сверен живой
//! пробой 20.11 и таблицей #1362. 429 — RetryInfo из тела (единый парсер
//! с семантикой #1363), полоса вызовов — локальный скользящий лимит
//! (квоты волны общие, TPM-пик массовой тишины не долбим).

use crate::search::{parse_retry_delay, RATE_LIMIT_CAP_SECS, RATE_LIMIT_DEFAULT_SECS};
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
            http: reqwest::Client::new(),
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
        let _ = writeln!(
            s,
            "КОНТЕКСТ КОНВЕЙЕРА: тишина stage-сессии {} мин; ход завершён: {}; \
             вызовов судьи без смены статуса: {}; подряд request_changes: {};{} \
             механика: автопинг при тишине 60+ мин, эскалация владельцу после \
             пингов; кап ревью-раундов = 4 подряд request_changes.",
            ctx.silence_min,
            if ctx.idle { "да" } else { "нет" },
            ctx.judge_calls_no_progress,
            ctx.review_rounds,
            if let Some(min) = ctx.restart_recent_min {
                format!(
                    " сервер перезапущен {} мин назад (каунтеры могут быть из-под рестарта);",
                    min
                )
            } else {
                String::new()
            }
        );
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
            // внутри тика нельзя — Err с задержкой в тексте (fallback тика
            // сработает, следующий вызов — после интервала судьи)
            let parsed: serde_json::Value =
                serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            let secs = parse_retry_delay(&parsed)
                .unwrap_or(RATE_LIMIT_DEFAULT_SECS)
                .min(RATE_LIMIT_CAP_SECS);
            return Err(UsecaseError::Session(format!(
                "judge: 429 rate limit — retry через ~{secs} с (RetryInfo)"
            )));
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
}
