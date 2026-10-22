//! Tests for affine_tables: chunking oversized markdown tables.

use crate::affine_tables::split_long_tables;

#[test]
fn long_table_is_split_into_fitting_chunks() {
    let rows: Vec<String> = (1..=45).map(|n| format!("| col{n} | value |")).collect();
    let markdown = format!(
        "# t\n\n| h1 | h2 |\n|---|---|\n{}\n\ntail text",
        rows.join("\n")
    );
    let split = split_long_tables(&markdown, 18);
    let header_count = split.matches("| h1 | h2 |").count();
    assert_eq!(header_count, 3, "45 rows / 18 = 3 chunks");
    assert!(split.contains("tail text"));
}

#[test]
fn short_table_passes_through_untouched() {
    let markdown = "| h |\n|---|\n| a |\n| b |";
    assert_eq!(split_long_tables(markdown, 18), markdown);
}

#[test]
fn non_table_pipes_are_ignored() {
    let markdown = "just `|code|` inline\nand text";
    assert_eq!(split_long_tables(markdown, 18), markdown);
}

#[test]
fn chunks_are_separated_by_blank_lines() {
    let rows: Vec<String> = (1..=30).map(|n| format!("| r{n} |")).collect();
    let markdown = format!("| h |\n|---|\n{}", rows.join("\n"));
    let split = split_long_tables(&markdown, 10);
    // 30 rows / 10 = 3 chunks; between chunks must be a blank line so the
    // importer does not glue them back into one oversized table
    assert!(split.contains(
        "|

| h |"
    ));
}
