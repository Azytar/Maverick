//! Properties of `json`, the single escaper every Maverick payload goes through.
//!
//! The instance ficha, the `state` snapshot and the `subscribe` event lines are
//! all assembled with `json_quote`, and the control protocol reads them back
//! line by line. Two obligations therefore have to hold for *every* text the WM
//! can be asked to carry: the encoder may only emit bytes a JSON string may
//! legally contain, and it has to be reversible, because the ficha reader
//! recovers its fields by unescaping.

mod common;

use common::{escape_soup, regex_s};
use maverick_sys::json::{json_escape, json_quote, json_unescape};
use proptest::prelude::*;

/// A short JSON string value, biased towards the characters that drive the
/// escaping table. Kept short so shrinking stays cheap.
fn json_text() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => common::text(),
        2 => regex_s("[\"\\/bfnrtu\x00-\x1f]{0,16}"),
    ]
}

/// Reject anything that would end the string early or smuggle a raw control
/// byte into a document a line-framed reader has to parse: an unescaped quote,
/// a backslash that does not start a complete escape, or a C0 character.
fn assert_grammar_safe(body: &str) -> Result<(), TestCaseError> {
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c == '"' {
            return Err(TestCaseError::fail(format!("unescaped quote in {body:?}")));
        }
        if (c as u32) < 0x20 {
            return Err(TestCaseError::fail(format!(
                "raw control char {c:?} in {body:?}"
            )));
        }
        if c == '\\' {
            match chars.next() {
                Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') => {}
                other => {
                    return Err(TestCaseError::fail(format!(
                        "backslash followed by {other:?} in {body:?}"
                    )))
                }
            }
        }
    }
    Ok(())
}

// The escaper is what makes a field recoverable: `parse_meta` unescapes each
// value it reads back, so a dropped escape silently corrupts the instance a
// tool believes it is talking to.
proptest! {
    #[test]
    fn escape_unescape_roundtrip_is_identity(s in json_text()) {
        prop_assert_eq!(json_unescape(&json_escape(&s)), s);
    }
}

// `json_quote` is the form the wire actually uses, and a reader that takes the
// token's interior verbatim must unescape it to recover the payload
// (`identity::unquote` decodes escapes for exactly this reason), so the token
// must keep its payload intact between the delimiter quotes.
proptest! {
    #[test]
    fn quoted_token_delimits_and_roundtrips_its_payload(s in json_text()) {
        let token = json_quote(&s);
        prop_assert!(token.len() >= 2, "token too short to hold a payload: {token:?}");
        prop_assert!(token.starts_with('"') && token.ends_with('"'), "token is not quoted: {token:?}");
        let body = &token[1..token.len() - 1];
        prop_assert_eq!(json_unescape(body), s);
    }
}

// The encoded form has to be parsable by a strict reader: a raw quote would end
// the value early, a raw C0 byte is illegal inside a JSON string, and a
// backslash that is not followed by a complete escape corrupts everything after
// it. The control protocol is line framed on top of that: a payload carrying a
// raw newline would desync every client reading replies with `read_line`, and
// the server should not have to flatten what the encoder already made safe.
proptest! {
    #[test]
    fn encoded_text_never_breaks_the_string_grammar(s in json_text()) {
        let body = json_escape(&s);
        assert_grammar_safe(&body)?;
        let token = json_quote(&s);
        assert_grammar_safe(&token[1..token.len() - 1])?;
        // The quoted form adds nothing but the two delimiters.
        prop_assert_eq!(token.len(), body.len() + 2);
        prop_assert!(
            !token.contains(['\n', '\r']),
            "encoded payload would break line framing: {token:?}"
        );
    }
}

// The decoder is the only thing standing between a hand-edited ficha and the
// discovery scan, so it has to accept anything at all and settle into a form it
// reproduces exactly: one pass normalises every escape the encoder can produce
// (and every malformed one it cannot), and a second pass is the identity.
proptest! {
    #[test]
    fn unescape_normalises_adversarial_input(s in escape_soup()) {
        let once = json_unescape(&s);
        prop_assert_eq!(json_unescape(&json_escape(&once)), once);
    }
}

// A `\uXXXX` escape either denotes a code point or it is malformed. Decodable
// ones must yield exactly that character; surrogates have no `char` and must be
// left verbatim, as `json_unescape` documents, rather than replaced or dropped.
proptest! {
    #[test]
    fn unicode_escapes_decode_by_contract(cp in 0u32..=0xffff, upper in any::<bool>()) {
        let text = if upper { format!("\\u{cp:04X}") } else { format!("\\u{cp:04x}") };
        match char::from_u32(cp) {
            Some(ch) => prop_assert_eq!(json_unescape(&text), ch.to_string()),
            None => prop_assert_eq!(json_unescape(&text), text, "malformed escape must survive verbatim"),
        }
    }
}
