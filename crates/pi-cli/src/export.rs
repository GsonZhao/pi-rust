//! Session file export helpers.
//!
//! Native Pi accepts a JSONL session file and writes an HTML transcript. The
//! Rust session format is already JSONL, so this module keeps the source lines
//! intact for `.jsonl` destinations and provides a dependency-free HTML view
//! for browser inspection.

use std::path::Path;

pub fn export_file(input: &Path, output: &Path) -> Result<(), String> {
    let source = std::fs::read_to_string(input)
        .map_err(|error| format!("could not read session {}: {error}", input.display()))?;
    if output
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
    {
        std::fs::write(output, source.as_bytes())
            .map_err(|error| format!("could not write export {}: {error}", output.display()))?;
        return Ok(());
    }

    let mut body = String::new();
    for line in source.lines().filter(|line| !line.trim().is_empty()) {
        let display = serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|value| serde_json::to_string_pretty(&value).ok())
            .unwrap_or_else(|| line.to_string());
        body.push_str("<pre>");
        body.push_str(&escape_html(&display));
        body.push_str("</pre>\n");
    }
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Pi session</title><style>body{{font:14px ui-monospace,monospace;background:#111;color:#eee;padding:24px}}pre{{white-space:pre-wrap;border-bottom:1px solid #444;padding:12px 0}}</style></head><body><h1>Pi session</h1>{body}</body></html>"
    );
    std::fs::write(output, html.as_bytes())
        .map_err(|error| format!("could not write export {}: {error}", output.display()))
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_jsonl_as_html_with_escaped_content() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("session.jsonl");
        let output = dir.path().join("session.html");
        std::fs::write(&input, "{\"text\":\"<hello>\"}\n").unwrap();
        export_file(&input, &output).unwrap();
        let html = std::fs::read_to_string(output).unwrap();
        assert!(html.contains("&lt;hello&gt;"));
        assert!(!html.contains("<hello>"));
    }

    #[test]
    fn jsonl_destination_preserves_source() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("session.jsonl");
        let output = dir.path().join("copy.jsonl");
        let source = "not-json-but-valid-session-line\n";
        std::fs::write(&input, source).unwrap();
        export_file(&input, &output).unwrap();
        assert_eq!(std::fs::read_to_string(output).unwrap(), source);
    }
}
