//! Суточный бюджет эмбеддингов синка (#1473, срез А): sync не съедает
//! дневную квоту Gemini — резерв остаётся query-эмбеддингам поисков.
//! Носитель — semantic_budget.json рядом с Lance-том (паттерн
//! semantic_shas.json). День окна — полночь Pacific: Gemini free tier
//! резетится в midnight PT; фиксированный UTC-8, летний DST-дрейф в час
//! принят осознанно — недоучёт деградирует через честный 429, не тихо.

use std::path::{Path, PathBuf};

use uwuwu_usecase::UsecaseResult;

use super::semantic_backend::err;

/// Суточный кап запросов sync к Gemini (#1473): RPD 1000 (429-метаданные
/// спайка #1363) минус резерв ≈300 на query-эмбеддинги поисков — фактическое
/// потребление поисков на порядок меньше, резерв с запасом.
pub const SYNC_DAILY_CAP: u64 = 700;

const BUDGET_FILE: &str = "semantic_budget.json";

/// Счётчик расхода одного суточного окна.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BudgetState {
    pub day: String,
    pub spent: u64,
}

/// Дата текущего окна бюджета: полночь Pacific по фиксированному UTC-8.
pub fn budget_day_now() -> String {
    let offset = chrono::FixedOffset::west_opt(8 * 3600).expect("fixed offset UTC-8");
    chrono::Utc::now()
        .with_timezone(&offset)
        .format("%Y-%m-%d")
        .to_string()
}

/// Путь счётчика: рядом с Lance-том, рядом с semantic_shas.json.
pub fn budget_path(volume: &Path) -> PathBuf {
    volume.join(BUDGET_FILE)
}

/// Свежий стейт: файл отсутствует/битый/чужого дня — счёт с нуля.
/// Недоучёт деградирует мягко (429 + честные строки синка), не тихо.
pub fn read_state(path: &Path) -> BudgetState {
    let today = budget_day_now();
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<BudgetState>(&s).ok())
        .filter(|state| state.day == today)
        .unwrap_or(BudgetState {
            day: today,
            spent: 0,
        })
}

/// Персист счётчика. Отказ записи не роняет sync — недоучёт до рестарта,
/// громкий Err наверх решает вызывающий код (warn вместо abort).
pub fn write_state(path: &Path, state: &BudgetState) -> UsecaseResult<()> {
    let json = serde_json::to_string(state).map_err(|e| err(format!("budget json: {e}")))?;
    std::fs::write(path, json).map_err(|e| err(format!("budget write: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_day_is_pacific_date() {
        // Формат ISO-даты без времени — окно суток, не момент.
        let day = budget_day_now();
        assert_eq!(day.len(), 10, "YYYY-MM-DD, факт: {day}");
        assert!(day.chars().nth(4) == Some('-') && day.chars().nth(7) == Some('-'));
    }

    #[test]
    fn unreadable_or_stale_file_reads_as_fresh_day() {
        let dir = std::env::temp_dir().join(format!("uwuwu-budget-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmpdir");
        let path = dir.join("semantic_budget.json");

        // нет файла — ноль
        let fresh = read_state(&path);
        assert_eq!(fresh.spent, 0);
        assert_eq!(fresh.day, budget_day_now());

        // чужой день — сброс, несмотря на большой spent
        std::fs::write(&path, r#"{"day":"2000-01-01","spent":650}"#).expect("seed");
        let stale = read_state(&path);
        assert_eq!(stale.spent, 0);

        // битый json — сброс
        std::fs::write(&path, "мусор").expect("seed garbage");
        let garbage = read_state(&path);
        assert_eq!(garbage.spent, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn written_state_round_trips() {
        let dir = std::env::temp_dir().join(format!("uwuwu-budget-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmpdir");
        let path = dir.join("semantic_budget.json");

        let state = BudgetState {
            day: budget_day_now(),
            spent: 42,
        };
        write_state(&path, &state).expect("write ok");
        let back = read_state(&path);
        assert_eq!(back, state);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
