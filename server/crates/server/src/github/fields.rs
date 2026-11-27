//! Нативные поля issue (fm-зеркало снесено решением владельца 15.11):
//! сессии/ветка/PR/тип/приоритет живут в глобальных полях issue.
//! Контракт: каталог GET /user/issue-fields (id по имени), чтение —
//! issue.issue_field_values (inline в GET issue), запись — батч POST,
//! очистка — DELETE (404 = no-op). Живые пробы: старый cli (#1092).

use super::HTTP_TIMEOUT;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use uwuwu_usecase::IssueFieldStore;
use uwuwu_usecase::{UsecaseError, UsecaseResult};

pub const F_DEV_SESSION: &str = "Сессия dev";
/// Legacy-поле, покрывавшее все три ревью-слота (#1710). Только чтение
/// когорты с пустым новым план-полем; новой записи не получает.
pub const F_REVIEW_SESSION: &str = "Сессия ревью";
pub const F_PLAN_REVIEW_SESSION: &str = "Сессия план-ревью";
pub const F_RESULT_REVIEW_SESSION: &str = "Сессия ревью кода";
pub const F_TEST_REVIEW_SESSION: &str = "Сессия ревью тестов";
pub const F_TEST_SESSION: &str = "Сессия тест";

/// Единственный источник имени поля сессии слота: три ревью-стадии —
/// три независимых ячейки (канон #1626); дев-семья и тестер — свои поля.
pub fn review_session_field(slot: uwuwu_domain::Slot) -> &'static str {
    use uwuwu_domain::Slot;
    match slot {
        Slot::Dev | Slot::Design => F_DEV_SESSION,
        Slot::PlanReview => F_PLAN_REVIEW_SESSION,
        Slot::ResultReview => F_RESULT_REVIEW_SESSION,
        Slot::TestReview => F_TEST_REVIEW_SESSION,
        Slot::Test => F_TEST_SESSION,
    }
}

/// Патч записи сессии: эксклюзивное поле слота плюс условная очистка legacy
/// (только для ревью-слотов и только если legacy-поле есть в каталоге).
pub fn review_store_patch(
    slot: uwuwu_domain::Slot,
    session: &str,
    legacy_present: bool,
) -> Vec<(&'static str, Option<String>)> {
    let mut patch = vec![(review_session_field(slot), Some(session.to_string()))];
    if slot.is_review() && legacy_present {
        patch.push((F_REVIEW_SESSION, None));
    }
    patch
}
/// Ветка задачи (фабрика пишет при спавне; PR-рид гейтится ею — hub.rs).
pub const F_BRANCH: &str = "Branch";
/// Единое имя поля приоритета (пишется create-ом, читается enrichment-ом):
/// источник — naming (usecase), здесь алиас для локальной конвенции.
pub const F_PRIORITY: &str = uwuwu_usecase::F_PRIORITY;

/// Карта значений полей задачи (непустые).
pub type Fields = HashMap<String, String>;

/// Резолв имени в каталоге с legacy-алиасом автомержа (#1232): пишем
/// «Automerge», при отсутствии в каталоге — legacy-поле «Autodone»
/// (id 49841025, опции off/on); оба отсутствуют → None (ensure_field
/// создаст «Automerge» single_select off/on).
fn resolve_field(catalog: &HashMap<String, u64>, name: &str) -> Option<u64> {
    if let Some(f) = catalog.get(name) {
        return Some(*f);
    }
    if name == uwuwu_usecase::F_AUTOMERGE {
        return catalog.get("Autodone").copied();
    }
    // легаси-лейбл приоритета (#1383): физическое поле «Приоритет» живёт
    // в каталогах до rename — канон "Priority" резолвится в него
    if name == uwuwu_usecase::F_PRIORITY {
        return catalog.get("Приоритет").copied();
    }
    None
}

pub struct IssueFields {
    repo: String,
    token: String,
    http: reqwest::Client,
    catalog: Mutex<Option<HashMap<String, u64>>>,
}

impl IssueFields {
    pub fn new(repo: String, token: String) -> Self {
        Self {
            repo,
            token,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .timeout(HTTP_TIMEOUT)
                .build()
                .expect("reqwest client"),
            catalog: Mutex::new(None),
        }
    }

    async fn rest(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> UsecaseResult<(reqwest::StatusCode, Value)> {
        let mut req = self
            .http
            .request(method, format!("https://api.github.com{path}"))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json");
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| UsecaseError::Board(format!("fields transport {path}: {e}")))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Board(format!("fields body {path}: {e}")))?;
        if let Some(fail) = crate::gherr::classify(status, &headers) {
            return Err(UsecaseError::Board(format!("fields {path}: {fail}")));
        }
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        Ok((status, value))
    }

    /// Каталог полей (id по имени), кэш на процесс.
    async fn catalog(&self) -> UsecaseResult<HashMap<String, u64>> {
        {
            let guard = self.catalog.lock().expect("catalog lock");
            if let Some(c) = guard.as_ref() {
                return Ok(c.clone());
            }
        }
        let (status, body) = self
            .rest(reqwest::Method::GET, "/user/issue-fields", None)
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!("field catalog: HTTP {status}")));
        }
        let mut map = HashMap::new();
        if let Some(list) = body.as_array() {
            for f in list {
                if let (Some(id), Some(name)) = (f["id"].as_u64(), f["name"].as_str()) {
                    map.insert(name.to_string(), id);
                }
            }
        }
        *self.catalog.lock().expect("catalog lock") = Some(map.clone());
        Ok(map)
    }

    /// Чтение полей задачи из GET issue (issue_field_values inline).
    pub async fn load(&self, number: u64) -> UsecaseResult<Fields> {
        let (status, body) = self
            .rest(
                reqwest::Method::GET,
                &format!("/repos/{}/issues/{number}", self.repo),
                None,
            )
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "fields load #{number}: HTTP {status}"
            )));
        }
        Ok(parse_field_values(&body))
    }

    /// Батч-запись: Some — значение, None — очистка (идемпотентно).
    pub async fn store(&self, number: u64, patch: &[(&str, Option<String>)]) -> UsecaseResult<()> {
        let mut catalog = self.catalog().await?;
        let mut batch = Vec::new();
        for (name, value) in patch {
            // Очистка отсутствующего поля — no-op: не создаём заново поле,
            // которое владелец снёс (класс #1271; #1710 legacy-ветка).
            if value.as_deref().is_none_or(str::is_empty) && resolve_field(&catalog, name).is_none()
            {
                continue;
            }
            // self-heal: отсутствующее поле создаём (инцидент #1149 —
            // create задачи падал целиком из-за неготового «Тип»)
            let fid = match resolve_field(&catalog, name) {
                Some(f) => f,
                None => {
                    self.ensure_field(name).await?;
                    catalog = self.catalog().await?;
                    resolve_field(&catalog, name).ok_or_else(|| {
                        UsecaseError::Board(format!("the global field «{name}» was not found"))
                    })?
                }
            };
            match value {
                Some(v) if !v.is_empty() => batch.push(json!({ "field_id": fid, "value": v })),
                _ => {
                    // очистка: 404 (значения не было) — no-op
                    let (st, _) = self
                        .rest(
                            reqwest::Method::DELETE,
                            &format!(
                                "/repos/{}/issues/{number}/issue-field-values/{fid}",
                                self.repo
                            ),
                            None,
                        )
                        .await?;
                    if !st.is_success() && st.as_u16() != 404 {
                        return Err(UsecaseError::Board(format!(
                            "fields clear #{number}:{name}: HTTP {st}"
                        )));
                    }
                }
            }
        }
        if batch.is_empty() {
            return Ok(());
        }
        let (status, _) = self
            .rest(
                reqwest::Method::POST,
                &format!("/repos/{}/issues/{number}/issue-field-values", self.repo),
                Some(json!({ "issue_field_values": batch })),
            )
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "fields store #{number}: HTTP {status}"
            )));
        }
        Ok(())
    }

    /// Признак поля в каталоге без создания (Sources: #1710 legacy-ветка):
    /// cleared-патч не должен пересоздавать снесённое владельцем поле.
    pub(crate) async fn has_field(&self, name: &str) -> UsecaseResult<bool> {
        Ok(self.catalog().await?.contains_key(name))
    }

    /// Создать глобальное поле, если его нет (каталог дрогнуть).
    pub async fn ensure_field(&self, name: &str) -> UsecaseResult<()> {
        let catalog = self.catalog().await?;
        if resolve_field(&catalog, name).is_some() {
            return Ok(());
        }
        let body = ensure_field_body(name);
        let (status, resp) = self
            .rest(reqwest::Method::POST, "/user/issue-fields", Some(body))
            .await?;
        if status.as_u16() == 422 {
            // 422 различается честно: «already exists» — гонка ensure'ов
            // (параллельные create'ы, ADR-057-семейство), идемпотентно ok;
            // прочий 422 — ошибка формы тела, раньше маскировалась любым
            // 422 (баг-репорт #1357 круг 1, п.2) — громкий отказ с телом
            if !field_422_already_exists(&resp) {
                return Err(UsecaseError::Board(format!(
                    "field creation «{name}»: HTTP 422 — {resp}"
                )));
            }
        } else if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "field creation «{name}»: HTTP {status}"
            )));
        }
        *self.catalog.lock().expect("catalog lock") = None;
        Ok(())
    }
}

/// Тело POST /user/issue-fields по имени поля. Опции single_select несут
/// priority: без него GitHub отвечает 422 на свежей среде (баг-репорт
/// #1357 круг 1). «Тип» снят (#1271): носитель типа — нативный issue Type,
/// ensure-минт пересоздал бы снесённое поле; автомерж (#1232) —
/// single_select off/on; direct (#1357) — тот же паттерн; прочие имена — текст.
fn ensure_field_body(name: &str) -> Value {
    let on_off = |on_color: &str| {
        json!({
            "options": [
                { "name": "off", "color": "gray", "priority": 1 },
                { "name": "on", "color": on_color, "priority": 2 },
            ]
        })
    };
    match name {
        "Automerge" | "Autodone" => {
            let mut body = on_off("green");
            body["name"] = json!(name);
            body["data_type"] = json!("single_select");
            body
        }
        // direct-класс (#1357): тот же носитель-паттерн, legacy-алиаса нет
        uwuwu_usecase::F_DIRECT => {
            let mut body = on_off("orange");
            body["name"] = json!(name);
            body["data_type"] = json!("single_select");
            body
        }
        // fact_track-класс (#1631): тот же паттерн, legacy-алиаса нет
        uwuwu_usecase::F_FACT_TRACK => {
            let mut body = on_off("blue");
            body["name"] = json!(name);
            body["data_type"] = json!("single_select");
            body
        }
        _ => json!({ "name": name, "data_type": "text" }),
    }
}

/// Детектор «422 = поле уже есть» (гонка параллельных ensure'ов,
/// ADR-057-семейство): GitHub отдаёт errors[].code с already_exists
/// префиксом. Прочий 422 — ошибка формы тела, не маскируется
/// (баг-репорт #1357 круг 1, п.2).
fn field_422_already_exists(resp: &Value) -> bool {
    let s = resp.to_string();
    s.contains("already exists") || s.contains("already_exists")
}

/// issue_field_values → карта (name → value; select — имя опции).
pub fn parse_field_values(issue: &Value) -> Fields {
    let mut out = Fields::new();
    if let Some(list) = issue["issue_field_values"].as_array() {
        for f in list {
            let mut name = f["issue_field_name"].as_str().unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            // Legacy-нормализация на границе носителя (#1280): resolve_field
            // пишет автомерж в legacy-поле «Autodone» (каталог «Automerge»
            // не содержит), читатели получают новое имя — потребители
            // fields.load не обязаны помнить алиас (док F_AUTOMERGE:
            // «сервер читает/пишет оба имени» — резолв здесь, как в store).
            if name == "Autodone" {
                name = uwuwu_usecase::F_AUTOMERGE;
            }
            // та же нормализация для приоритета (#1383): физический лейбл
            // «Приоритет» читается каноном "Priority" — потребители load
            // алиас не помнят (док F_PRIORITY: наружу всегда новое)
            if name == "Приоритет" {
                name = uwuwu_usecase::F_PRIORITY;
            }
            let value = f["single_select_option"]["name"]
                .as_str()
                .or(f["value"].as_str())
                .unwrap_or_default();
            if !value.is_empty() {
                out.entry(name.to_string()).or_insert(value.to_string());
            }
        }
    }
    out
}

#[async_trait]
impl IssueFieldStore for IssueFields {
    async fn load(&self, number: u64) -> UsecaseResult<Fields> {
        IssueFields::load(self, number).await
    }

    async fn store(&self, number: u64, patch: &[(&str, Option<String>)]) -> UsecaseResult<()> {
        IssueFields::store(self, number, patch).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ensure-боди несёт priority в каждой опции: GitHub 422 без него
    /// на свежей среде (баг-репорт #1357 круг 1; семья — Automerge и Direct).
    #[test]
    fn ensure_field_body_options_carry_priority() {
        for name in [
            "Automerge",
            "Autodone",
            uwuwu_usecase::F_DIRECT,
            uwuwu_usecase::F_FACT_TRACK,
        ] {
            let body = ensure_field_body(name);
            assert_eq!(body["name"], *name, "{name}: имя поля в теле");
            assert_eq!(body["data_type"], "single_select");
            let options = body["options"].as_array().expect("options");
            assert_eq!(options.len(), 2, "{name}: off/on");
            for (i, opt) in options.iter().enumerate() {
                assert_eq!(
                    opt["priority"].as_u64(),
                    Some((i + 1) as u64),
                    "{name}: опция {} без priority (422 на свежей среде)",
                    opt["name"]
                );
            }
        }
        // «Тип» снят с ensure (#1271): тип живёт в нативном issue Type,
        // ensure-минт пересоздал бы снесённое владельцем поле (self-heal
        // store) — arm удалён, имя уходит в текстовую ветку как прочий мусор
        let legacy_kind = ensure_field_body("Тип");
        assert_eq!(legacy_kind["data_type"], "text");
        // прочие имена — текст (без options)
        let text = ensure_field_body("Сессия dev");
        assert_eq!(text["data_type"], "text");
    }

    /// 422 различается честно: already exists — идемпотентный ok,
    /// прочий 422 (ошибка формы) — громкий отказ, не маскировка
    /// (баг-репорт #1357 круг 1, п.2).
    #[test]
    fn ensure_field_422_already_exists_is_tolerated() {
        let already = json!({
            "message": "Validation Failed",
            "errors": [{ "resource": "IssueField", "code": "already_exists_at_user" }]
        });
        assert!(field_422_already_exists(&already));
        let already_verbose = json!({ "message": "name already exists on this account" });
        assert!(field_422_already_exists(&already_verbose));
        let other = json!({ "message": "Validation Failed: priority is invalid" });
        assert!(
            !field_422_already_exists(&other),
            "не-already 422 не маскируется: {other}"
        );
    }

    #[test]
    fn review_session_field_is_exclusive_per_slot() {
        use uwuwu_domain::Slot;
        let names: Vec<_> = [
            Slot::Dev,
            Slot::Design,
            Slot::PlanReview,
            Slot::ResultReview,
            Slot::TestReview,
            Slot::Test,
        ]
        .into_iter()
        .map(review_session_field)
        .collect();
        assert_eq!(
            names,
            vec![
                F_DEV_SESSION,
                F_DEV_SESSION,
                F_PLAN_REVIEW_SESSION,
                F_RESULT_REVIEW_SESSION,
                F_TEST_REVIEW_SESSION,
                F_TEST_SESSION,
            ]
        );
    }

    #[test]
    fn review_store_patch_clears_legacy_only_for_review() {
        use uwuwu_domain::Slot;
        let patch = review_store_patch(Slot::ResultReview, "ses_result", true);
        assert_eq!(
            patch,
            vec![
                (F_RESULT_REVIEW_SESSION, Some("ses_result".into())),
                (F_REVIEW_SESSION, None),
            ]
        );
        let patch = review_store_patch(Slot::Dev, "ses_dev", true);
        assert_eq!(patch, vec![(F_DEV_SESSION, Some("ses_dev".into()))]);
        let patch = review_store_patch(Slot::PlanReview, "ses_plan", false);
        assert_eq!(
            patch,
            vec![(F_PLAN_REVIEW_SESSION, Some("ses_plan".into()))]
        );
    }

    /// REST-путь чтения direct (#1357, тестировщик): issue_field_values
    /// с Direct читается в карту, read_issue поверх мапит «on» → флаг.
    #[test]
    fn parse_field_values_direct_on_off() {
        let issue = json!({
            "issue_field_values": [
                { "issue_field_name": "Direct", "single_select_option": { "name": "on" } },
            ]
        });
        let f = parse_field_values(&issue);
        assert_eq!(
            f.get(uwuwu_usecase::F_DIRECT).map(String::as_str),
            Some("on")
        );
        let issue_off = json!({
            "issue_field_values": [
                { "issue_field_name": "Direct", "single_select_option": { "name": "off" } },
            ]
        });
        let f = parse_field_values(&issue_off);
        assert_eq!(
            f.get(uwuwu_usecase::F_DIRECT).map(String::as_str),
            Some("off")
        );
        // отсутствующее поле — ключа нет (внутри read_issue → direct=false)
        let bare = parse_field_values(&json!({ "issue_field_values": [] }));
        assert!(!bare.contains_key(uwuwu_usecase::F_DIRECT));
    }

    #[test]
    fn parses_inline_values() {
        let issue = json!({
            "issue_field_values": [
                { "issue_field_name": "Branch", "value": "work/task-9" },
                { "issue_field_name": "Сессия dev", "value": "ses_a" },
                { "issue_field_name": "Приоритет", "single_select_option": { "name": "high" } },
                { "issue_field_name": "PR", "value": "" },
            ]
        });
        let f = parse_field_values(&issue);
        assert_eq!(f.get("Branch").map(String::as_str), Some("work/task-9"));
        assert_eq!(f.get("Сессия dev").map(String::as_str), Some("ses_a"));
        assert_eq!(
            f.get("Priority").map(String::as_str),
            Some("high"),
            "легаси-лейбл «Приоритет» нормализован на границе (#1383)"
        );
        assert!(!f.contains_key("Приоритет"), "legacy-ключ в карте");
        assert!(!f.contains_key("PR"), "пустые значения не в карте");
    }

    #[test]
    fn parse_normalizes_legacy_autodone() {
        // живой каталог: физическое имя legacy «Autodone» (#1280 ревью) —
        // наружу нормализованное «Automerge», legacy-ключ не просачивается
        let issue = json!({
            "issue_field_values": [
                { "issue_field_name": "Autodone", "single_select_option": { "name": "on" } },
                { "issue_field_name": "Приоритет", "single_select_option": { "name": "high" } },
            ]
        });
        let f = parse_field_values(&issue);
        assert_eq!(f.get("Automerge").map(String::as_str), Some("on"));
        assert!(!f.contains_key("Autodone"), "legacy-ключ в карте");
    }

    #[test]
    fn parse_keeps_live_automerge_name() {
        // если каталог пополнился новым именем — оно не перетирается
        let issue = json!({
            "issue_field_values": [
                { "issue_field_name": "Automerge", "single_select_option": { "name": "off" } },
            ]
        });
        let f = parse_field_values(&issue);
        assert_eq!(f.get("Automerge").map(String::as_str), Some("off"));
    }

    #[test]
    fn resolve_field_prefers_new_name() {
        let mut catalog = HashMap::new();
        catalog.insert("Automerge".to_string(), 1);
        catalog.insert("Autodone".to_string(), 2);
        assert_eq!(
            resolve_field(&catalog, "Automerge"),
            Some(1),
            "новое имя бьёт legacy-алиас"
        );
    }

    #[test]
    fn resolve_field_falls_back_to_legacy_autodone() {
        // живой каталог 17.11: Autodone id 49841025, опции off/on; поля
        // Automerge в каталоге нет — значение приземляется в legacy-поле
        let mut catalog = HashMap::new();
        catalog.insert("Autodone".to_string(), 49841025);
        assert_eq!(resolve_field(&catalog, "Automerge"), Some(49841025));
    }

    #[test]
    fn resolve_field_priority_prefers_canon() {
        let mut catalog = HashMap::new();
        catalog.insert("Priority".to_string(), 1);
        catalog.insert("Приоритет".to_string(), 2);
        assert_eq!(
            resolve_field(&catalog, "Priority"),
            Some(1),
            "канон бьёт legacy-лейбл"
        );
    }

    #[test]
    fn resolve_field_priority_falls_back_to_legacy_label() {
        // живой каталог: физическое поле «Приоритет» до rename (#1383) —
        // запись каноном "Priority" резолвится в легаси-лейбл
        let mut catalog = HashMap::new();
        catalog.insert("Приоритет".to_string(), 49841008);
        assert_eq!(resolve_field(&catalog, "Priority"), Some(49841008));
    }

    #[test]
    fn resolve_field_misses_yield_none() {
        let empty: HashMap<String, u64> = HashMap::new();
        assert_eq!(resolve_field(&empty, "Automerge"), None, "ensure_field");
        assert_eq!(resolve_field(&empty, "Priority"), None);
        assert_eq!(resolve_field(&empty, "Приоритет"), None);
        let mut no_legacy = HashMap::new();
        no_legacy.insert("Приоритет".to_string(), 49841008);
        assert_eq!(resolve_field(&no_legacy, "Automerge"), None);
    }
}
