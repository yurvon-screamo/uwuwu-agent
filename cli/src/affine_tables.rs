//! Markdown table splitting for the AFFiNE importer.
//!
//! AFFiNE caps imported tables at 20 rows total; longer tables abort the
//! whole import. Oversized tables are split into consecutive chunks with the
//! header repeated so every chunk fits the cap.

/// Chunks keep 18 data rows to stay under the 20-row limit with the header
/// and delimiter counted in.
pub const AFFINE_TABLE_CHUNK_ROWS: usize = 18;

pub fn split_long_tables(markdown: &str, max_data_rows: usize) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        if is_table_row(line) && i + 1 < lines.len() && is_table_delimiter(lines[i + 1]) {
            let header = line.to_string();
            let delimiter = lines[i + 1].to_string();
            let mut j = i + 2;
            let mut data: Vec<&str> = Vec::new();
            while j < lines.len() && is_table_row(lines[j]) {
                data.push(lines[j]);
                j += 1;
            }
            if data.len() > max_data_rows {
                for chunk in data.chunks(max_data_rows) {
                    let needs_gap = !out.last().map(|l: &String| l.is_empty()).unwrap_or(false);
                    if needs_gap {
                        out.push(String::new());
                    }
                    out.push(header.clone());
                    out.push(delimiter.clone());
                    out.extend(chunk.iter().map(|l| l.to_string()));
                }
            } else {
                out.push(header);
                out.push(delimiter);
                out.extend(data.iter().map(|l| l.to_string()));
            }
            i = j;
        } else {
            out.push(line.to_string());
            i += 1;
        }
    }
    out.join("\n")
}

fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.len() > 1
}

fn is_table_delimiter(line: &str) -> bool {
    let t = line.trim();
    is_table_row(t)
        && t.chars()
            .all(|c| c == '|' || c == '-' || c == ':' || c.is_whitespace())
        && t.contains('-')
}
