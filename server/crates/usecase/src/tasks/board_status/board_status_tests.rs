//! Юнит-пины гварда rename статуса (#1447) на фейк-носителе: дословный
//! payload, отказы до мутации, verify-отказы (снос опции/пустые values),
//! write-ahead журнал, лаг-классификация, warn-формы. Лежат рядом с ядром
//! (паттерн pv2_cache_tests) — file-complexity ядра не считают тесты.

use super::*;
use std::sync::Mutex;

struct FakeClock;
impl Clock for FakeClock {
    fn utc_minute(&self) -> uwuwu_domain::UtcMinute {
        uwuwu_domain::UtcMinute {
            minute_of_day: 0,
            weekday: 0,
        }
    }
    fn now_ms(&self) -> u64 {
        1_700_000_000_000
    }
}

struct FakeJournal {
    entries: Mutex<Vec<StatusPreImage>>,
}
impl FakeJournal {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(Vec::new()),
        })
    }
}
impl StatusPreImageStore for FakeJournal {
    fn put(&self, image: &StatusPreImage) -> UsecaseResult<()> {
        self.entries.lock().expect("lock").push(image.clone());
        Ok(())
    }
    fn path_hint(&self) -> String {
        "/data/rename-pre-image.json".into()
    }
}

/// Фейк-носитель с живым каталогом: rename меняет его, verify_catalog
/// читает актуальное; значения строк следуют за опцией (fresh), либо
/// отдают стейл-снимок (лаг — симуляция read-лага ADR-113).
struct FakeRepo {
    live: Mutex<Vec<StatusOption>>,
    dist_before: HashMap<u64, Option<String>>,
    /// Переопределение payload'а мутации (фейк повреждённого ответа).
    payload_override: Mutex<Option<Vec<StatusOption>>>,
    /// FIFO стейл-снимков каталога для verify_catalog (лаг-симуляция);
    /// исчерпан — чтение живого каталога.
    stale_catalog: Mutex<Vec<Vec<StatusOption>>>,
    /// Переопределение after-дистрибуции (None — свежая: from→to).
    dist_after_override: Mutex<Option<HashMap<u64, Option<String>>>>,
    /// Счётчик fresh_distribution: первый вызов — pre-image.
    dist_calls: Mutex<usize>,
    mutations: Mutex<Vec<(String, String)>>,
    journal_seen_before_mutation: Mutex<bool>,
    journal: Arc<FakeJournal>,
}

impl FakeRepo {
    fn new(options: Vec<StatusOption>, dist_before: HashMap<u64, Option<String>>) -> Self {
        Self {
            live: Mutex::new(options),
            dist_before,
            payload_override: Mutex::new(None),
            stale_catalog: Mutex::new(Vec::new()),
            dist_after_override: Mutex::new(None),
            dist_calls: Mutex::new(0),
            mutations: Mutex::new(Vec::new()),
            journal_seen_before_mutation: Mutex::new(false),
            journal: FakeJournal::new(),
        }
    }

    fn opt(id: &str, name: &str, color: Option<&str>) -> StatusOption {
        StatusOption {
            id: id.into(),
            name: name.into(),
            color: color.map(Into::into),
            description: None,
        }
    }

    fn renamed_dist(&self, from: &str, to: &str) -> HashMap<u64, Option<String>> {
        self.dist_before
            .iter()
            .map(|(n, v)| {
                let fresh = v
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|raw| {
                        if raw == from {
                            to.to_string()
                        } else {
                            raw.to_string()
                        }
                    });
                (*n, fresh)
            })
            .collect()
    }
}

#[async_trait]
impl BoardStatusRepo for FakeRepo {
    async fn fresh_field(&self, _board: &str) -> UsecaseResult<StatusFieldImage> {
        Ok(StatusFieldImage {
            field_node_id: "PVTF_test".into(),
            options: self.live.lock().expect("lock").clone(),
        })
    }

    async fn fresh_distribution(
        &self,
        _board: &str,
    ) -> UsecaseResult<HashMap<u64, Option<String>>> {
        // первый вызов — pre-image (до мутации), дальнейшие — post-verify:
        // override обязан перекрывать только post-чтение
        let call = {
            let mut c = self.dist_calls.lock().expect("lock");
            *c += 1;
            *c
        };
        if call == 1 {
            return Ok(self.dist_before.clone());
        }
        if let Some(over) = self.dist_after_override.lock().expect("lock").clone() {
            return Ok(over);
        }
        // свежий носитель: значения привязаны к опции — читаются новым именем
        let pre = self.journal.entries.lock().expect("lock").last().cloned();
        match pre {
            Some(pre) => Ok(self.renamed_dist(&pre.from, &pre.to)),
            None => Ok(self.dist_before.clone()),
        }
    }

    async fn rename(
        &self,
        _image: &StatusFieldImage,
        option_id: &str,
        new_name: &str,
    ) -> UsecaseResult<Vec<StatusOption>> {
        // гвард write-ahead: журнал обязан существовать к моменту мутации
        *self.journal_seen_before_mutation.lock().expect("lock") =
            !self.journal.entries.lock().expect("lock").is_empty();
        self.mutations
            .lock()
            .expect("lock")
            .push((option_id.to_string(), new_name.to_string()));
        let mut live = self.live.lock().expect("lock");
        for o in live.iter_mut() {
            if o.id == option_id {
                o.name = new_name.to_string();
            }
        }
        let result = live.clone();
        drop(live);
        // повреждённый ответ носителя (фейк структурной потери)
        let payload = self.payload_override.lock().expect("lock").clone();
        Ok(payload.unwrap_or(result))
    }

    async fn verify_catalog(&self, _board: &str) -> UsecaseResult<Vec<(String, String)>> {
        let mut stale = self.stale_catalog.lock().expect("lock");
        let snapshot = if !stale.is_empty() {
            stale.remove(0)
        } else {
            self.live.lock().expect("lock").clone()
        };
        Ok(snapshot
            .iter()
            .map(|o| (o.id.clone(), o.name.clone()))
            .collect())
    }

    fn invalidate_meta(&self, _board: &str) {}
}

fn uc(repo: FakeRepo) -> (Arc<BoardStatusRenameUseCase>, Arc<FakeRepo>) {
    let repo = Arc::new(repo);
    let usecase = BoardStatusRenameUseCase {
        repo: repo.clone(),
        journal: repo.journal.clone(),
        boards: vec![("uwuwu".into(), "Задачи uwuwu".into())],
        clock: Arc::new(FakeClock),
    };
    (Arc::new(usecase), repo)
}

fn sample_options() -> Vec<StatusOption> {
    vec![
        FakeRepo::opt("a1", "Draft", Some("GRAY")),
        FakeRepo::opt("b2", "Wait", Some("BLUE")),
        FakeRepo::opt("c3", "Done", None),
    ]
}

fn sample_dist() -> HashMap<u64, Option<String>> {
    HashMap::from([
        (1u64, Some("Wait".into())),
        (2u64, Some("Draft".into())),
        (3u64, None),
    ])
}

#[tokio::test]
async fn rename_payload_is_verbatim_preimage_with_one_name_changed() {
    let (usecase, repo) = uc(FakeRepo::new(sample_options(), sample_dist()));
    // to — канон-статус вне набора носителя: warn нет, коллизии нет
    let report = usecase
        .rename("uwuwu", "Wait", "Тестирование")
        .await
        .expect("ok");
    let mutations = repo.mutations.lock().expect("lock");
    assert_eq!(mutations.len(), 1, "ровно одна мутация");
    assert_eq!(mutations[0], ("b2".to_string(), "Тестирование".to_string()));
    // отчёт: гистограммы до/после, id переименованной опции сохранён
    assert_eq!(report.histogram_before.get("Wait"), Some(&1));
    assert_eq!(report.histogram_after.get("Тестирование"), Some(&1));
    assert_eq!(report.histogram_before.get("Draft"), Some(&1));
    assert_eq!(report.option_id, "b2");
    assert_eq!(report.options_total, 3);
    assert!(
        report.warnings.is_empty(),
        "канон→канон без warn: {:?}",
        report.warnings
    );
    assert!(
        *repo.journal_seen_before_mutation.lock().expect("lock"),
        "журнал должен существовать к моменту мутации (write-ahead)"
    );
}

#[tokio::test]
async fn rename_keeps_color_and_description_verbatim() {
    let (usecase, repo) = uc(FakeRepo::new(sample_options(), sample_dist()));
    usecase.rename("uwuwu", "Wait", "Hold").await.expect("ok");
    let journal = repo.journal.entries.lock().expect("lock");
    let pre = &journal[0];
    let wait = pre.options.iter().find(|o| o.id == "b2").expect("opt");
    assert_eq!(
        wait.color.as_deref(),
        Some("BLUE"),
        "оформление pre-image дословно"
    );
    assert_eq!(wait.name, "Wait");
    let done = pre.options.iter().find(|o| o.id == "c3").expect("opt");
    assert_eq!(done.color, None, "None-оформление сохраняется");
    // живой каталог носителя: только имя изменено, оформление не тронуто
    let live = repo.live.lock().expect("lock");
    let held = live.iter().find(|o| o.id == "b2").expect("live");
    assert_eq!(held.name, "Hold");
    assert_eq!(held.color.as_deref(), Some("BLUE"));
}

#[tokio::test]
async fn verify_refuses_option_loss_with_journal_hint() {
    let (usecase, repo) = uc(FakeRepo::new(sample_options(), sample_dist()));
    // payload мутации потерял опцию c3 — структурная потеря
    *repo.payload_override.lock().expect("lock") = Some(vec![
        FakeRepo::opt("a1", "Draft", Some("GRAY")),
        FakeRepo::opt("b2", "Hold", Some("BLUE")),
    ]);
    let err = usecase
        .rename("uwuwu", "Wait", "Hold")
        .await
        .expect_err("fail");
    let text = err.to_string();
    assert!(text.contains("POST-VERIFY FAILED"), "{text}");
    assert!(text.contains("option(s) lost"), "{text}");
    assert!(text.contains("/data/rename-pre-image.json"), "{text}");
    assert!(
        text.to_uppercase().contains("BEFORE MANUAL ROLLBACK"),
        "{text}"
    );
}

#[tokio::test]
async fn verify_refuses_empty_field_values() {
    let (usecase, repo) = uc(FakeRepo::new(sample_options(), sample_dist()));
    // класс инцидента origa: значение строки опустело
    *repo.dist_after_override.lock().expect("lock") = Some(HashMap::from([
        (1u64, Some("Wait".into())),
        (2u64, None),
        (3u64, None),
    ]));
    let err = usecase
        .rename("uwuwu", "Wait", "Hold")
        .await
        .expect_err("fail");
    assert!(
        err.to_string().contains("fieldValues went empty"),
        "{}",
        err
    );
}

#[tokio::test]
async fn verify_refuses_vanished_row() {
    let (usecase, repo) = uc(FakeRepo::new(sample_options(), sample_dist()));
    *repo.dist_after_override.lock().expect("lock") =
        Some(HashMap::from([(1u64, Some("Hold".into()))]));
    let err = usecase
        .rename("uwuwu", "Wait", "Hold")
        .await
        .expect_err("fail");
    assert!(
        err.to_string().contains("vanished from the board index"),
        "{}",
        err
    );
}

#[tokio::test]
async fn verify_refuses_dangling_value_reference() {
    let (usecase, repo) = uc(FakeRepo::new(sample_options(), sample_dist()));
    // значение смотрит на несуществующую опцию — потеря привязки
    *repo.dist_after_override.lock().expect("lock") = Some(HashMap::from([
        (1u64, Some("Мусор".into())),
        (2u64, Some("Draft".into())),
        (3u64, None),
    ]));
    let err = usecase
        .rename("uwuwu", "Wait", "Hold")
        .await
        .expect_err("fail");
    assert!(err.to_string().contains("unknown option"), "{}", err);
}

#[tokio::test]
async fn persistent_catalog_lag_is_warning_not_failure() {
    let (usecase, repo) = uc(FakeRepo::new(sample_options(), sample_dist()));
    // все контрольные чтения отдают старое имя при целом id-наборе — лаг
    *repo.stale_catalog.lock().expect("lock") = vec![sample_options(); VERIFY_ATTEMPTS];
    let report = usecase.rename("uwuwu", "Wait", "Hold").await.expect("ok");
    assert!(
        report.warnings.iter().any(|w| w.contains("lagged")),
        "лаг-пометка: {:?}",
        report.warnings
    );
}

#[tokio::test]
async fn lag_resolving_on_retry_is_clean_success() {
    let (usecase, repo) = uc(FakeRepo::new(sample_options(), sample_dist()));
    // первое чтение стейл, второе — живой каталог: ретрай дотянулся
    *repo.stale_catalog.lock().expect("lock") = vec![sample_options()];
    let report = usecase
        .rename("uwuwu", "Wait", "Тестирование")
        .await
        .expect("ok");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

#[tokio::test]
async fn unknown_from_refuses_before_mutation() {
    let (usecase, repo) = uc(FakeRepo::new(sample_options(), sample_dist()));
    let err = usecase
        .rename("uwuwu", "Вейс", "Hold")
        .await
        .expect_err("fail");
    assert!(err.to_string().contains("not found"), "{err}");
    assert!(err.to_string().contains("already renamed"), "{err}");
    assert!(
        repo.mutations.lock().expect("lock").is_empty(),
        "мутаций нет"
    );
}

#[tokio::test]
async fn to_equals_from_refuses_before_mutation() {
    let (usecase, repo) = uc(FakeRepo::new(sample_options(), sample_dist()));
    let err = usecase
        .rename("uwuwu", "Wait", "Wait")
        .await
        .expect_err("fail");
    assert!(err.to_string().contains("no-op"), "{err}");
    assert!(
        repo.mutations.lock().expect("lock").is_empty(),
        "мутаций нет"
    );
}

#[tokio::test]
async fn empty_names_refuse() {
    let (usecase, repo) = uc(FakeRepo::new(sample_options(), sample_dist()));
    assert!(usecase.rename("uwuwu", "  ", "Hold").await.is_err());
    assert!(usecase.rename("uwuwu", "Wait", "   ").await.is_err());
    assert!(repo.mutations.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn name_collision_refuses_before_mutation() {
    let (usecase, repo) = uc(FakeRepo::new(sample_options(), sample_dist()));
    let err = usecase
        .rename("uwuwu", "Wait", "Done")
        .await
        .expect_err("fail");
    assert!(err.to_string().contains("collision"), "{err}");
    assert!(
        repo.mutations.lock().expect("lock").is_empty(),
        "мутаций нет"
    );
}

#[tokio::test]
async fn project_outside_config_refuses() {
    let (usecase, _) = uc(FakeRepo::new(sample_options(), sample_dist()));
    let err = usecase
        .rename("origa", "Wait", "Hold")
        .await
        .expect_err("fail");
    assert!(err.to_string().contains("not in config"), "{err}");
}

#[tokio::test]
async fn non_canonical_to_warns() {
    let (usecase, _) = uc(FakeRepo::new(sample_options(), sample_dist()));
    let report = usecase
        .rename("uwuwu", "Wait", "Парковка")
        .await
        .expect("ok");
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("not a canonical")),
        "{:?}",
        report.warnings
    );
}

#[tokio::test]
async fn alias_rename_has_no_warning() {
    // «Вейт»→«Wait»: оба парсятся в один доменный статус — warn нет
    let options = vec![
        FakeRepo::opt("x1", "Вейт", None),
        FakeRepo::opt("a1", "Draft", None),
    ];
    let dist = HashMap::from([(9u64, Some("Вейт".into()))]);
    let (usecase, _) = uc(FakeRepo::new(options, dist));
    let report = usecase.rename("uwuwu", "Вейт", "Wait").await.expect("ok");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

#[tokio::test]
async fn journal_carries_full_per_row_snapshot() {
    let (usecase, repo) = uc(FakeRepo::new(sample_options(), sample_dist()));
    usecase.rename("uwuwu", "Wait", "Hold").await.expect("ok");
    let journal = repo.journal.entries.lock().expect("lock");
    assert_eq!(journal.len(), 1);
    let pre = &journal[0];
    assert_eq!(pre.board, "Задачи uwuwu");
    assert_eq!(
        pre.distribution.len(),
        3,
        "per-row снимок цел, включая пустые"
    );
    assert_eq!(pre.options.len(), 3);
    assert_eq!(pre.created_ms, 1_700_000_000_000);
}

#[test]
fn histogram_counts_unknown_names_under_placeholder() {
    let dist = HashMap::from([
        (1u64, Some("Wait".into())),
        (2u64, Some("Мусор".into())),
        (3u64, None),
    ]);
    let h = histogram(&sample_options(), &dist);
    assert_eq!(h.get("Wait"), Some(&1));
    assert_eq!(h.get("(unknown)"), Some(&1));
    assert_eq!(h.get("Draft"), Some(&0));
}
