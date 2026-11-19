//! Манифест-мост route-coverage пин-теста (#1329): маршруты роутера
//! mock_opencode в роутерной форме (с /api). Файл вне mod-дерева — в
//! пин-тест включается через #[path] (tests/mock_route_coverage.rs),
//! прод-код не участвует.
//!
//! Цепочка правды: вызовы клиента ⊆ MOCK_ROUTES ⊆ роутер мока.
//! Проб-тест ходит по маршруту как есть (base + путь), скан-тест
//! срезает ведущий /api при сравнении.

/// Маршруты роутера мока: (method, path). {id} — единственный плейсхолдер.
pub const MOCK_ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/session"),
    ("GET", "/api/session"),
    ("GET", "/api/session/{id}"),
    ("DELETE", "/api/session/{id}"),
    ("POST", "/api/session/{id}/abort"),
    ("POST", "/api/session/{id}/age"),
    ("POST", "/api/session/{id}/interrupt"),
    ("POST", "/api/session/{id}/prompt"),
    ("POST", "/api/session/{id}/command"),
    ("POST", "/api/session/{id}/agent"),
    ("GET", "/api/session/{id}/message"),
    ("GET", "/api/experimental/session/{id}/export"),
];

/// Канонизация для сравнения: срез query, ведущий /api, {…} → {id}.
pub fn canonical(path: &str) -> String {
    let no_query = path.split('?').next().unwrap_or(path);
    let no_api = no_query.strip_prefix("/api").unwrap_or(no_query);
    normalize_placeholders(no_api)
}

/// Ручная нормализация плейсхолдеров {…} → {id} (regex запрещён).
fn normalize_placeholders(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        if c == '{' {
            for closing in chars.by_ref() {
                if closing == '}' {
                    break;
                }
            }
            out.push_str("{id}");
        } else {
            out.push(c);
        }
    }
    out
}
