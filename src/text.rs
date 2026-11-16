//! Text cleanup and segment bookkeeping shared by the capture code and the
//! result window.

/// Normalizes all line endings to LF.
pub fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Replaces exotic Unicode spaces with a regular ASCII space.
pub fn unicode_spaces_to_ascii(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{00A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

/// Cleans one text fragment obtained from an accessibility provider.
///
/// * removes embedded NUL characters
/// * normalizes line endings
/// * trims trailing whitespace and leading/trailing blank lines
///
/// Leading indentation is preserved on purpose: multiline control contents
/// often rely on it.
pub fn clean_segment(s: &str) -> String {
    let s = s.replace('\0', "");
    let s = normalize_newlines(&s);
    let s = s.trim_end();
    let s = s.trim_start_matches('\n');
    s.trim_end().to_string()
}

/// Removes empty and duplicate fragments while keeping the original order.
pub fn dedupe_segments(segments: Vec<String>) -> Vec<String> {
    let mut result: Vec<String> = Vec::with_capacity(segments.len());
    for segment in segments {
        let segment = clean_segment(&segment);
        if segment.is_empty() {
            continue;
        }
        if result.iter().any(|existing| existing == &segment) {
            continue;
        }
        result.push(segment);
    }
    result
}

/// Joins fragments with a blank line and returns the text together with the
/// UTF-16 offset of every fragment. The offsets are used by the result window
/// to implement Tab-based fragment navigation in the edit control.
pub fn join_segments(segments: &[String]) -> (String, Vec<usize>) {
    let mut text = String::new();
    let mut offsets = Vec::with_capacity(segments.len());

    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            text.push_str("\r\n\r\n");
        }
        offsets.push(text.encode_utf16().count());
        text.push_str(&segment.replace('\n', "\r\n"));
    }

    (text, offsets)
}

/// Number of UTF-16 code units in a string.
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_line_endings() {
        assert_eq!(normalize_newlines("a\r\nb\rc\nd"), "a\nb\nc\nd");
    }

    #[test]
    fn cleans_fragments() {
        assert_eq!(clean_segment("  hello  \r\n\r\n"), "  hello");
        assert_eq!(clean_segment("\r\n\r\nhello"), "hello");
        assert_eq!(clean_segment("\0\0"), "");
        assert_eq!(clean_segment("a\0b"), "ab");
    }

    #[test]
    fn dedupes_segments() {
        let segments = vec![
            "one".to_string(),
            "one".to_string(),
            "  ".to_string(),
            "two".to_string(),
        ];
        assert_eq!(dedupe_segments(segments), vec!["one", "two"]);
    }

    #[test]
    fn joins_with_blank_line_and_offsets() {
        let segments = vec!["abc".to_string(), "Привет".to_string()];
        let (text, offsets) = join_segments(&segments);

        assert_eq!(text, "abc\r\n\r\nПривет");
        assert_eq!(offsets, vec![0, 7]);

        let units: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(String::from_utf16_lossy(&units[7..]), "Привет");
    }

    #[test]
    fn converts_unicode_spaces() {
        assert_eq!(
            unicode_spaces_to_ascii("a\u{00A0}b\u{2009}c\u{3000}d"),
            "a b c d"
        );
    }
}
