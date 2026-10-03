//! Minimal incremental parser for `text/event-stream` (SSE).
//!
//! The progress endpoint emits `event: progress` frames with a single `data:`
//! JSON line each, plus periodic `: keep-alive` comments. This parser
//! accumulates `data:` lines per event and yields the assembled payload when
//! the blank-line event boundary arrives. `event:`, `id:`, `retry:` fields
//! and comments are ignored — the API only sends one event type.

/// Incremental SSE frame parser. Feed it lines without their trailing
/// newline; it returns the JSON payload of each completed frame.
#[derive(Debug, Default)]
pub struct SseParser {
    data_lines: Vec<String>,
}

impl SseParser {
    /// Create an empty parser.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            data_lines: Vec::new(),
        }
    }

    /// Feed one line. Returns the assembled `data:` payload (multi-line data
    /// joined with `\n`, per the SSE spec) when a frame boundary is reached.
    pub fn feed(&mut self, line: &str) -> Option<String> {
        // A trailing CR can appear when the server uses CRLF line endings.
        let line = line.strip_suffix('\r').unwrap_or(line);

        if line.is_empty() {
            if self.data_lines.is_empty() {
                return None;
            }
            return Some(std::mem::take(&mut self.data_lines).join("\n"));
        }

        if let Some(data) = line.strip_prefix("data:") {
            // The spec allows one optional leading space after the colon.
            self.data_lines
                .push(data.strip_prefix(' ').unwrap_or(data).to_string());
        }
        // event:, id:, retry: fields and comments (": ...") are ignored.

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_data_frame() {
        let mut p = SseParser::new();
        assert_eq!(p.feed("event: progress"), None);
        assert_eq!(p.feed("data: {\"percent\":42}"), None);
        assert_eq!(p.feed(""), Some("{\"percent\":42}".to_string()));
    }

    #[test]
    fn ignores_keepalive_comments() {
        let mut p = SseParser::new();
        assert_eq!(p.feed(": keep-alive"), None);
        assert_eq!(p.feed(""), None);
    }

    #[test]
    fn joins_multiline_data() {
        let mut p = SseParser::new();
        assert_eq!(p.feed("data: line1"), None);
        assert_eq!(p.feed("data: line2"), None);
        assert_eq!(p.feed(""), Some("line1\nline2".to_string()));
    }

    #[test]
    fn tolerates_crlf_and_no_space_after_colon() {
        let mut p = SseParser::new();
        assert_eq!(p.feed("data:{\"a\":1}\r"), None);
        assert_eq!(p.feed("\r"), Some("{\"a\":1}".to_string()));
    }
}
