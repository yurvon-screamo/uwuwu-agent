//! Тесты аттачей: collect_asset_refs, percent-кодирование, asset-refs,
//! validate_attach_files, save_body_assets, asset_key_is_safe.

use super::attach::asset_key_is_safe;

use super::attach::collect_asset_refs;

use super::attach::save_body_assets;

use crate::siyuan::SiYuan;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn collect_asset_refs_extracts_unique_assets_in_order() {
    let md = "![а](assets/first.png)\nтекст [файл](assets/second.htm) ![а](assets/first.png)\n";
    assert_eq!(collect_asset_refs(md), vec!["first.png", "second.htm"]);
}

#[test]
fn collect_asset_refs_percent_decodes_names() {
    let md = "![ф](assets/%D0%B8%D0%BC%D1%8F%20я.png)\n";
    assert_eq!(collect_asset_refs(md), vec!["имя я.png"]);
}

#[test]
fn collect_asset_refs_ignores_non_asset_links() {
    let md = "[сайт](https://example.com/x.png) [внутр](/Projects/doc) ![п](assets/ok.png)\n";
    assert_eq!(collect_asset_refs(md), vec!["ok.png"]);
}

// ——— attach: кодер референсов (симметрия с читателем collect_asset_refs) ———

use super::attach::{asset_ref_md, attach_refs_md, percent_encode_asset_path, validate_attach_files};
use crate::siyuan::UploadedAsset;

fn asset(name: &str, path: &str) -> UploadedAsset {
    UploadedAsset {
        original_name: name.into(),
        asset_path: path.into(),
    }
}

#[test]
fn percent_encoding_round_trips_through_reader_on_space_and_cyrillic() {
    for name in [
        "скрин 2.png",
        "имя я.png",
        "снимок экрана (1).png",
        "отчёт.pdf",
    ] {
        let path = format!("assets/{name}");
        let md = asset_ref_md(&asset(name, &path));
        let decoded = collect_asset_refs(&md);
        assert_eq!(decoded, vec![name.to_string()], "round-trip {name}: {md}");
    }
}

#[test]
fn percent_encoding_covers_unreserved_only() {
    assert_eq!(
        percent_encode_asset_path("assets/a b.png"),
        "assets/a%20b.png"
    );
    assert_eq!(
        percent_encode_asset_path("assets/а.png"),
        "assets/%D0%B0.png"
    );
    assert_eq!(
        percent_encode_asset_path("assets/ok-_.~1.txt"),
        "assets/ok-_.~1.txt"
    );
    assert_eq!(
        percent_encode_asset_path("assets/x(1).png"),
        "assets/x%281%29.png"
    );
}

#[test]
fn asset_ref_md_renders_image_for_image_extension_and_link_otherwise() {
    let img = asset_ref_md(&asset("скрин.png", "assets/скрин.png"));
    assert!(img.starts_with("![скрин.png]("), "картинка: {img}");
    assert!(img.contains("%D1%81"), "кириллица пути закодирована: {img}");
    let file = asset_ref_md(&asset("отчёт.pdf", "assets/отчёт.pdf"));
    assert!(
        file.starts_with("[отчёт.pdf](") && !file.starts_with("!["),
        "ссылка: {file}"
    );
}

#[test]
fn asset_ref_md_alt_text_without_brackets_keeps_markdown_intact() {
    let md = asset_ref_md(&asset("weird]name.png", "assets/weird.png"));
    let decoded = collect_asset_refs(&md);
    assert_eq!(
        decoded,
        vec!["weird.png".to_string()],
        "ссылка не разорвана: {md}"
    );
}

#[test]
fn attach_refs_md_starts_with_blank_line_one_ref_per_line() {
    let md = attach_refs_md(&[
        asset("a.png", "assets/a.png"),
        asset("b.pdf", "assets/b.pdf"),
    ]);
    assert!(
        md.starts_with("\n"),
        "пустая строка-разделитель от контента: {md:?}"
    );
    let lines: Vec<&str> = md.trim().lines().collect();
    assert_eq!(lines.len(), 2, "по строке на ассет: {md:?}");
    assert!(lines[0].starts_with("![") && lines[1].starts_with("["));
}

#[test]
fn validate_attach_files_bails_on_missing_and_directory_before_network() {
    let dir = tempfile::tempdir().unwrap();
    let ok = dir.path().join("ok.png");
    std::fs::write(&ok, b"x").unwrap();
    let missing = dir.path().join("нет.png");
    let err =
        validate_attach_files(&[ok.clone(), missing]).expect_err("несуществующий файл — отказ");
    assert!(err.to_string().contains("нет.png"), "суть: {err}");
    let err = validate_attach_files(&[dir.path().to_path_buf()]).expect_err("директория — отказ");
    assert!(err.to_string().contains("не файл"), "суть: {err}");
    assert_eq!(validate_attach_files(&[ok]).unwrap().len(), 1);
}

#[tokio::test]
async fn save_body_assets_downloads_with_original_names_and_skips_missing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/file/getFile"))
        .respond_with(|req: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let path = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
            if path == "data/assets/assets-self-test/img.png" {
                ResponseTemplate::new(200).set_body_bytes(b"png-bytes".to_vec())
            } else {
                ResponseTemplate::new(404)
                    .set_body_string(r#"{"code":404,"msg":"path does not exist"}"#)
            }
        })
        .mount(&server)
        .await;
    let sy = SiYuan::for_tests(server.uri());

    const ROW: &str = "20261105090000-assette1";
    let md = "![и](assets/assets-self-test/img.png) ![нет](assets/assets-self-test/gone.png)\n";
    let saved = save_body_assets(&sy, md, ROW).await;

    assert_eq!(
        saved.len(),
        1,
        "доступный аттач сохранён, Missing — тихо пропущен: {saved:?}"
    );
    assert!(
        saved[0].ends_with("/img.png"),
        "оригинальное имя файла: {saved:?}"
    );
    let bytes = std::fs::read(&saved[0]).expect("файл записан");
    assert_eq!(bytes, b"png-bytes");
    std::fs::remove_dir_all(format!("/tmp/uwuwu-task-{ROW}")).expect("тест прибрал за себя");
}

#[tokio::test]
async fn save_body_assets_never_downloads_traversal_keys() {
    let server = MockServer::start().await;
    let file_calls = Arc::new(AtomicUsize::new(0));
    let calls = file_calls.clone();
    Mock::given(method("POST"))
        .and(path("/api/file/getFile"))
        .respond_with(move |req: &wiremock::Request| {
            calls.fetch_add(1, Ordering::SeqCst);
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let path = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
            if path == "data/assets/ok.png" {
                ResponseTemplate::new(200).set_body_bytes(b"png-bytes".to_vec())
            } else {
                ResponseTemplate::new(404)
                    .set_body_string(r#"{"code":404,"msg":"path does not exist"}"#)
            }
        })
        .mount(&server)
        .await;
    let sy = SiYuan::for_tests(server.uri());

    const ROW: &str = "20261105090000-travers";
    let md = "![x](assets/../../etc) ![y](assets/..) ![z](assets/./x) ![ok](assets/ok.png)\n";
    let saved = save_body_assets(&sy, md, ROW).await;

    assert_eq!(saved.len(), 1, "только безопасный ключ скачан: {saved:?}");
    assert_eq!(
        file_calls.load(Ordering::SeqCst),
        1,
        "точечные сегменты (../ и ./) не скачиваются вовсе — один getFile за ok.png"
    );
    std::fs::remove_dir_all(format!("/tmp/uwuwu-task-{ROW}")).expect("тест прибрал за себя");
}

#[test]
fn asset_key_is_safe_rejects_any_dot_segment() {
    assert!(!asset_key_is_safe("../../etc"));
    assert!(!asset_key_is_safe("sub/./x"));
    assert!(!asset_key_is_safe("a/../b"));
    assert!(!asset_key_is_safe(""));
    assert!(asset_key_is_safe("img.png"));
    assert!(asset_key_is_safe("sub/dir/file.htm"));
}
