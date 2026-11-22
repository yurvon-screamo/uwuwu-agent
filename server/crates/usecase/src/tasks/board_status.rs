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

/// Свежий каталог Status-поля (pre-image опций): node-id поля (мутация
/// ищет поле по fieldId напрямую — живая проба #1447) + полный набор опций.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusFieldImage {
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

/// Валидация входа (отказы до любых чтений носителя): имена обязательны,
/// no-op по деструктивному транспорту не гоняем.
fn validate_names(from: &str, to: &str) -> UsecaseResult<(String, String)> {
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
    Ok((from.to_string(), to.to_string()))
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

/// Чистая сверка одной поверхности (юнит-пины гварда). Дискриминация по
/// тройке (pre, expected, got): набор по id цел; имя опции — ожидаемое
/// новое (Ok-фактор), исходное старое (Lag-фактор: read-лаг носителя) или
/// третье/искажённое (Fail: конкурентный писатель или подмена). Оформление
/// (color/description) сверяется только там, где поверхность их несёт
/// (payload мутации; REST-каталог отдаёт id+name — expected передаётся
/// names-only, pre тоже).
fn verify_surface(
    pre: &[StatusOption],
    expected: &[StatusOption],
    got: &[StatusOption],
) -> VerifyOutcome {
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
    // per-option: новое имя → Ok-фактор; старое (pre-image) → Lag-фактор;
    // третье/искажённое → немедленный Fail. Оформление — exact-match.
    let mut lagged = false;
    for e in expected {
        let g = got
            .iter()
            .find(|g| g.id == e.id)
            .expect("id присутствует — проверено выше");
        if g.name != e.name {
            let pre_name = pre
                .iter()
                .find(|p| p.id == e.id)
                .map(|p| p.name.as_str())
                .unwrap_or_default();
            if g.name != pre_name {
                return VerifyOutcome::Fail(format!(
                    "option \"{}\" reports unexpected name \"{}\" (expected \"{}\", \
                     was \"{}\") — concurrent writer or corrupted carrier response",
                    e.name, g.name, e.name, pre_name
                ));
            }
            lagged = true;
        }
        // оформление: сравнение только там, где pre-image его знал
        // (None не выдумывали и не сравниваем; известное — exact-match)
        if (e.color.is_some() && g.color != e.color)
            || (e.description.is_some() && g.description != e.description)
        {
            return VerifyOutcome::Fail(format!(
                "option \"{}\" lost its styling in carrier response (color/description \
                 changed) — payload does not confirm the new state",
                e.name
            ));
        }
    }
    if lagged {
        VerifyOutcome::Lag
    } else {
        VerifyOutcome::Ok
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
    /// rename по всем доскам — цикл вызовов агентом). Фазы: валидация
    /// входа → pre-image (журнал до мутации) → дословная мутация →
    /// post-verify → отчёт. Контекст фаз — RenameFlow (одна связка).
    pub async fn rename(&self, project: &str, from: &str, to: &str) -> UsecaseResult<RenameReport> {
        let (from, to) = validate_names(from, to)?;
        let board = self.board_of(project)?;
        let flow = self.pre_image(&board, &from, &to).await?;
        // Мутация: дословный набор, одно имя. Отказ носителя (в т.ч.
        // транспортный/строгий parse payload) мог оставить мутацию
        // применённой — текст обязан звать к pre-image журналу.
        let expected = expected_options(&flow.image.options, &flow.option_id, &to);
        let payload = self
            .repo
            .rename(&flow.image, &flow.option_id, &to)
            .await
            .map_err(|e| {
                UsecaseError::Board(format!(
                    "RENAME MUTATION FAILED on board \"{}\": {e}. If the mutation \
                     actually applied, compare the carrier with the pre-image journal \
                     BEFORE any rollback: {}",
                    flow.board,
                    self.journal.path_hint()
                ))
            })?;
        // Post-verify (мимо кэшей): payload → каталог (окно попыток на
        // read-лаг) → непустота fieldValues.
        self.repo.invalidate_meta(&board);
        let (dist_after, lagged) = self.verify_fresh(&flow, &expected, &payload).await?;
        Ok(flow.report(self, &expected, dist_after, lagged))
    }

    /// Pre-image: свежие пробы носителя (мимо кэшей) + write-ahead журнал
    /// (ДО мутации) + сводка в лог. Возврат — контекст фаз RenameFlow.
    async fn pre_image(&self, board: &str, from: &str, to: &str) -> UsecaseResult<RenameFlow> {
        let image = self.repo.fresh_field(board).await?;
        let option = image
            .options
            .iter()
            .find(|o| o.name == from)
            .cloned()
            .ok_or_else(|| self.unknown_option_error(board, from))?;
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
        let distribution = self.repo.fresh_distribution(board).await?;
        let pre = StatusPreImage {
            board: board.to_string(),
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
        Ok(RenameFlow {
            board: board.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            image,
            option_id: option.id,
            distribution,
        })
    }

    /// Post-verify: payload мутации → контрольные чтения каталога
    /// (VERIFY_ATTEMPTS на read-лаг носителя, ADR-113) → распределение
    /// значений. Возврат — after-дистрибуция + флаг лага (для warn).
    async fn verify_fresh(
        &self,
        flow: &RenameFlow,
        expected: &[StatusOption],
        payload: &[StatusOption],
    ) -> UsecaseResult<(HashMap<u64, Option<String>>, bool)> {
        // payload: носитель обязан подтвердить РОВНО новое состояние —
        // старые имена/искажения здесь не лаг (ответ post-write), а
        // ненадёжное подтверждение = громкий отказ (НФТ: молчаливая
        // потеря запрещена); оформление payload единственный носит.
        match verify_surface(&flow.image.options, expected, payload) {
            VerifyOutcome::Ok => {}
            VerifyOutcome::Fail(reason) => return Err(self.verify_failed(&flow.board, reason)),
            VerifyOutcome::Lag => {
                return Err(self.verify_failed(
                    &flow.board,
                    "mutation payload does not reflect the new state (old/mismatched \
                     names in carrier response)"
                        .into(),
                ))
            }
        }
        // каталог REST не несет color/description — сверка по id+name
        // против pre-image (лаг = старое имя; третье имя = отказ)
        let pre_names_only: Vec<StatusOption> = flow
            .image
            .options
            .iter()
            .map(|o| StatusOption {
                color: None,
                description: None,
                ..o.clone()
            })
            .collect();
        let expected_names_only: Vec<StatusOption> = expected
            .iter()
            .map(|o| StatusOption {
                color: None,
                description: None,
                ..o.clone()
            })
            .collect();
        let mut lagged = false;
        for attempt in 0..VERIFY_ATTEMPTS {
            if attempt > 0 {
                // пауза между контрольными чтениями (секундный лаг видимости)
                self.clock.sleep_ms(1000);
            }
            // контрольное чтение после УЖЕ применённой мутации: транспортный
            // отказ не «ничего не произошло» — рамка журнала обязательна
            // (ревью #1447 раунд 2 Low-1)
            let catalog = self
                .repo
                .verify_catalog(&flow.board)
                .await
                .map_err(|e| self.post_mutation_read_failed(&flow.board, e))?;
            let as_options: Vec<StatusOption> = catalog
                .into_iter()
                .map(|(id, name)| StatusOption {
                    id,
                    name,
                    color: None,
                    description: None,
                })
                .collect();
            match verify_surface(&pre_names_only, &expected_names_only, &as_options) {
                VerifyOutcome::Ok => break,
                VerifyOutcome::Fail(reason) => return Err(self.verify_failed(&flow.board, reason)),
                VerifyOutcome::Lag if attempt + 1 == VERIFY_ATTEMPTS => {
                    lagged = true;
                }
                VerifyOutcome::Lag => {}
            }
        }
        let dist_after = self
            .repo
            .fresh_distribution(&flow.board)
            .await
            .map_err(|e| self.post_mutation_read_failed(&flow.board, e))?;
        match verify_distribution(&flow.from, &flow.to, &flow.distribution, &dist_after) {
            VerifyOutcome::Ok => {}
            VerifyOutcome::Fail(reason) => return Err(self.verify_failed(&flow.board, reason)),
            // старые имена при живой привязке — read-лаг дистрибуции
            VerifyOutcome::Lag => lagged = true,
        }
        Ok((dist_after, lagged))
    }

    fn verify_failed(&self, board: &str, reason: String) -> UsecaseError {
        UsecaseError::Board(format!(
            "POST-VERIFY FAILED on board \"{board}\": {reason}. Possible option loss — \
             compare the carrier with the pre-image journal BEFORE manual rollback: {}",
            self.journal.path_hint()
        ))
    }

    /// Отказ контрольного чтения ПОСЛЕ применённой мутации: рамка «rename
    /// уже применён и подтверждён payload» — голый HTTP-ошибку агент может
    /// прочитать как «ничего не произошло» и пойти на слепой повтор.
    fn post_mutation_read_failed(&self, board: &str, e: UsecaseError) -> UsecaseError {
        UsecaseError::Board(format!(
            "POST-MUTATION CARRIER READ FAILED on board \"{board}\": {e}. The rename \
             was already applied and payload-confirmed; the board may differ from the \
             pre-image — compare with the journal BEFORE any action: {}",
            self.journal.path_hint()
        ))
    }
}

/// Контекст фаз одной rename-операции (доска, имена, pre-image образ):
/// живёт от pre-image до отчёта, фазы принимают связку вместо россыпи.
struct RenameFlow {
    board: String,
    from: String,
    to: String,
    image: StatusFieldImage,
    option_id: String,
    distribution: HashMap<u64, Option<String>>,
}

impl RenameFlow {
    /// Отчёт успеха (warn: не-каноническое имя; лаг контрольного чтения).
    fn report(
        &self,
        usecase: &BoardStatusRenameUseCase,
        expected: &[StatusOption],
        dist_after: HashMap<u64, Option<String>>,
        lagged: bool,
    ) -> RenameReport {
        let mut warnings = Vec::new();
        // warn только на не-парсящееся имя: строки выпадут из рабочих
        // списков сервера; канон→канон (и даже другой канон) — валидный
        // rename класса #1441, его помечать нечем
        if parse_status(&self.to) == uwuwu_domain::Status::Empty {
            warnings.push(format!(
                "new name \"{}\" is not a canonical board status — rows with it \
                 drop out of pipeline working lists",
                self.to
            ));
        }
        if lagged {
            warnings.push(format!(
                "carrier read lagged (catalog and/or row values still show old names \
                 after {VERIFY_ATTEMPTS} checks); structural checks passed (payload \
                 id-set + non-empty values)"
            ));
        }
        RenameReport {
            board: self.board.clone(),
            from: self.from.clone(),
            to: self.to.clone(),
            option_id: self.option_id.clone(),
            options_total: expected.len(),
            rows_total: self.distribution.len(),
            histogram_before: histogram(&self.image.options, &self.distribution),
            histogram_after: histogram(expected, &dist_after),
            warnings,
            journal_path: usecase.journal.path_hint(),
        }
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
mod board_status_tests;
