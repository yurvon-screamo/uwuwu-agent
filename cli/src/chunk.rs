use std::path::Path;

pub struct Chunk {
    pub text: String,
}

pub const PRIORITY_VALUES: [&str; 3] = ["low", "normal", "high"];

#[derive(Debug, Clone, Default)]
pub struct TaskMeta {
    pub status: Option<String>,
    pub created: Option<String>,
    pub priority: Option<String>,
    pub deadline: Option<String>,
    pub designed: Option<bool>,
}

fn parse_priority(value: &str) -> Option<String> {
    let v = value.trim().to_lowercase();
    if PRIORITY_VALUES.contains(&v.as_str()) {
        Some(v)
    } else {
        None
    }
}

fn parse_deadline(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    crate::task_search::parse_date(v).ok()
}

fn parse_designed(value: &str) -> Option<bool> {
    let v = value.trim().to_lowercase();
    if ["true", "yes", "on", "1"].contains(&v.as_str()) {
        Some(true)
    } else if ["false", "no", "off", "0"].contains(&v.as_str()) {
        Some(false)
    } else {
        None
    }
}

pub fn read_task_meta(filepath: &Path) -> TaskMeta {
    let content = std::fs::read_to_string(filepath).unwrap_or_default();
    let (fm, _) = split_frontmatter(&content);

    parse_frontmatter_fields(&fm)
        .map(|fields| {
            let status = fields.get("status").and_then(|v| non_empty(v));
            let created = fields.get("created").and_then(|v| non_empty(v));
            let priority = fields.get("priority").and_then(|v| parse_priority(v));
            let deadline = fields.get("deadline").and_then(|v| parse_deadline(v));
            let designed = fields.get("designed").and_then(|v| parse_designed(v));
            TaskMeta {
                status,
                created,
                priority,
                deadline,
                designed,
            }
        })
        .unwrap_or_default()
}

pub fn split_chunks(filepath: &Path, wiki_root: &Path) -> Vec<Chunk> {
    let content = match std::fs::read_to_string(filepath) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let rel = filepath
        .strip_prefix(wiki_root)
        .unwrap_or(filepath)
        .to_string_lossy()
        .replace('\\', "/");

    let (fm, body) = split_frontmatter(&content);
    if body.trim().is_empty() {
        return vec![];
    }

    let mut chunks = Vec::new();

    if let Some(meta) = parse_frontmatter_fields(&fm) {
        let title = meta.get("title").cloned().unwrap_or_default();
        let desc = meta.get("description").cloned().unwrap_or_default();
        if !title.is_empty() || !desc.is_empty() {
            let meta_text = format!(
                "{} — Metadata\ntitle: {}\ndescription: {}",
                rel, title, desc
            );
            chunks.push(Chunk { text: meta_text });
        }
    }

    let mut current = String::new();
    let mut current_heading = "Intro".to_string();

    for line in body.lines() {
        if line.starts_with("## ") {
            if !current.trim().is_empty() {
                let text = format!("{} — {}\n{}", rel, current_heading, current.trim());
                chunks.push(Chunk { text });
            }
            current_heading = line.trim_start_matches("## ").trim().to_string();
            current.clear();
        } else {
            current.push_str(line);
            current.push('\n');
        }
    }

    if !current.trim().is_empty() {
        let text = format!("{} — {}\n{}", rel, current_heading, current.trim());
        chunks.push(Chunk { text });
    }

    chunks
}

pub fn split_frontmatter(content: &str) -> (String, String) {
    if !content.starts_with("---") {
        return (String::new(), content.to_string());
    }
    let parts: Vec<&str> = content.splitn(3, "---").collect();
    if parts.len() < 3 {
        return (String::new(), content.to_string());
    }
    (parts[1].to_string(), parts[2].to_string())
}

pub fn update_frontmatter_fields(content: &str, updates: &[(&str, &str)]) -> String {
    let (fm, body) = split_frontmatter(content);
    if fm.is_empty() {
        return content.to_string();
    }

    let fm_normalized = fm.trim();
    let mut lines: Vec<String> = fm_normalized.lines().map(String::from).collect();
    let mut applied = std::collections::HashSet::new();

    for line in lines.iter_mut() {
        if let Some((key, _)) = line.split_once(':') {
            let trimmed_key = key.trim().to_string();
            if let Some((_, new_val)) = updates.iter().find(|(k, _)| *k == trimmed_key.as_str()) {
                *line = format!("{trimmed_key}: {new_val}");
                applied.insert(trimmed_key);
            }
        }
    }

    for (key, val) in updates {
        if !applied.contains(*key) {
            lines.push(format!("{key}: {val}"));
        }
    }

    let new_fm = lines.join("\n");
    format!("---\n{new_fm}\n---{body}")
}

pub fn remove_frontmatter_fields(content: &str, keys: &[&str]) -> String {
    let (fm, body) = split_frontmatter(content);
    if fm.is_empty() {
        return content.to_string();
    }

    let fm_normalized = fm.trim();
    let key_set: std::collections::HashSet<&str> = keys.iter().copied().collect();

    let kept: Vec<String> = fm_normalized
        .lines()
        .filter(|line| {
            if let Some((key, _)) = line.split_once(':') {
                let trimmed_key = key.trim();
                !key_set.contains(trimmed_key)
            } else {
                true
            }
        })
        .map(String::from)
        .collect();

    let new_fm = kept.join("\n");
    format!("---\n{new_fm}\n---{body}")
}

pub(crate) fn parse_frontmatter_fields(
    fm: &str,
) -> Option<std::collections::HashMap<String, String>> {
    if fm.trim().is_empty() {
        return None;
    }
    let mut fields = std::collections::HashMap::new();
    for line in fm.lines() {
        if let Some((key, val)) = line.split_once(':') {
            fields.insert(key.trim().to_string(), val.trim().to_string());
        }
    }
    if fields.is_empty() {
        None
    } else {
        Some(fields)
    }
}

pub fn read_meta(filepath: &Path) -> (Option<String>, Option<String>) {
    let content = std::fs::read_to_string(filepath).unwrap_or_default();
    let (fm, body) = split_frontmatter(&content);

    let (title, description) = parse_frontmatter_fields(&fm)
        .map(|fields| {
            let title = fields.get("title").and_then(|v| non_empty(v));
            let description = fields.get("description").and_then(|v| non_empty(v));
            (title, description)
        })
        .unwrap_or((None, None));

    let title = title.or_else(|| first_h1(&body));
    (title, description)
}

pub fn read_status_and_created(filepath: &Path) -> (Option<String>, Option<String>) {
    let meta = read_task_meta(filepath);
    (meta.status, meta.created)
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn first_h1(body: &str) -> Option<String> {
    body.lines()
        .find_map(|line| line.strip_prefix("# ").map(|h| h.trim().to_string()))
}
