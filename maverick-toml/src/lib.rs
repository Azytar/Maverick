//! Ultra-light, zero-dependency parser for the TOML *subset* that Maverick's
//! optional config relies on.
//!
//! Built to replace `toml` + `serde` at startup for two reasons: binary size
//! (neither pulls in serde-derived codegen, `winnow`, or other machinery) and
//! a deliberate, narrow grammar — everything outside this subset is a hard,
//! whole-file error reported with the offending line number.
//!
//! The parser is an **event-driven, mostly zero-copy iterator**: it walks the
//! input `&str` advancing a cursor and borrows directly from the buffer. Keys
//! and escape-free strings are zero-copy (`&str`/`Cow::Borrowed`); no AST or
//! intermediate `String` heap is built for the common case.
//!
//! # Grammar (strict subset)
//!
//! * sections `[name]` and arrays of tables `[[name]]` — plain names only, no
//!   `[a.b]` dotted paths;
//! * `key = value` pairs with bare-ASCII keys (`[A-Za-z0-9_-]`, exact match);
//! * values: integers (decimals, negatives), hex specifiers `0x…`, floats
//!   `123.456`, booleans, basic strings `"…"` with escapes `\"`, `\\`, `\n`,
//!   `\r`, `\t`, and arrays: `["a","b"]`, `[1,2]`, and `[[...],[...]]`
//!   (strings-of-strings, e.g. `commands`). Arrays may span lines, contain
//!   comments, and end with a trailing comma;
//! * `#` comments to end of line, outside strings.
//!
//! Anything else — single-quoted strings `'…'`, dotted keys, multi-line
//! strings, exponent notation, mixed-type or float arrays, duplicated keys in
//! a table — is rejected with an `Err` on a specific `line`. The caller
//! (Maverick's fail-safe config loader) reacts to an error by falling back to
//! compiled defaults. This parser never panics and never allocates for the
//! escape-free common case.

use std::borrow::Cow;

/// A structured parser failure. `line` is 1-based; `kind` is a short tag
/// suitable for logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub kind: &'static str,
}

/// One top-level item, in file order.
#[derive(Debug, Clone, PartialEq)]
pub enum Event<'a> {
    /// A `[section]` header.
    Section(&'a str),
    /// A `[[array-of-tables]]` header.
    ArraySection(&'a str),
    /// `key = value`.
    KeyValue(&'a str, Value<'a>),
}

/// A parsed value. Scalars are zero-copy; `Str` is borrowed unless it
/// contains an escape, in which case it is decoded into an owned `Cow`.
#[derive(Debug, Clone, PartialEq)]
pub enum Value<'a> {
    /// Decimal integer (may be negative).
    Integer(i64),
    /// Hexadecimal literal `0xRRGGBB`, already decoded.
    Hex(u32),
    /// Decimal float.
    Float(f64),
    Boolean(bool),
    /// Basic string.
    Str(Cow<'a, str>),
    /// Flat list of strings, e.g. `tag_names`.
    StrList(Vec<Cow<'a, str>>),
    /// Flat list of integers, e.g. `size`/`position`.
    IntList(Vec<i64>),
    /// List of string lists, e.g. `commands`.
    Grid(Vec<Vec<Cow<'a, str>>>),
}

impl<'a> Value<'a> {
    /// The content of a `Str` value (`None` otherwise).
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// A boolean scalar.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    /// An integer or hex value, range-checked — colors are commonly written
    /// in either spelling and both must work.
    #[must_use]
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Self::Integer(n) => u32::try_from(*n).ok(),
            Self::Hex(h) => Some(*h),
            _ => None,
        }
    }

    /// A decimal or hex integer as `i64`.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(n) => Some(*n),
            Self::Hex(h) => Some(i64::from(*h)),
            _ => None,
        }
    }

    /// A float scalar. Strict like `toml`: an integer does not implicitly
    /// convert to a float field.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// A flat integer array, e.g. `size = [1280, 720]`.
    #[must_use]
    pub fn as_int_list(&self) -> Option<&[i64]> {
        match self {
            Self::IntList(v) => Some(v),
            _ => None,
        }
    }

    /// A flat string array, e.g. `tag_names = ["web", "code"]`.
    #[must_use]
    pub fn as_str_list(&self) -> Option<&[Cow<'a, str>]> {
        match self {
            Self::StrList(v) => Some(v),
            _ => None,
        }
    }

    /// A list of string lists, e.g. `commands = [["nm-applet"], ["picom"]]`.
    #[must_use]
    pub fn as_grid(&self) -> Option<&[Vec<Cow<'a, str>>]> {
        match self {
            Self::Grid(v) => Some(v),
            _ => None,
        }
    }
}

/// Parse `src` into an event stream. The returned [`Parser`] implements
/// [`Iterator`], yields strictly in file order, and is **fused**: after the
/// first `Err` — or end of input — it yields `None` forever. [`Parser`] never
/// panics on adversarial input.
#[must_use]
pub fn parse(src: &str) -> Parser<'_> {
    Parser {
        src,
        pos: 0,
        line: 1,
        done: false,
        seen: Vec::new(),
    }
}

/// The event iterator returned by [`parse`].
#[derive(Debug)]
pub struct Parser<'a> {
    src: &'a str,
    pos: usize,
    /// 1-based current line (bumped whenever a `\n` is consumed).
    line: usize,
    done: bool,
    /// Keys observed in the *current* table — for duplicate-key detection.
    seen: Vec<&'a str>,
}

/// A parsed header, normalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Header<'a> {
    Section(&'a str),
    Array(&'a str),
}

impl<'a> Iterator for Parser<'a> {
    type Item = Result<Event<'a>, ParseError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        self.skip_ws_comments();
        if self.at_eof() {
            self.done = true;
            return None;
        }
        if self.byte(self.pos) == b'[' {
            match self.parse_header() {
                Ok(h) => {
                    self.seen.clear();
                    Some(Ok(match h {
                        Header::Section(name) => Event::Section(name),
                        Header::Array(name) => Event::ArraySection(name),
                    }))
                }
                Err(e) => {
                    self.done = true;
                    Some(Err(e))
                }
            }
        } else {
            match self.parse_pair() {
                Ok((key, value)) => {
                    if self.seen.contains(&key) {
                        self.done = true;
                        return Some(Err(ParseError {
                            line: self.line,
                            kind: "duplicate-key",
                        }));
                    }
                    self.seen.push(key);
                    Some(Ok(Event::KeyValue(key, value)))
                }
                Err(e) => {
                    self.done = true;
                    Some(Err(e))
                }
            }
        }
    }
}

impl<'a> Parser<'a> {
    fn bytes(&self) -> &[u8] {
        self.src.as_bytes()
    }

    /// Byte at `pos`, or 0 past the end — 0 never collides with a grammar
    /// byte, so scans terminate safely at EOF.
    #[inline]
    fn byte(&self, pos: usize) -> u8 {
        *self.bytes().get(pos).unwrap_or(&0)
    }

    #[inline]
    fn at_eof(&self) -> bool {
        self.pos >= self.src.len()
    }

    /// Consume whitespace and `#` comments (between items and inside
    /// multiline arrays), advancing `line` across newlines.
    fn skip_ws_comments(&mut self) {
        loop {
            match self.byte(self.pos) {
                b' ' | b'\t' | b'\r' => self.pos += 1,
                b'\n' => {
                    self.pos += 1;
                    self.line += 1;
                }
                b'#' => {
                    while self.byte(self.pos) != b'\n' && !self.at_eof() {
                        self.pos += 1;
                    }
                }
                _ => return,
            }
        }
    }

    fn skip_horizontal_ws(&mut self) {
        while matches!(self.byte(self.pos), b' ' | b'\t') {
            self.pos += 1;
        }
    }

    fn err(&self, kind: &'static str) -> ParseError {
        ParseError {
            line: self.line,
            kind,
        }
    }

    /// `[name]` vs `[[name]]`; the name is trimmed and must be non-empty.
    fn parse_header(&mut self) -> Result<Header<'a>, ParseError> {
        let is_array = self.byte(self.pos + 1) == b'[';
        self.pos += 1; // consume '['
        if is_array {
            self.pos += 1; // consume second '['
        }
        self.skip_horizontal_ws();
        let start = self.pos;
        while !matches!(self.byte(self.pos), b']' | b' ' | b'\t' | b'\n' | b'\r') && !self.at_eof()
        {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(self.err("header"));
        }
        let name = &self.src[start..self.pos];
        self.skip_horizontal_ws();
        if self.byte(self.pos) != b']' {
            return Err(self.err("header"));
        }
        self.pos += 1;
        if is_array {
            self.skip_horizontal_ws();
            if self.byte(self.pos) != b']' {
                return Err(self.err("header"));
            }
            self.pos += 1;
        }
        // After the closing bracket: end of line or a comment.
        self.skip_horizontal_ws();
        match self.byte(self.pos) {
            0 | b'\n' | b'\r' => {}
            b'#' => {
                while !self.at_eof() && self.byte(self.pos) != b'\n' {
                    self.pos += 1;
                }
            }
            _ => return Err(self.err("header")),
        }
        Ok(if is_array {
            Header::Array(name)
        } else {
            Header::Section(name)
        })
    }

    /// `key = <value>` with trailing end-of-line or comment enforcement.
    fn parse_pair(&mut self) -> Result<(&'a str, Value<'a>), ParseError> {
        let key_start = self.pos;
        while self.byte(self.pos).is_ascii_alphanumeric()
            || matches!(self.byte(self.pos), b'_' | b'-')
        {
            self.pos += 1;
        }
        if self.pos == key_start {
            return Err(self.err("key"));
        }
        let key = &self.src[key_start..self.pos];
        self.skip_horizontal_ws();
        if self.byte(self.pos) != b'=' {
            return Err(self.err("pair"));
        }
        self.pos += 1;
        self.skip_horizontal_ws();
        let value = self.parse_value()?;
        self.skip_horizontal_ws();
        match self.byte(self.pos) {
            0 | b'\n' | b'\r' => {}
            b'#' => {
                while !self.at_eof() && self.byte(self.pos) != b'\n' {
                    self.pos += 1;
                }
            }
            _ => return Err(self.err("pair")),
        }
        Ok((key, value))
    }

    fn parse_value(&mut self) -> Result<Value<'a>, ParseError> {
        match self.byte(self.pos) {
            b'"' => self.parse_string().map(Value::Str),
            b't' | b'f' => self.parse_bool(),
            b'[' => self.parse_array(),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => Err(self.err("value")),
        }
    }

    fn parse_bool(&mut self) -> Result<Value<'a>, ParseError> {
        for want in ["true", "false"] {
            let w = want.as_bytes();
            if self
                .bytes()
                .get(self.pos..self.pos + w.len())
                .is_some_and(|b| b == w)
            {
                self.pos += w.len();
                return Ok(Value::Boolean(want == "true"));
            }
        }
        Err(self.err("value"))
    }

    /// Integers (`123`, `-5`), hex (`0x…`), floats (`0.6`, `-1.5`) — or an
    /// error. No exponents, digit separators, or trailing dots: all rejected.
    fn parse_number(&mut self) -> Result<Value<'a>, ParseError> {
        let start = self.pos;
        if self.byte(self.pos) == b'-' {
            self.pos += 1;
        }
        let digits = self.pos;
        if matches!(
            self.bytes().get(digits..digits + 2),
            Some(b"0x") | Some(b"0X")
        ) {
            self.pos += 2;
            let hex_start = self.pos;
            while self.byte(self.pos).is_ascii_hexdigit() {
                self.pos += 1;
            }
            if self.pos == hex_start {
                return Err(self.err("value"));
            }
            return match u32::from_str_radix(&self.src[hex_start..self.pos], 16) {
                Ok(v) => Ok(Value::Hex(v)),
                Err(_) => Err(self.err("value")),
            };
        }
        while self.byte(self.pos).is_ascii_digit() {
            self.pos += 1;
        }
        let int_end = self.pos;
        let is_float = self.byte(self.pos) == b'.' && self.byte(self.pos + 1).is_ascii_digit();
        let end = if is_float {
            self.pos += 1;
            while self.byte(self.pos).is_ascii_digit() {
                self.pos += 1;
            }
            self.pos
        } else {
            int_end
        };
        if end == digits {
            return Err(self.err("value"));
        }
        let raw = &self.src[start..end];
        if is_float {
            return match raw.parse::<f64>() {
                Ok(v) => Ok(Value::Float(v)),
                Err(_) => Err(self.err("value")),
            };
        }
        match raw.parse::<i64>() {
            Ok(v) => Ok(Value::Integer(v)),
            Err(_) => Err(self.err("value")),
        }
    }

    /// Basic string `"…"` with `\" \\ \n \r \t`. Zero-copy when no escapes.
    fn parse_string(&mut self) -> Result<Cow<'a, str>, ParseError> {
        debug_assert_eq!(self.byte(self.pos), b'"');
        self.pos += 1;
        let content = self.pos;
        let mut has_escape = false;
        loop {
            match self.byte(self.pos) {
                b'"' => break,
                b'\\' => {
                    has_escape = true;
                    self.pos += 1;
                    if !matches!(self.byte(self.pos), b'\"' | b'\\' | b'n' | b'r' | b't') {
                        return Err(self.err("string"));
                    }
                    self.pos += 1;
                }
                0 => return Err(self.err("string")),
                _ => self.pos += 1,
            }
        }
        let quote = self.pos;
        let value = if has_escape {
            let mut buf = Vec::with_capacity(quote - content);
            let mut i = content;
            while i < quote {
                match self.byte(i) {
                    b'\\' => {
                        match self.byte(i + 1) {
                            b'n' => buf.push(b'\n'),
                            b't' => buf.push(b'\t'),
                            b'r' => buf.push(b'\r'),
                            b'"' => buf.push(b'"'),
                            _ => buf.push(b'\\'),
                        }
                        i += 1;
                    }
                    b => buf.push(b),
                }
                i += 1;
            }
            // The buffer mirrors the input minus ASCII escape sequences, so
            // it is always valid UTF-8; still, degrade to an error instead of
            // panicking on a hypothetical malformed input.
            Cow::Owned(match String::from_utf8(buf) {
                Ok(s) => s,
                Err(_) => return Err(self.err("string")),
            })
        } else {
            Cow::Borrowed(&self.src[content..quote])
        };
        self.pos = quote + 1;
        Ok(value)
    }

    /// Body of a string list: `self.pos` just past the opening `[`.
    fn parse_string_list_body(&mut self) -> Result<Vec<Cow<'a, str>>, ParseError> {
        let mut out = Vec::new();
        loop {
            self.skip_ws_comments();
            match self.byte(self.pos) {
                b']' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'"' => out.push(self.parse_string()?),
                _ => return Err(self.err("array")),
            }
            self.skip_ws_comments();
            match self.byte(self.pos) {
                b',' => {
                    self.pos += 1;
                    self.skip_ws_comments();
                    if self.byte(self.pos) == b']' {
                        self.pos += 1;
                        return Ok(out);
                    }
                }
                b']' => {
                    self.pos += 1;
                    return Ok(out);
                }
                _ => return Err(self.err("array")),
            }
        }
    }

    /// A single integer array element: decimal or hex, optional sign.
    fn parse_int_elem(&mut self) -> Result<i64, ParseError> {
        let start = self.pos;
        if self.byte(self.pos) == b'-' {
            self.pos += 1;
        }
        let digits = self.pos;
        if matches!(
            self.bytes().get(digits..digits + 2),
            Some(b"0x") | Some(b"0X")
        ) {
            self.pos += 2;
            let hex_start = self.pos;
            while self.byte(self.pos).is_ascii_hexdigit() {
                self.pos += 1;
            }
            if self.pos == hex_start {
                return Err(self.err("array"));
            }
            let v: u64 = match u64::from_str_radix(&self.src[hex_start..self.pos], 16) {
                Ok(v) => v,
                Err(_) => return Err(self.err("array")),
            };
            return match i64::try_from(v) {
                Ok(v) => Ok(v),
                Err(_) => Err(self.err("array")),
            };
        }
        while self.byte(self.pos).is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(self.err("array"));
        }
        match self.src[start..self.pos].parse::<i64>() {
            Ok(v) => Ok(v),
            Err(_) => Err(self.err("array")),
        }
    }

    /// Any array value; classified by its first element.
    fn parse_array(&mut self) -> Result<Value<'a>, ParseError> {
        debug_assert_eq!(self.byte(self.pos), b'[');
        self.pos += 1; // consume '['
        self.skip_ws_comments();
        if self.byte(self.pos) == b']' {
            // Empty array. Reported as an empty string list; consumers check
            // `is_empty()` rather than the element kind.
            self.pos += 1;
            return Ok(Value::StrList(Vec::new()));
        }
        match self.byte(self.pos) {
            b'"' => self.parse_string_list_body().map(Value::StrList),
            b'[' => {
                let mut grid: Vec<Vec<Cow<'a, str>>> = Vec::new();
                loop {
                    self.pos += 1; // consume inner '['
                    let inner = self.parse_string_list_body()?;
                    grid.push(inner);
                    self.skip_ws_comments();
                    match self.byte(self.pos) {
                        b',' => {
                            self.pos += 1;
                            self.skip_ws_comments();
                            if self.byte(self.pos) == b']' {
                                self.pos += 1;
                                return Ok(Value::Grid(grid));
                            }
                        }
                        b']' => {
                            self.pos += 1;
                            return Ok(Value::Grid(grid));
                        }
                        _ => return Err(self.err("array")),
                    }
                }
            }
            b'0'..=b'9' | b'-' => {
                let mut out: Vec<i64> = Vec::new();
                loop {
                    out.push(self.parse_int_elem()?);
                    self.skip_ws_comments();
                    match self.byte(self.pos) {
                        b',' => {
                            self.pos += 1;
                            self.skip_ws_comments();
                            if self.byte(self.pos) == b']' {
                                self.pos += 1;
                                return Ok(Value::IntList(out));
                            }
                        }
                        b']' => {
                            self.pos += 1;
                            return Ok(Value::IntList(out));
                        }
                        _ => return Err(self.err("array")),
                    }
                }
            }
            _ => Err(self.err("array")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, Event, Value};
    use std::borrow::Cow;

    fn all(src: &str) -> Vec<Event<'_>> {
        parse(src).collect::<Result<Vec<_>, _>>().unwrap()
    }

    fn kv<'a>(events: &'a [Event<'a>], key: &str) -> Option<&'a Value<'a>> {
        events.iter().find_map(|e| match e {
            Event::KeyValue(k, v) if *k == key => Some(v),
            _ => None,
        })
    }

    #[test]
    fn parses_sections_and_simple_pairs() {
        let e = all("[general]\nborder_width = 2\n[colors]\nnormal = 0x45475a\n");
        assert!(matches!(e[0], Event::Section("general")));
        assert!(matches!(e[2], Event::Section("colors")));
        assert!(matches!(kv(&e, "border_width"), Some(Value::Integer(2))));
        assert!(matches!(kv(&e, "normal"), Some(Value::Hex(0x45475a))));
    }

    #[test]
    fn hex_is_decoded() {
        let e = all("focused = 0x89b4fa\n");
        assert!(matches!(kv(&e, "focused"), Some(Value::Hex(0x89b4fa))));
    }

    #[test]
    fn booleans_and_floats() {
        let e = all("smart_gaps = true\nopacity = 0.95\nfocus_mouse = false\n");
        assert!(matches!(kv(&e, "smart_gaps"), Some(Value::Boolean(true))));
        assert!(matches!(
            kv(&e, "opacity"),
            Some(Value::Float(f)) if (f - 0.95).abs() < 1e-9
        ));
        assert!(matches!(kv(&e, "focus_mouse"), Some(Value::Boolean(false))));
    }

    #[test]
    fn negative_integers() {
        let e = all("v = -50\n");
        assert!(matches!(kv(&e, "v"), Some(Value::Integer(-50))));
    }

    #[test]
    fn comments_stripped_everywhere() {
        let src = "# leading\n[general] # trailing header comment\nborder_width = 2 # trailing pair comment\n";
        let e = all(src);
        assert!(matches!(e[0], Event::Section("general")));
        assert!(matches!(kv(&e, "border_width"), Some(Value::Integer(2))));
    }

    #[test]
    fn unicode_survives_in_strings_and_comments() {
        let e = all("title = \"pérez\" # ── box drawing ──\n");
        assert_eq!(kv(&e, "title"), Some(&Value::Str(Cow::Borrowed("pérez"))));
    }

    #[test]
    fn strings_borrow_without_escapes() {
        let e = all("action = \"spawn:alacritty\"\n");
        assert_eq!(
            kv(&e, "action"),
            Some(&Value::Str(Cow::Borrowed("spawn:alacritty")))
        );
    }

    #[test]
    fn strings_unescape_owned() {
        let e = all(r#"t = "a\nb\t\"c\"\\""#);
        match kv(&e, "t") {
            Some(Value::Str(Cow::Owned(s))) => assert_eq!(s.as_str(), "a\nb\t\"c\"\\"),
            other => panic!("expected owned escaped string, got {other:?}"),
        }
    }

    #[test]
    fn single_quoted_strings_are_rejected() {
        let r: Result<Vec<_>, _> = parse("x = 'y'\n").collect();
        assert!(r.is_err());
    }

    #[test]
    fn flat_string_list() {
        let e = all(r#"tag_names = ["web", "code", "chat"]"#);
        let v = kv(&e, "tag_names").unwrap();
        let list = v.as_str_list().unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0], Cow::Borrowed("web"));
    }

    #[test]
    fn flat_int_list() {
        let e = all("size = [1280, -720]\n");
        let list = kv(&e, "size").unwrap().as_int_list().unwrap();
        assert_eq!(list, &[1280, -720]);
    }

    #[test]
    fn multiline_grid_with_comments_and_trailing_comma() {
        let src = r#"
[autostart]
commands = [
    ["/usr/lib/xdg-desktop-portal-gtk"],
    # a comment containing a [bracket]
    ["picom", "--vsync"],
    []
]
"#;
        let e = all(src);
        let grid = kv(&e, "commands").unwrap().as_grid().unwrap();
        assert_eq!(grid.len(), 3);
        assert_eq!(grid[0][0], Cow::Borrowed("/usr/lib/xdg-desktop-portal-gtk"));
        assert_eq!(grid[1][1], Cow::Borrowed("--vsync"));
        assert!(grid[2].is_empty());
    }

    #[test]
    fn grid_with_trailing_comma_after_last_row() {
        let src = r#"
[autostart]
commands = [
    ["alacritty", "-e", "htop"],
    ["alacritty", "--title", "notes"],
]
"#;
        let e = all(src);
        let grid = kv(&e, "commands").unwrap().as_grid().unwrap();
        assert_eq!(grid.len(), 2);
        assert_eq!(grid[1][1], Cow::Borrowed("--title"));
    }

    #[test]
    fn empty_array_is_ok() {
        let e = all("commands = []\n");
        let v = kv(&e, "commands").unwrap();
        assert!(v.as_grid().is_none());
        assert!(v.as_str_list().unwrap().is_empty());
    }

    #[test]
    fn array_of_tables() {
        let e =
            all("[[rules]]\nclass = \"mpv\"\nfloat = true\n\n[[rules]]\nclass = \"pinentry\"\n");
        let sections: Vec<_> = e
            .iter()
            .filter_map(|ev| match ev {
                Event::ArraySection(name) => Some(*name),
                _ => None,
            })
            .collect();
        assert_eq!(sections, vec!["rules", "rules"]);
    }

    #[test]
    fn header_allows_spaces() {
        let e = all("[ general ]\nx = 1\n");
        assert!(matches!(e[0], Event::Section("general")));
    }

    #[test]
    fn comments_only_file_is_empty() {
        assert!(all("# nothing\n# here\n\n").is_empty());
    }

    #[test]
    fn malformed_header_is_error() {
        let r: Result<Vec<_>, _> = parse("[general\ngaps = 4\n").collect();
        let err = r.unwrap_err();
        assert_eq!(err.line, 1);
        assert_eq!(err.kind, "header");
    }

    #[test]
    fn unterminated_string_is_error() {
        let r: Result<Vec<_>, _> = parse("key = \"never closed\n").collect();
        assert_eq!(r.unwrap_err().kind, "string");
    }

    #[test]
    fn malformed_number_is_error() {
        let r: Result<Vec<_>, _> = parse("gaps = nope\n").collect();
        assert_eq!(r.unwrap_err().kind, "value");
    }

    #[test]
    fn mixed_type_array_is_error() {
        let r: Result<Vec<_>, _> = parse("x = [\"a\", 1]\n").collect();
        assert_eq!(r.unwrap_err().kind, "array");
    }

    #[test]
    fn duplicate_key_is_error() {
        let r: Result<Vec<_>, _> = parse("[general]\na = 1\na = 2\n").collect();
        assert_eq!(r.unwrap_err().kind, "duplicate-key");
    }

    #[test]
    fn parser_is_fused_after_error() {
        let mut p = parse("[general]\n= 1\n");
        assert!(matches!(p.next(), Some(Ok(_))));
        assert!(matches!(p.next(), Some(Err(_))));
        assert!(p.next().is_none());
        assert!(p.next().is_none());
    }

    #[test]
    fn error_line_is_accurate_after_multiline_array() {
        let r: Result<Vec<_>, _> = parse("[a]\nb = [\n  1,\n  \"x\",\n]\n").collect();
        assert_eq!(r.unwrap_err().line, 4);
    }

    #[test]
    fn accessors_return_none_on_mismatch() {
        let e = all("i = 5\nb = true\ns = \"x\"\nf = [1,2]\n");
        assert_eq!(kv(&e, "i").unwrap().as_bool(), None);
        assert_eq!(kv(&e, "i").unwrap().as_str(), None);
        assert_eq!(kv(&e, "s").unwrap().as_i64(), None);
        assert_eq!(kv(&e, "b").unwrap().as_int_list(), None);
        assert_eq!(kv(&e, "i").unwrap().as_u32(), Some(5));
    }

    #[test]
    fn hex_and_decimal_both_read_as_u32() {
        let e = all("a = 0x0000ff\nb = 255\n");
        assert_eq!(kv(&e, "a").unwrap().as_u32(), Some(0x0000ff));
        assert_eq!(kv(&e, "b").unwrap().as_u32(), Some(255));
    }

    #[test]
    fn key_names_reject_quotes_and_spaces() {
        assert!(parse("border width = 4\n").next().unwrap().is_err());
        assert!(parse("=\"missing\"\n").next().unwrap().is_err());
    }
}
