//! Серверный rename статуса канбана (#1447): pre-image → мутация →
//! post-verify → громкий отказ с pre-image в журнале. Семья гвардов
//! тихой потери носителя (#1158/#1409): replace-семантика updateProjectV2Field
//! безопасной не считается (инцидент origa 20.11, корень не установлен) —
//! гарантию даёт дословный pre-image payload + контрольное чтение после
//! записи. Мутация — GraphQL (REST ProjectsV2 полей не мутирует —
//! docs.github.com 22.11: только List/Add/Get), контрольные чтения —
//! REST-поверхность Pv2 (независима от мутационной). Не-каноническое
//! новое имя — warn, не отказ: строки выпадут из рабочих списков,
//! решение за владельцем (стоп-краны не заводим).

use crate::error::{UsecaseError, UsecaseResult};
use crate::naming::parse_status;
use crate::ports::Clock;
use async_trait::async_trait;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Опция поля Status: hex-id (единый между REST/GraphQL поверхностями),
/// raw-имя, оформление (передаётся в мутацию дословно — не сбрасывать).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StatusOption {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub description: Option<String>,
}

/// Свежий каталог Status-поля (pre-image опций): node-id носителей
/// GraphQL-мутации + полный набор опций.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusFieldImage {
    pub project_node_id: String,
    pub field_node_id: String,
    pub options: Vec<StatusOption>,
}

/// Write-ahead pre-image: пишется ДО мутации, хранит последний снимок
/// на доску — материал ручного отката (путь восстановления #1441).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StatusPreImage {
    pub board: String,
    pub from: String,
    pub to: String,
    pub created_ms: u64,
    pub options: Vec<StatusOption>,
    /// number → raw Status (None/пустое — не задан; пустые сохранены).
    pub distribution: HashMap<u64, Option<String>>,
}

/// Носитель pre-image журнала (сервер: JSON рядом со state.json —
/// переживает деплой/реверт, stdout Railway для отката не годится).
pub trait StatusPreImageStore: Send + Sync {
    /// Write-ahead: вызов ДО мутации; перезаписывает снимок доски.
    fn put(&self, image: &StatusPreImage) -> UsecaseResult<()>;
    /// Путь журнала — в тексты отказов и отчёт (где искать pre-image).
    fn path_hint(&self) -> String;
}

/// Порт операции: свежие пробы носителя (мимо всех кэшей — сервер верит
/// только своим пробам на момент мутации, канон clean.rs) + мутация.
#[async_trait]
pub trait BoardStatusRepo: Send + Sync {
    /// Свежий каталог Status-поля (GraphQL, мимо кэшей).
    async fn fresh_field(&self, board: &str) -> UsecaseResult<StatusFieldImage>;
    /// Свежее распределение строк по статусам (REST, мимо кэшей).
    async fn fresh_distribution(&self, board: &str) -> UsecaseResult<HashMap<u64, Option<String>>>;
    /// Мутация rename по option-id: носителю уходит ДОСЛОВНЫЙ набор
    /// каталога с одним изменённым именем; возврат — options payload'а.
    async fn rename(
        &self,
        image: &StatusFieldImage,
        option_id: &str,
        new_name: &str,
    ) -> UsecaseResult<Vec<StatusOption>>;
    /// Контрольное чтение каталога (id/name) — REST-поверхность, мимо
    /// кэшей (независимая от мутационной GraphQL поверхность).
    async fn verify_catalog(&self, board: &str) -> UsecaseResult<Vec<(String, String)>>;
    /// Инвалидация meta-кэша доски: TTL 600с иначе отдаст стейл каталог.
    fn invalidate_meta(&self, board: &str);
}

/// Контрольных чтений каталога после мутации: read-лаг Projects v2
/// минутного масштаба (ADR-113) — лаг-форма не отказ (см. VerifyOutcome).
pub const VERIFY_ATTEMPTS: usize = 3;

/// Ожидаемый набор после rename: pre-image с одним изменённым именем
/// (id/color/description сохранены) — материал всех verify-сверок.
fn expected_options(pre: &[StatusOption], option_id: &str, to: &str) -> Vec<StatusOption> {
    pre.iter()
        .map(|o| {
            if o.id == option_id {
                StatusOption {
                    name: to.to_string(),
                    ..o.clone()
                }
            } else {
                o.clone()
            }
        })
        .collect()
}

/// Итог одной verify-сверки. Структурная потеря (набор сжался/id пропали/
/// values опустели) — немедленный отказ; старые имена при целом наборе
/// и живых values — read-лаг носителя (минутный масштаб, ADR-113), не отказ.
#[derive(Debug, Clone, PartialEq, Eq)]
enum VerifyOutcome {
    Ok,
    /// Набор цел, но имена ещё старые — носитель лагает.
    Lag,
    Fail(String),
}

/// Чистая сверка одной поверхности (юнит-пины гварда).
fn verify_surface(expected: &[StatusOption], got: &[StatusOption]) -> VerifyOutcome {
    // id-набор цел? (потеря/подмена опций — класс инцидента)
    let missing: Vec<&str> = expected
        .iter()
        .filter(|e| !got.iter().any(|g| g.id == e.id))
        .map(|e| e.name.as_str())
        .collect();
    if !missing.is_empty() {
        return VerifyOutcome::Fail(format!(
            "option(s) lost after mutation: {}",
            missing.join(", ")
        ));
    }
    let extra = got
        .iter()
        .filter(|g| !expected.iter().any(|e| e.id == g.id))
        .map(|g| g.name.as_str())
        .collect::<Vec<_>>();
    if !extra.is_empty() {
        return VerifyOutcome::Fail(format!(
            "unexpected option(s) appeared: {}",
            extra.join(", ")
        ));
    }
    // имена/оформление: цело → Ok; ещё старые → лаг
    let all_match = expected.iter().all(|e| {
        got.iter().any(|g| {
            g.id == e.id && g.name == e.name && g.color == e.color && g.description == e.description
        })
    });
    if all_match {
        VerifyOutcome::Ok
    } else {
        VerifyOutcome::Lag
    }
}

/// Сверка распределения строк: непустота обязательна (класс инцидента —
/// «fieldValues отдали пусто»); значения привязаны к опции носителя, поэтому
/// после rename строка читается новым именем — или старым, если носитель
/// ещё лагает. Ссылка на несуществующую опцию — потеря привязки.
fn verify_distribution(
    from: &str,
    to: &str,
    before: &HashMap<u64, Option<String>>,
    after: &HashMap<u64, Option<String>>,
) -> VerifyOutcome {
    let mut lagged = false;
    for (number, pre) in before {
        let Some(raw) = pre.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) else {
            continue; // пустые до мутации не проверяются
        };
        let Some(post) = after.get(number) else {
            return VerifyOutcome::Fail(format!("row #{number} vanished from the board index"));
        };
        let post_trim = post.as_deref().map(str::trim).unwrap_or_default();
        if post_trim.is_empty() {
            return VerifyOutcome::Fail(format!(
                "row #{number} fieldValues went empty (was «{raw}»)"
            ));
        }
        let fresh_name = if raw == from {
            to.to_string()
        } else {
            raw.to_string()
        };
        if post_trim == fresh_name {
            continue; // носитель уже отдал новое имя (или значение не менялось)
        }
        if post_trim == raw {
            lagged = true; // до-мутационное имя при живой привязке — read-лаг
            continue;
        }
        return VerifyOutcome::Fail(format!(
            "row #{number} points to unknown option «{post_trim}» (was «{raw}»)"
        ));
    }
    if lagged {
        VerifyOutcome::Lag
    } else {
        VerifyOutcome::Ok
    }
}

/// Отчёт успеха: сводка для тула (в журнал уходит полный per-row снимок).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameReport {
    pub board: String,
    pub from: String,
    pub to: String,
    pub option_id: String,
    pub options_total: usize,
    pub rows_total: usize,
    pub histogram_before: BTreeMap<String, usize>,
    pub histogram_after: BTreeMap<String, usize>,
    /// Не-каноническое имя / лаг контрольного чтения — честные пометки.
    pub warnings: Vec<String>,
    pub journal_path: String,
}

pub struct BoardStatusRenameUseCase {
    pub repo: Arc<dyn BoardStatusRepo>,
    pub journal: Arc<dyn StatusPreImageStore>,
    /// карта проект → доска (мультипроектный конфиг).
    pub boards: Vec<(String, String)>,
    pub clock: Arc<dyn Clock>,
}

impl BoardStatusRenameUseCase {
    fn board_of(&self, project: &str) -> UsecaseResult<String> {
        self.boards
            .iter()
            .find(|(p, _)| p == project)
            .map(|(_, b)| b.clone())
            .ok_or_else(|| UsecaseError::Board(format!("project \"{project}\" is not in config")))
    }

    fn unknown_option_error(&self, board: &str, from: &str) -> UsecaseError {
        UsecaseError::Board(format!(
            "Status option \"{from}\" not found on board \"{board}\" — board untouched. \
             If this is a retry after a successful rename, the option is likely already \
             renamed; compare with the pre-image journal: {}",
            self.journal.path_hint()
        ))
    }

    /// Rename одной опции Status одной доски (атомарность на доску;
    /// rename по всем доскам — цикл вызовов агентом).
    pub async fn rename(&self, project: &str, from: &str, to: &str) -> UsecaseResult<RenameReport> {
        let from = from.trim();
        let to = to.trim();
        if from.is_empty() {
            return Err(UsecaseError::Board("from is required".into()));
        }
        if to.is_empty() {
            return Err(UsecaseError::Board("to is required".into()));
        }
        if from == to {
            return Err(UsecaseError::Board(
                "no-op: from and to are the same — destructive transport is not driven \
                 for identity renames"
                    .into(),
            ));
        }
        let board = self.board_of(project)?;

        // 1. Pre-image: свежие пробы носителя (мимо кэшей).
        let image = self.repo.fresh_field(&board).await?;
        let Some(option) = image.options.iter().find(|o| o.name == from) else {
            return Err(self.unknown_option_error(&board, from));
        };
        if image
            .options
            .iter()
            .any(|o| o.name == to && o.id != option.id)
        {
            return Err(UsecaseError::Board(format!(
                "option \"{to}\" already exists on board \"{board}\" — name collision, \
                 board untouched"
            )));
        }
        let distribution = self.repo.fresh_distribution(&board).await?;

        // 2. Write-ahead журнал (ДО мутации) + сводка в лог.
        let pre = StatusPreImage {
            board: board.clone(),
            from: from.to_string(),
            to: to.to_string(),
            created_ms: self.clock.now_ms(),
            options: image.options.clone(),
            distribution: distribution.clone(),
        };
        self.journal.put(&pre)?;
        tracing::info!(
            event = "board_status.pre_image",
            board = %board,
            from = %from,
            to = %to,
            options = image.options.len(),
            rows = distribution.len(),
            journal = %self.journal.path_hint(),
            "pre-image written before mutation"
        );

        // 3. Мутация: дословный набор, одно имя.
        let option_id = option.id.clone();
        let expected = expected_options(&image.options, &option_id, to);
        let payload = self.repo.rename(&image, &option_id, to).await?;

        // 4. Post-verify: payload → контрольное чтение каталога (окно
        // попыток на read-лаг) → распределение значений.
        self.repo.invalidate_meta(&board);
        let payload_verdict = verify_surface(&expected, &payload);
        if let VerifyOutcome::Fail(reason) = payload_verdict {
            return Err(self.verify_failed(&board, reason));
        }
        let mut lagged = false;
        // каталог REST не несёт color/description — сверка по id+name
        // (оформление проверено по payload'у мутации)
        let expected_names_only: Vec<StatusOption> = expected
            .iter()
            .map(|o| StatusOption {
                color: None,
                description: None,
                ..o.clone()
            })
            .collect();
        for attempt in 0..VERIFY_ATTEMPTS {
            if attempt > 0 {
                // пауза между контрольными чтениями (секундный лаг видимости)
                self.clock.sleep_ms(1000);
            }
            let catalog = self.repo.verify_catalog(&board).await?;
            let as_options: Vec<StatusOption> = catalog
                .into_iter()
                .map(|(id, name)| StatusOption {
                    id,
                    name,
                    color: None,
                    description: None,
                })
                .collect();
            match verify_surface(&expected_names_only, &as_options) {
                VerifyOutcome::Ok => break,
                VerifyOutcome::Fail(reason) => return Err(self.verify_failed(&board, reason)),
                VerifyOutcome::Lag if attempt + 1 == VERIFY_ATTEMPTS => {
                    lagged = true;
                }
                VerifyOutcome::Lag => {}
            }
        }
        let dist_after = self.repo.fresh_distribution(&board).await?;
        match verify_distribution(from, to, &distribution, &dist_after) {
            VerifyOutcome::Ok => {}
            VerifyOutcome::Fail(reason) => return Err(self.verify_failed(&board, reason)),
            // старые имена при живой привязке — read-лаг дистрибуции
            VerifyOutcome::Lag => lagged = true,
        }

        // 5. Отчёт (warn: не-каноническое имя; лаг контрольного чтения).
        let mut warnings = Vec::new();
        // warn только на не-парсящееся имя: строки выпадут из рабочих
        // списков сервера; канон→канон (и даже другой канон) — валидный
        // rename класса #1441, его помечать нечем
        if parse_status(to) == uwuwu_domain::Status::Empty {
            warnings.push(format!(
                "new name \"{to}\" is not a canonical board status — rows with it \
                 drop out of pipeline working lists"
            ));
        }
        if lagged {
            warnings.push(format!(
                "carrier read lagged (catalog and/or row values still show old names \
                 after {VERIFY_ATTEMPTS} checks); structural checks passed (payload \
                 id-set + non-empty values)"
            ));
        }
        Ok(RenameReport {
            board,
            from: from.to_string(),
            to: to.to_string(),
            option_id,
            options_total: expected.len(),
            rows_total: distribution.len(),
            histogram_before: histogram(&image.options, &distribution),
            histogram_after: histogram(&expected, &dist_after),
            warnings,
            journal_path: self.journal.path_hint(),
        })
    }

    fn verify_failed(&self, board: &str, reason: String) -> UsecaseError {
        UsecaseError::Board(format!(
            "POST-VERIFY FAILED on board \"{board}\": {reason}. Possible option loss — \
             compare the carrier with the pre-image journal BEFORE manual rollback: {}",
            self.journal.path_hint()
        ))
    }
}

/// Гистограмма распределения строк по опциям каталога (BTreeMap —
/// детерминированный порядок в отчёте; неизвестные имена — под «(unknown)»).
fn histogram(
    options: &[StatusOption],
    distribution: &HashMap<u64, Option<String>>,
) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> =
        options.iter().map(|o| (o.name.clone(), 0usize)).collect();
    for raw in distribution.values() {
        let Some(name) = raw.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let key = if options.iter().any(|o| o.name == name) {
            name.to_string()
        } else {
            "(unknown)".to_string()
        };
        *out.entry(key).or_insert(0) += 1;
    }
    out
}

#[cfg(test)]
mod board_status_tests {
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
                project_node_id: "PVT_test".into(),
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
}
