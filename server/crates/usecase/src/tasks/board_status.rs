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
    /// rename по всем доскам — цикл вызовов агентом). Фазы: валидация
    /// входа → pre-image (журнал до мутации) → дословная мутация →
    /// post-verify → отчёт.
    pub async fn rename(&self, project: &str, from: &str, to: &str) -> UsecaseResult<RenameReport> {
        let (from, to) = validate_names(from, to)?;
        let (from, to) = (from.as_str(), to.as_str());
        let board = self.board_of(project)?;
        let (image, option, distribution) = self.pre_image(&board, from, to).await?;
        // 3. Мутация: дословный набор, одно имя.
        let option_id = option.id.clone();
        let expected = expected_options(&image.options, &option_id, to);
        let payload = self.repo.rename(&image, &option_id, to).await?;
        // 4. Post-verify (мимо кэшей): payload → каталог (окно попыток на
        // read-лаг) → непустота fieldValues.
        self.repo.invalidate_meta(&board);
        let (dist_after, lagged) = self
            .verify_fresh(&board, from, to, &expected, &payload, &distribution)
            .await?;
        Ok(self.report(
            &board,
            &image.options,
            &expected,
            from,
            to,
            option_id,
            distribution,
            &dist_after,
            lagged,
        ))
    }

    /// Pre-image: свежие пробы носителя (мимо кэшей) + write-ahead журнал
    /// (ДО мутации) + сводка в лог.
    async fn pre_image(
        &self,
        board: &str,
        from: &str,
        to: &str,
    ) -> UsecaseResult<(StatusFieldImage, StatusOption, HashMap<u64, Option<String>>)> {
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
        Ok((image, option, distribution))
    }

    /// Post-verify: payload мутации → контрольные чтения каталога
    /// (VERIFY_ATTEMPTS на read-лаг носителя, ADR-113) → распределение
    /// значений. Возврат — after-дистрибуция + флаг лага (для warn).
    async fn verify_fresh(
        &self,
        board: &str,
        from: &str,
        to: &str,
        expected: &[StatusOption],
        payload: &[StatusOption],
        before: &HashMap<u64, Option<String>>,
    ) -> UsecaseResult<(HashMap<u64, Option<String>>, bool)> {
        if let VerifyOutcome::Fail(reason) = verify_surface(expected, payload) {
            return Err(self.verify_failed(board, reason));
        }
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
        let mut lagged = false;
        for attempt in 0..VERIFY_ATTEMPTS {
            if attempt > 0 {
                // пауза между контрольными чтениями (секундный лаг видимости)
                self.clock.sleep_ms(1000);
            }
            let catalog = self.repo.verify_catalog(board).await?;
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
                VerifyOutcome::Fail(reason) => return Err(self.verify_failed(board, reason)),
                VerifyOutcome::Lag if attempt + 1 == VERIFY_ATTEMPTS => {
                    lagged = true;
                }
                VerifyOutcome::Lag => {}
            }
        }
        let dist_after = self.repo.fresh_distribution(board).await?;
        match verify_distribution(from, to, before, &dist_after) {
            VerifyOutcome::Ok => {}
            VerifyOutcome::Fail(reason) => return Err(self.verify_failed(board, reason)),
            // старые имена при живой привязке — read-лаг дистрибуции
            VerifyOutcome::Lag => lagged = true,
        }
        Ok((dist_after, lagged))
    }

    /// Отчёт успеха (warn: не-каноническое имя; лаг контрольного чтения).
    #[allow(clippy::too_many_arguments)]
    fn report(
        &self,
        board: &str,
        options_before: &[StatusOption],
        expected: &[StatusOption],
        from: &str,
        to: &str,
        option_id: String,
        distribution: HashMap<u64, Option<String>>,
        dist_after: &HashMap<u64, Option<String>>,
        lagged: bool,
    ) -> RenameReport {
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
        RenameReport {
            board: board.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            option_id,
            options_total: expected.len(),
            rows_total: distribution.len(),
            histogram_before: histogram(options_before, &distribution),
            histogram_after: histogram(expected, dist_after),
            warnings,
            journal_path: self.journal.path_hint(),
        }
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
mod board_status_tests;
