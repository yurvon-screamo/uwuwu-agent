use std::io::IsTerminal;

use comfy_table::{presets::UTF8_FULL, Attribute, Cell, Color, ContentArrangement, Table};

use crate::task_list::ListEntry;

fn priority_color(p: Option<&str>) -> Color {
    use crate::task_query::priority_rank;
    match priority_rank(p) {
        0 => Color::Red,
        1 => Color::Yellow,
        2 => Color::DarkGrey,
        _ => Color::DarkGrey,
    }
}

fn priority_badge(p: Option<&str>) -> String {
    match p.map(|s| s.to_lowercase()).as_deref() {
        Some("high") => "H".to_string(),
        Some("normal") => "N".to_string(),
        Some("low") => "L".to_string(),
        _ => "?".to_string(),
    }
}

fn designed_badge(designed: Option<bool>) -> String {
    if designed == Some(true) {
        "✓".to_string()
    } else {
        "".to_string()
    }
}

pub fn render_task_table(entries: &[ListEntry]) {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Pr"),
            Cell::new("Date"),
            Cell::new("Dl"),
            Cell::new("✎"),
            Cell::new("Title").add_attribute(Attribute::Bold),
            Cell::new("Description"),
            Cell::new("Slug").fg(Color::Cyan),
        ]);

    if !std::io::stdout().is_terminal() {
        table.set_width(220);
    }

    for e in entries {
        let created = e.created.as_deref().unwrap_or("????-??-??");
        let title = e.title.as_deref().unwrap_or("(no title)");
        let description = e.description.as_deref().unwrap_or("");
        let deadline = e.deadline.as_deref().unwrap_or("");
        table.add_row(vec![
            Cell::new(priority_badge(e.priority.as_deref()))
                .fg(priority_color(e.priority.as_deref())),
            Cell::new(created).fg(Color::DarkGrey),
            Cell::new(deadline).fg(Color::Magenta),
            Cell::new(designed_badge(e.designed)).fg(Color::Green),
            Cell::new(title),
            Cell::new(description),
            Cell::new(&e.slug).fg(Color::Cyan),
        ]);
    }

    println!("{table}");
}
