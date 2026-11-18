//! Юнит-пины парсеров Pv2 REST (#1236): формы — живая проба среза 0
//! (тело задачи #1236, 18.11): numeric field/item id, hex option id,
//! Status в value.name.raw, Link-курсор пагинации.

use crate::github::pv2_parse::{
    next_after, parse_field_catalog, parse_item_entry, parse_status_value,
};
use serde_json::json;

/// Живой каталог GET /projectsV2/{n}/fields (проба 18.11): Status —
/// numeric id, опции — hex-строки, имя опции в name.raw.
#[test]
fn field_catalog_matches_live_probe_shape() {
    let fields = json!([
        { "id": 442951855, "name": "Title", "data_type": "title", "options": null },
        { "id": 442951857, "name": "Status", "data_type": "single_select", "options": [
            { "id": "eff28a30", "name": { "html": "Analys", "raw": "Analys" } },
            { "id": "79e23eab", "name": { "html": "Todo", "raw": "Todo" } }
        ] }
    ]);
    let meta = parse_field_catalog(5, &fields, "Задачи uwuwu").expect("catalog parses");
    assert_eq!(meta.project_number, 5);
    assert_eq!(meta.status_field_id, 442951857);
    assert_eq!(
        meta.options.get("Analys").map(String::as_str),
        Some("eff28a30")
    );
    assert_eq!(
        meta.options.get("Todo").map(String::as_str),
        Some("79e23eab")
    );
}

/// Статус-поле отсутствует в каталоге — громкий отказ, не тихий None.
#[test]
fn field_catalog_without_status_is_loud() {
    let fields = json!([{ "id": 1, "name": "Title", "data_type": "title" }]);
    assert!(parse_field_catalog(5, &fields, "Задачи uwuwu").is_err());
}

/// Живой PATCH-ответ (проба): Status single-select — значение в
/// value.name.raw, single_select_option = null.
#[test]
fn status_value_reads_value_name_raw() {
    let fields = json!([
        { "id": 442951855, "name": "Title", "value": { "html": "T", "raw": "T" } },
        { "id": 442951857, "name": "Status", "single_select_option": null,
          "value": { "color": "YELLOW", "id": "eff28a30",
                     "name": { "html": "Analys", "raw": "Analys" } } }
    ]);
    assert_eq!(parse_status_value(&fields).as_deref(), Some("Analys"));
}

/// Read-лаг (проба среза 0): fields без Status-поля — None, не пустая
/// строка: ретрай-цикл verify отличает лаг от «задано пустое имя».
#[test]
fn status_value_missing_field_is_none() {
    let fields = json!([
        { "id": 442951855, "name": "Title", "value": { "html": "T", "raw": "T" } }
    ]);
    assert_eq!(parse_status_value(&fields), None);
}

/// Живой Link-заголовок (проба): after-курсор rel="next" ветки.
#[test]
fn next_after_reads_live_link_cursor() {
    let link = r#"<https://api.github.com/user/109030262/projectsV2/5/items?per_page=100&after=Y3Vyc29yOnYyOpK5MDAwMDAwMDAuMDAyNzMyMjQwNDM3MTU4NM4SFe8H>; rel="next", <https://api.github.com/user/109030262/projectsV2/5/items?per_page=100>; rel="prev""#;
    assert_eq!(
        next_after(Some(link)).as_deref(),
        Some("Y3Vyc29yOnYyOpK5MDAwMDAwMDAuMDAyNzMyMjQwNDM3MTU4NM4SFe8H")
    );
}

/// Последняя страница: Link без rel="next" — None (конец пагинации).
#[test]
fn next_after_without_next_branch_is_none() {
    let link =
        r#"<https://api.github.com/user/109030262/projectsV2/5/items?per_page=100>; rel="prev""#;
    assert_eq!(next_after(Some(link)), None);
    assert_eq!(next_after(None), None);
}

/// Живой элемент list items (проба): numeric id + content.number.
#[test]
fn item_entry_reads_live_list_shape() {
    let item = json!({
        "id": 309546144,
        "node_id": "PVTI_lAHOBn-rds4BrTmTzhJzTKA",
        "content_type": "Issue",
        "content": { "number": 1307 }
    });
    assert_eq!(parse_item_entry(&item), Some((1307, 309546144)));
    assert_eq!(parse_item_entry(&json!({ "id": 1 })), None);
}
