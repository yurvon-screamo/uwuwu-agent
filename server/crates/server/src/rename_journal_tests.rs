//! Офлайн-пины файловой персистентности pre-image журнала (#1609): ветки
//! load() (corrupt / IO / NotFound) и атомарность save_now(). Без сети,
//! живых досок и гейта UWUWU_E2E — временный каталог и ловушка событий.

use super::test_support::{assert_event, board_keys, image, trap_events, TempDir};
use crate::rename_journal::JsonStatusPreImages;
use uwuwu_usecase::StatusPreImageStore;

/// Валидный журнал переживает reload: следующий put не затирает чужой снимок.
/// Пинит основной путь load() — мутация `Ok(f) => default()` её не ловит
/// ни одна из веток-ошибок.
#[test]
fn valid_journal_survives_reload_and_keeps_other_board() {
    let dir = TempDir::new("roundtrip");
    let path = dir.join("rename-pre-image.json");
    JsonStatusPreImages::load(path.clone())
        .put(&image("board-a"))
        .expect("первый put");
    // Новый экземпляр — как рестарт сервера.
    JsonStatusPreImages::load(path.clone())
        .put(&image("board-b"))
        .expect("второй put");
    let raw = std::fs::read_to_string(&path).expect("журнал прочитан");
    let json: serde_json::Value = serde_json::from_str(&raw).expect("журнал — JSON");
    let boards = json["by_board"].as_object().expect("by_board — объект");
    let mut keys: Vec<String> = boards.keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["board-a", "board-b"],
        "обе доски пережили reload"
    );
    assert_eq!(boards["board-a"]["board"].as_str(), Some("board-a"));
    assert_eq!(boards["board-a"]["from"].as_str(), Some("Draft"));
    assert_eq!(boards["board-a"]["to"].as_str(), Some("Todo"));
    assert_eq!(boards["board-b"]["board"].as_str(), Some("board-b"));
    assert_eq!(boards["board-b"]["created_ms"].as_u64(), Some(42));
}

/// Битый JSON: громкое rename_preimage.corrupt, пустой старт, журнал жив.
#[test]
fn corrupt_journal_starts_empty_with_loud_warn() {
    let dir = TempDir::new("corrupt");
    let path = dir.join("rename-pre-image.json");
    // Старое содержимое остаётся в тексте, но не разбирается: после старта
    // пустым свежий put оставляет в файле только новую доску.
    std::fs::write(&path, r#"{"by_board":{"uwuwu":{}}}GARBAGE"#).unwrap();
    let (guard, records) = trap_events();
    let store = JsonStatusPreImages::load(path.clone());
    store
        .put(&image("fresh-board"))
        .expect("журнал пишется после битых данных");
    drop(guard);
    assert_event(
        &records.lock().expect("trap lock"),
        "rename_preimage.corrupt",
    );
    assert_eq!(
        board_keys(&path),
        vec!["fresh-board".to_string()],
        "старые данные из битого файла не воскресли"
    );
}

/// IO-ошибка чтения: warn, старт пустым; где запись возможна — put чинит файл.
#[test]
fn unreadable_journal_warns_and_put_recovers_when_allowed() {
    let dir = TempDir::new("io");
    let path = dir.join("rename-pre-image.json");
    std::fs::write(&path, r#"{"by_board":{"old-board":{}}}"#).unwrap();
    #[cfg(unix)]
    let read_denied = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o200)).unwrap();
        std::fs::read_to_string(&path).is_err()
    };
    #[cfg(not(unix))]
    let read_denied = false;
    // root читает даже 0o200 — каталог на месте файла даёт тот же не-NotFound
    // отказ (EISDIR), но запись в цель невозможна; это честно проверяется ниже.
    if !read_denied {
        eprintln!("root: IO-ветка проверена на EISDIR-фолбэке без put");
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
    }
    let (guard, records) = trap_events();
    let store = JsonStatusPreImages::load(path.clone());
    let put = store.put(&image("new-board"));
    drop(guard);
    assert_event(
        &records.lock().expect("trap lock"),
        "rename_preimage.unreadable",
    );
    if read_denied {
        put.expect("put перезаписывает файл");
        assert_eq!(board_keys(&path), vec!["new-board".to_string()]);
    } else {
        let err = put.expect_err("цель — каталог: rename невозможен");
        assert!(err.to_string().contains("rename "), "{err}");
    }
}

#[test]
fn missing_journal_is_silent_empty_store() {
    let dir = TempDir::new("notfound");
    let path = dir.join("rename-pre-image.json");
    let (guard, records) = trap_events();
    let store = JsonStatusPreImages::load(path.clone());
    assert_eq!(store.path_hint(), path.display().to_string());
    store
        .put(&image("first-board"))
        .expect("первый put создаёт файл");
    drop(guard);
    let records = records.lock().expect("trap lock");
    assert!(
        !records.iter().any(|r| r.contains("rename_preimage.")),
        "NotFound — тихий дефолт: {records:?}"
    );
    assert_eq!(board_keys(&path), vec!["first-board".to_string()]);
}

#[test]
fn failed_tmp_write_keeps_existing_journal() {
    let dir = TempDir::new("write-fail");
    let path = dir.join("rename-pre-image.json");
    let store = JsonStatusPreImages::load(path.clone());
    store.put(&image("old-board")).unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    // tmp-путь — каталог: write(tmp) отказывается до rename, цель не тронута.
    std::fs::create_dir(path.with_extension("tmp")).unwrap();
    let err = store.put(&image("new-board")).expect_err("write tmp fails");
    assert!(err.to_string().contains("write "), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn failed_rename_keeps_existing_journal() {
    let dir = TempDir::new("rename-fail");
    let path = dir.join("rename-pre-image.json");
    let store = JsonStatusPreImages::load(path.clone());
    store.put(&image("old-board")).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let sentinel = path.join("sentinel");
    std::fs::write(&sentinel, b"keep").unwrap();
    // rename файла поверх каталога отказывается: saved-материал не портится.
    let err = store.put(&image("new-board")).expect_err("rename fails");
    assert!(err.to_string().contains("rename "), "{err}");
    assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "keep");
}
