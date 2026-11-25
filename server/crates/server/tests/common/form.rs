//! Дескрипторы формы и дифф против пина (#1254): сравнение идёт по
//! осям пина, каждая ось названа; красный печатает все расхождения
//! сразу — поверхность, ось, ожидалось/фактически.

use serde_json::Value;

/// Пин живой формы executor-API (эталоны пробы #1249 поле-в-поле).
pub const PIN_JSON: &str = include_str!("../fixtures/executor_form_pin.json");

/// Проекция пин-файла: оси контракта формы. Полнота перечня осей
/// держится юнит-пином `pin_carries_every_contract_axis` в
/// executor_form_contract.rs — тривиально-зелёное сравнение исключено.
pub struct Pin {
    pub message_wrapper: String,
    pub forbidden_time: Vec<String>,
    pub assistant_final_time: Vec<String>,
    pub assistant_streaming_time: Vec<String>,
    pub idle_time: Vec<String>,
    pub shell_message_form: String,
    pub shell_message_forbidden: Vec<String>,
    pub list_wrapper: String,
    pub list_cursor: bool,
    pub item_time: Vec<String>,
    pub default_agent_key: String,
    pub default_outcome: String,
    pub role_agent_key: String,
}

impl Pin {
    pub fn load() -> Self {
        let p: Value = serde_json::from_str(PIN_JSON).expect("пин-файл — валидный JSON");
        let mt = &p["message_tail"];
        let um = &p["shell_message"];
        let sl = &p["session_list"];
        let ds = &sl["default_session"];
        let rs = &sl["role_session"];
        Self {
            message_wrapper: mt["wrapper"].as_str().expect("message_tail.wrapper").into(),
            forbidden_time: str_list(&mt["forbidden_time_fields"]),
            assistant_final_time: str_list(&mt["assistant_final_time"]),
            assistant_streaming_time: str_list(&mt["assistant_streaming_time"]),
            idle_time: str_list(&mt["idle_time"]),
            shell_message_form: um["form"].as_str().expect("shell_message.form").into(),
            shell_message_forbidden: str_list(&um["forbidden_keys"]),
            list_wrapper: sl["wrapper"].as_str().expect("session_list.wrapper").into(),
            list_cursor: sl["cursor"].as_bool().expect("session_list.cursor"),
            item_time: str_list(&sl["item_time"]),
            default_agent_key: ds["agent_key"]
                .as_str()
                .expect("default_session.agent_key")
                .into(),
            default_outcome: ds["outcome"]
                .as_str()
                .expect("default_session.outcome")
                .into(),
            role_agent_key: rs["agent_key"]
                .as_str()
                .expect("role_session.agent_key")
                .into(),
        }
    }
}

fn str_list(v: &Value) -> Vec<String> {
    v.as_array()
        .expect("список строк в пине")
        .iter()
        .map(|x| x.as_str().expect("строка в пине").to_string())
        .collect()
}

/// Ключи time-объекта события, отсортированные; объекта нет — пусто.
pub fn time_keys(msg: &Value) -> Vec<String> {
    let mut keys: Vec<String> = msg["time"]
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

/// Накопитель диффа по осям: расхождения собираются все, красный
/// печатает их списком — починка не по одному перегону.
#[derive(Default, Debug)]
pub struct Diff(Vec<String>);

impl Diff {
    /// Сравнение КАК НАБОРОВ: порядок ключей JSON-объекта вне
    /// контракта (time_keys и оси пина отсортированы — сравнение
    /// сортировкой делает хелпер устойчивым к порядку входа).
    pub fn field_set(&mut self, axis: &str, expected: &[String], actual: &[String]) {
        let mut e: Vec<&String> = expected.iter().collect();
        let mut a: Vec<&String> = actual.iter().collect();
        e.sort();
        a.sort();
        if e != a {
            self.0.push(format!(
                "{axis}: ожидалось {expected:?}, фактически {actual:?}"
            ));
        }
    }

    /// Булева ось: условие + описание нарушения.
    pub fn expect(&mut self, axis: &str, ok: bool, violation: &str) {
        if !ok {
            self.0.push(format!("{axis}: {violation}"));
        }
    }

    pub fn finish(self, surface: &str) {
        assert!(
            self.0.is_empty(),
            "рассинхрон формы мока с пином — {surface}:\n{}",
            self.0.join("\n")
        );
    }
}
