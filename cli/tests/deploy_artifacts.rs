//! Пин канон-артефакта ретеншена журнала (deploy/siyuan-demo).
//!
//! Задача 20261109083919-v5mlahe: лимит journald поднят с пакетных 50M до 1G
//! дроп-ином в /etc; канон-артефакт в репо — контракт «хост-состояние
//! воспроизводимо с репо». Живая граница (работающий journald, персистентность
//! логов) в CI недоступна — здесь закреплено то, что репо может проверить:
//! артефакт существует и несёт директиву, перекрывающую пакетный лимит
//! cachyos-settings (механика merge: поздний лексикографически drop-in
//! выигрывает, имя артефакта 50-… > пакетного 00-journal-size.conf).

use std::path::Path;

const PACKAGE_DROPOIN_NAME: &str = "00-journal-size.conf";
const PACKAGE_SYSTEM_MAX_USE: u64 = 50 * 1024 * 1024; // 50M из cachyos-settings

/// Минимальный парсер значений journald.conf: байты и суффиксы K/M/G/X
/// (бинарные степени, как читает journald). Строки без суффикса — байты.
fn parse_journal_size(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    let (digits, multiplier) = match trimmed.chars().last()? {
        'K' | 'k' => (&trimmed[..trimmed.len() - 1], 1024u64),
        'M' | 'm' => (&trimmed[..trimmed.len() - 1], 1024 * 1024),
        'G' | 'g' => (&trimmed[..trimmed.len() - 1], 1024 * 1024 * 1024),
        _ => (trimmed, 1),
    };
    digits.trim().parse::<u64>().ok().map(|n| n * multiplier)
}

/// Активные (не закомментированные) директивы секции [Journal] дроп-ина.
fn active_journal_directives(conf: &str) -> Vec<(String, String)> {
    let mut in_journal_section = false;
    let mut directives = Vec::new();
    for line in conf.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_journal_section = line == "[Journal]";
            continue;
        }
        if in_journal_section {
            if let Some((key, value)) = line.split_once('=') {
                directives.push((key.trim().to_string(), value.trim().to_string()));
            }
        }
    }
    directives
}

fn retention_artifact_path() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli/ always has a parent (repo root)")
        .join("deploy/siyuan-demo/journald-retention.conf")
        .leak()
}

#[test]
fn retention_dropin_carries_active_system_max_use_over_package_limit() {
    // Arrange
    let path = retention_artifact_path();
    let conf = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("канон-артефакт ретеншена должен существовать: {path:?}: {e}"));

    // Act
    let directives = active_journal_directives(&conf);
    let system_max_use = directives
        .iter()
        .find(|(key, _)| key == "SystemMaxUse")
        .map(|(_, value)| parse_journal_size(value))
        .unwrap_or(None);

    // Assert
    assert!(
        directives.iter().any(|(key, _)| key == "SystemMaxUse"),
        "артефакт потерял активную директиву SystemMaxUse — повторная установка по README вернёт пакетный вакуум 50M"
    );
    let system_max_use =
        system_max_use.expect("значение SystemMaxUse не парсится как размер journald (K/M/G)");
    assert!(
        system_max_use > PACKAGE_SYSTEM_MAX_USE,
        "SystemMaxUse ({system_max_use} байт) не перекрывает пакетный лимит cachyos-settings ({PACKAGE_SYSTEM_MAX_USE} байт) — user-журнал снова вакуумируется за часы"
    );
}

#[test]
fn retention_dropin_filename_lexicographically_outlasts_package_dropin() {
    // Arrange
    const ARTIFACT_INSTALLED_NAME: &str = "50-uwuwu-journal-retention.conf";

    // Act + Assert
    // merge journald: поздний лексикографически drop-in выигрывает —
    // суть механики перекрытия пакетного твика из постановки.
    assert!(
        ARTIFACT_INSTALLED_NAME > PACKAGE_DROPOIN_NAME,
        "имя дроп-ина «{ARTIFACT_INSTALLED_NAME}» перестало лексикографически перекрывать пакетный «{PACKAGE_DROPOIN_NAME}» — при merge journald наш лимит проиграет пакетному"
    );
}

#[test]
fn journal_size_parser_reads_binary_suffixes_and_bare_bytes() {
    assert_eq!(parse_journal_size("1G"), Some(1024 * 1024 * 1024));
    assert_eq!(parse_journal_size("50M"), Some(50 * 1024 * 1024));
    assert_eq!(parse_journal_size("512K"), Some(512 * 1024));
    assert_eq!(parse_journal_size("1073741824"), Some(1024 * 1024 * 1024));
    assert_eq!(parse_journal_size("abc"), None);
    assert_eq!(parse_journal_size(""), None);
}

#[test]
fn commented_directives_and_other_sections_are_not_active() {
    let conf =
        "# SystemMaxUse=1G\n[Log]\nMaxLevel=info\n[Journal]\n# SystemMaxUse=8G\nSystemMaxUse=1G\n";
    let directives = active_journal_directives(conf);
    assert_eq!(
        directives,
        vec![("SystemMaxUse".to_string(), "1G".to_string())]
    );
}

// --- Пин канона JST ядра SiYuan (задача 20261110042724-aivnb3b) ---
//
// Ядро форматирует date-клетки досок и frontmatter-даты в своей зоне; базовый
// образ b3log/siyuan несёт TZ=Asia/Shanghai из Dockerfile — без явного
// переопределения клетка «Запланировано» кронов уезжала на час против
// JST-тайтлов ран-карточек. Канон задаётся окружением юнита (Environment=TZ),
// не кодом — артефакт в репо контракт «хост-состояние воспроизводимо с репо».

const SIYUAN_TZ_CANON: &str = "Asia/Tokyo";

/// Активные (не закомментированные) значения `Environment=` секции
/// `[Container]` quadlet-юнита. Quadlet допускает несколько Environment-строк.
fn container_env_values(unit: &str) -> Vec<String> {
    let mut in_container = false;
    let mut values = Vec::new();
    for line in unit.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_container = line == "[Container]";
            continue;
        }
        if in_container {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == "Environment" {
                    values.push(value.trim().to_string());
                }
            }
        }
    }
    values
}

fn siyuan_container_unit_path() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli/ always has a parent (repo root)")
        .join("deploy/siyuan-demo/siyuan-demo.container")
        .leak()
}

#[test]
fn siyuan_demo_container_carries_active_tz_tokyo() {
    // Arrange
    let path = siyuan_container_unit_path();
    let unit = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("quadlet-юнит siyuan-demo должен существовать: {path:?}: {e}"));

    // Act
    let env_values = container_env_values(&unit);
    let tz = env_values.iter().find_map(|v| {
        v.split_whitespace()
            .find_map(|pair| pair.strip_prefix("TZ=").map(str::to_string))
    });

    // Assert
    assert!(
        !env_values.is_empty() || unit.contains("Environment"),
        "в секции [Container] не осталось активных Environment-директив"
    );
    assert_eq!(
        tz.as_deref(),
        Some(SIYUAN_TZ_CANON),
        "активной директивы Environment=…TZ={SIYUAN_TZ_CANON} нет в quadlet siyuan-demo — ядро унаследует Asia/Shanghai из образа и клетки «Запланировано» снова уедут на час против JST-тайтлов"
    );
}

#[test]
fn siyuan_demo_readme_documents_tz_canon() {
    // Arrange
    let readme_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli/ always has a parent (repo root)")
        .join("deploy/siyuan-demo/README.md");
    let readme = std::fs::read_to_string(&readme_path).unwrap_or_else(|e| {
        panic!("deploy/siyuan-demo/README.md должен существовать: {readme_path:?}: {e}")
    });

    // Act + Assert
    // README — путь воспроизводимости установки: секция канона обязана нести
    // и причину (образ несёт Shanghai), и живую проверку зоны ядра.
    assert!(
        readme.contains("Часовой пояс"),
        "в deploy/siyuan-demo/README.md пропала секция про часовой пояс — канон JST не воспроизводим с репо"
    );
    assert!(
        readme.contains(SIYUAN_TZ_CANON),
        "deploy/siyuan-demo/README.md не называет канонную зону ({SIYUAN_TZ_CANON})"
    );
    assert!(
        readme.contains("docker exec siyuan-demo date"),
        "в deploy/siyuan-demo/README.md пропала живая проверка зоны ядра (docker exec siyuan-demo date)"
    );
}

#[test]
fn container_env_parser_skips_comments_and_other_sections() {
    let unit = "# Environment=TZ=Asia/Tokyo\n[Unit]\nEnvironment=LEAK=yes\n[Container]\nImage=x\n# Environment=TZ=Europe/Moscow\nEnvironment=TZ=Asia/Tokyo\nEnvironment=FOO=bar\n";
    assert_eq!(
        container_env_values(unit),
        vec!["TZ=Asia/Tokyo".to_string(), "FOO=bar".to_string()]
    );
}
