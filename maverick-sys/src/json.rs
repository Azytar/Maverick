// maverick-sys/src/json.rs
// Minimal JSON string helpers shared by the whole project (no serde
// dependency). One canonical copy replaces the three duplicated escapers that
// used to live in `identity::serde_free_json`, `control::json_esc` and
// `maverick/src/core/ipc.rs::esc`.

/// Escape `s` for use inside a JSON string literal, WITHOUT adding the
/// surrounding quotes. Quotes, backslashes and C0 control characters are
/// escaped (`\uXXXX` for the rest of the control range).
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            _ => out.push(c),
        }
    }
    out
}

/// Return `s` as a complete JSON string token (escaped, with quotes).
pub fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    out.push_str(&json_escape(s));
    out.push('"');
    out
}

/// Inverse of `json_escape`/`json_quote`: decode a JSON string (with or without
/// its surrounding quotes) back to the original text. Lenient: an unknown
/// escape is copied verbatim, and a malformed `\uXXXX` is left as-is.
pub fn json_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('b') => out.push('\u{0008}'),
                Some('f') => out.push('\u{000c}'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(cp) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(cp) {
                            out.push(ch);
                            continue;
                        }
                    }
                    out.push_str(&format!("\\u{hex}"));
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_covers_the_usual_specials() {
        assert_eq!(json_escape("a\"b\\c\nd\te"), "a\\\"b\\\\c\\nd\\te");
        assert_eq!(json_quote("x"), "\"x\"");
    }

    #[test]
    fn control_characters_use_unicode_escapes() {
        assert_eq!(json_escape("\u{07}"), "\\u0007");
    }

    #[test]
    fn unescape_handles_unicode_escapes() {
        assert_eq!(json_unescape("\\u00e9"), "é");
        assert_eq!(json_unescape("\\u0041\\u0042"), "AB");
    }

    #[test]
    fn escape_unescape_roundtrip() {
        for s in [
            "plain",
            "with \"quotes\" and \\slashes\\",
            "tab\there\n",
            "é👍",
        ] {
            assert_eq!(json_unescape(&json_escape(s)), s);
            let quoted = json_quote(s);
            // json_quote adds the surrounding quotes; unescape expects the
            // body without them (matching `identity::unquote`).
            assert_eq!(json_unescape(&quoted[1..quoted.len() - 1]), s);
        }
    }
}
