//! Optional TOML overlay on the compiled baseline — never fatal.
//!
//! # Pipeline
//!
//! ```text
//! compiled_config()                        // compiled baseline (config.rs)
//!        │  ▲
//!        │  └────────────── merge_config(baseline, UserConfig) → Cfg
//!        ▼                                ▲
//! load_from_path ── read_to_string ── parse_user(Event stream) ──► UserConfig
//! ```
//!
//! `parse_user` turns `maverick-toml`'s strict event stream into a transient
//! `UserConfig` whose every field is an `Option`; `merge_config` folds that
//! overlay onto an owned `Cfg` and returns it together with `Diagnostics`.
//! `load_config` wraps that with XDG path resolution and `dump_diagnostics`.
//! The resulting `Cfg` reaches `Engine::new` at startup and `Engine::reload`
//! after a reload.
//!
//! # Precedence
//!
//! The overlay is per *field*, not per table or file: a user entry overrides
//! only the keys it actually sets, and everything else keeps the compiled
//! value. The order-sensitive cases are:
//!
//! * `[general].theme` is applied before `[colors]`, so an explicit color
//!   always beats the palette the theme selected.
//! * `[[keybindings]]`, `[[rules]]` and `[autostart].commands` *replace* the
//!   compiled lists wholesale. A file with no `[[keybindings]]`/`[[rules]]`
//!   keeps the compiled keymap/rules; a file that declares one of them drops
//!   the compiled list entirely (there is no "append" or "clear" form).
//! * Generated workspace binds are appended last within the keybind list, and
//!   only into unclaimed slots (see `append_numeric_keybindings`).
//!
//! # Failure policy
//!
//! Config never aborts startup. A missing file is silent; a syntax error
//! discards the whole file and returns the compiled baseline; a semantic fault
//! drops only the offending value or entry and is recorded in `Diagnostics`.
//! Callers decide what a diagnostic means:
//! `load_config` logs and continues, `--check-config` turns it into an exit
//! code, and `reload_config` goes further — it asks for the [`ConfigSource`]
//! first, because a compiled baseline returned for a missing or unparseable file
//! is not the configuration the session is already running, so it keeps that one
//! and only warns (see `load_from_path_classified`).
//!
//! What is reported is scoped to what Maverick can name. A key inside a table
//! it knows (`[general]`, `[colors]`, `[autostart]`, `[[keybindings]]`,
//! `[[rules]]`) with no field behind it is a warning: the file still loads, but
//! the user is told which setting did not take. A *table* it has never heard of
//! is skipped without a word, because that is the shape a config written for a
//! newer Maverick takes, and refusing to load it (or nagging about it) would
//! break forward compatibility for no benefit.

use std::path::{Path, PathBuf};

use maverick_toml::{parse, Event, ParseError, Value};
use x11rb::protocol::xproto::ModMask;

use crate::config::{compiled_config, Cfg, Rule};
use crate::log;
use crate::types::Action;

/// Accumulated config diagnostics, separated from logging so a caller can
/// decide what to do with them. The boot path and `reload_config` dump both
/// lists via `log::warn` and carry on, while `--check-config` inspects them to
/// decide its exit code.
///
/// * `errors`   — a value that could not be applied at all: unknown keysym,
///   unknown action, binding conflict, workspace out of range, unknown
///   `window_type`, numeric value out of range, rule with no criteria.
/// * `warnings` — a value that was accepted-but-degraded: deprecated alias,
///   wrong-typed field, unknown key in a known table, empty entry discarded,
///   unknown theme.
///
/// A warning still makes `--check-config` exit non-zero (`is_clean` requires
/// both lists empty), so "the file loads" is never mistaken for "the file is
/// what you wrote".
#[derive(Debug, Default, Clone)]
pub struct Diagnostics {
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

impl Diagnostics {
    /// True when there is nothing worth reporting (used by the `--check-config`
    /// contract test and exit-code decision).
    pub fn is_clean(&self) -> bool {
        self.warnings.is_empty() && self.errors.is_empty()
    }
}

/// Emit every diagnostic via `log::warn`; the runtime path keeps going
/// regardless, so a broken config costs a warning, never a session.
pub fn dump_diagnostics(diag: &Diagnostics) {
    for w in &diag.warnings {
        log::warn!("config: {w}");
    }
    for e in &diag.errors {
        log::warn!("config: {e}");
    }
}
#[derive(Debug, Default)]
struct UserConfig {
    general: Option<GeneralCfg>,
    colors: Option<ColorsCfg>,
    keybindings: Vec<KeybindEntry>,
    rules: Vec<RuleEntry>,
    autostart: Option<AutostartCfg>,
}

#[derive(Debug, Default)]
struct GeneralCfg {
    border_width: Option<u32>,
    /// Legacy alias: sets both `gaps_inner` and `gaps_outer` at once.
    gaps: Option<u32>,
    gaps_inner: Option<u32>,
    gaps_outer: Option<u32>,
    smart_gaps: Option<bool>,
    corner_radius: Option<u32>,
    /// Named color-scheme preset (see `config::theme_palette`). Applied
    /// before `[colors]`, so an explicit `[colors]` entry always wins over
    /// whatever the theme sets.
    theme: Option<String>,
    n_tags: Option<usize>,
    /// New-style column width: fraction (0.1–1.0) of the workarea given to a
    /// freshly created column. Replaces the old `default_col_width` (px) and
    /// `split_bias` (fraction) keys, which are kept as deprecated aliases.
    column_width: Option<f32>,
    /// Legacy alias: pixel column width, converted to `column_width` against a
    /// fixed 1920px workarea (no monitor is known at parse time); emits a
    /// deprecation warning.
    default_col_width: Option<u32>,
    /// Legacy alias: fraction of the workarea for a new column. Maps directly
    /// onto `column_width`; emits a deprecation warning.
    split_bias: Option<f32>,
    /// Accordion focus-expansion factor (0.0–0.9), see `Cfg::accordion_boost`.
    accordion_boost: Option<f32>,
    /// Overview zoom-out floor (0.05–1.0): the smallest scale Overview may use,
    /// see `Cfg::overview_zoom_min`.
    overview_zoom_min: Option<f32>,
    /// Overview entry scale (0.05–1.0): the scale the mode is entered at, see
    /// `Cfg::overview_scale`.
    overview_scale: Option<f32>,
    /// Auto-generate `Super+1..n` / `Super+Shift+1..n` workspace binds.
    /// Defaults to `true`; when `false` no workspace binds are added.
    auto_workspace_binds: Option<bool>,
    focus_mouse: Option<bool>,
    warp_cursor: Option<bool>,
    tag_names: Option<Vec<String>>,
    /// Global policy for the client's map-time `_NET_WM_STATE`. `false` (the
    /// default) normalizes away a window's initial maximized/fullscreen request
    /// so it opens as a normal tile; `true` honours it. See `Cfg::honor_initial_state`.
    honor_initial_state: Option<bool>,
}

#[derive(Debug, Default)]
struct ColorsCfg {
    normal: Option<u32>,
    focused: Option<u32>,
    urgent: Option<u32>,
}

#[derive(Debug, Default)]
struct KeybindEntry {
    key: String,
    action: String,
}

#[derive(Debug, Default)]
struct RuleEntry {
    class: Option<String>,
    instance: Option<String>,
    window_type: Option<String>,
    title: Option<String>,
    float: bool,
    sticky: bool,
    workspace: Option<usize>,
    /// `[width, height]` in pixels for forced floating size.
    size: Option<[u32; 2]>,
    /// `[x, y]` relative to the monitor workarea origin, for forced position.
    position: Option<[i32; 2]>,
    /// 0.0-1.0, not validated here: `manage::apply_rules` clamps it when the
    /// `_NET_WM_WINDOW_OPACITY` property is written, so a typo degrades the
    /// window instead of discarding the whole rule.
    opacity: Option<f32>,
    border_width: Option<u32>,
    /// Forces the window's map-time `_NET_WM_STATE` to be normalized away.
    /// See `config::Rule::ignore_initial_state`.
    ignore_initial_state: bool,
    /// Per-rule override of the global `honor_initial_state` policy: `Some(true)`
    /// honours this window's map-time state, `Some(false)` normalizes it away,
    /// `None` defers to the global config. See `config::Rule::honor_initial_state`.
    honor_initial_state: Option<bool>,
    /// Drop the app's own runtime fullscreen requests (F11 / EWMH); `Mod4+Shift+F`
    /// is unaffected. See `config::Rule::deny_fullscreen`.
    deny_fullscreen: bool,
    /// Exclusive, ribbon-free fullscreen for games. Wins over
    /// `deny_fullscreen`; see `config::Rule::true_fullscreen`.
    true_fullscreen: bool,
}

#[derive(Debug, Default)]
struct AutostartCfg {
    commands: Vec<Vec<String>>,
}

/// The current top-level table the event stream is inside. `Plain` tables are
/// `[general]`-style singletons; `Rows` tables are `[[keybindings]]`/`[[rules]]`
/// arrays whose keys append to the most recent entry.
#[derive(Debug, Clone, Copy)]
enum Cur<'a> {
    Plain(&'a str),
    Row(&'a str),
}

/// Return Maverick's XDG config path without requiring it to exist: an empty
/// `XDG_CONFIG_HOME` is treated as unset, and the `HOME` fallback only
/// produces a path, never the directory or the file.
pub fn config_path() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("maverick/config.toml"));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| PathBuf::from(home).join(".config/maverick/config.toml"))
}

/// Load the user config from `path` (when `None`, falls back to the standard
/// XDG path; if even that is missing, the compiled defaults are used). All
/// diagnostics are logged and startup proceeds regardless — config is never
/// fatal.
pub fn load_config(path: Option<&Path>) -> Cfg {
    let Some(path) = path.map(Path::to_path_buf).or_else(config_path) else {
        let cfg = default_config();
        log::config_snapshot("normalized_no_path", &cfg);
        return cfg;
    };
    let (cfg, diag) = load_from_path(&path);
    dump_diagnostics(&diag);
    cfg
}

/// The complete fail-safe default: the compiled baseline plus the auto-generated
/// numeric workspace keybindings (`Super+1..n` → view, `Super+Shift+1..n` → move).
///
/// Every fallback path in the loader (no path, missing file, unreadable file,
/// broken TOML) returns this, so a WM that never reads a config file still
/// starts with a reachable workspace keymap. A file that *does* parse is never
/// merged onto it: `merge_config` takes [`compiled_config`] as its baseline and
/// adds the generated binds itself, so they reflect the file's `n_tags` and
/// `auto_workspace_binds`. The compiled baseline therefore stays free of
/// workspace binds — they are generated, never configured.
fn default_config() -> Cfg {
    let mut cfg = compiled_config();
    append_numeric_keybindings(&mut cfg.keybinds, cfg.n_tags);
    cfg
}

/// Why a load ended the way it did — information `Diagnostics` cannot carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    /// The file was read and parsed: the returned `Cfg` is the compiled
    /// baseline with the file's overlay merged onto it. Values the file got
    /// wrong are dropped and recorded in `Diagnostics` as always, so a non-empty
    /// `errors` list does NOT make this variant.
    UserFile,
    /// The file is missing, unreadable, or not valid TOML: the returned `Cfg`
    /// is the compiled baseline (`default_config`), and it is NOT the user's
    /// configuration.
    CompiledBaseline,
}

/// Parse `path` into a `Cfg`, the full `Diagnostics`, and the [`ConfigSource`]
/// the load ended on. On a missing file the compiled defaults are returned with
/// an empty diagnostic; on a read or syntax error the defaults are returned with
/// the error recorded; on semantic issues the offending entries are dropped and
/// reported. Never panics and never returns `None`.
///
/// The source is not a restatement of the diagnostic — it is the one fact the
/// diagnostic cannot express. A missing file is *clean*, and a file that parsed
/// but had a value rejected reports that through `errors` while the rest of the
/// file is merged in, so neither `is_clean()` nor `!errors.is_empty()`
/// identifies the compiled baseline. Startup (`load_config`, `--check-config`)
/// takes the `Cfg` either way, because falling back to compiled defaults is what
/// it is for.
pub fn load_from_path_classified(path: &Path) -> (ConfigSource, Cfg, Diagnostics) {
    log::config_trace("defaults_start", format_args!("path={}", path.display()));
    let fallback = default_config();
    log::config_trace("defaults_end", format_args!(""));
    let mut diag = Diagnostics::default();

    log::config_trace("config_read_start", format_args!("path={}", path.display()));
    let read_result = std::fs::read_to_string(path);
    log::config_trace(
        "config_read_end",
        format_args!("ok={}", read_result.is_ok()),
    );
    let source = match read_result {
        Ok(source) => source,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::config_snapshot("normalized_missing_file", &fallback);
            return (ConfigSource::CompiledBaseline, fallback, diag);
        }
        Err(e) => {
            diag.errors.push(format!(
                "cannot read '{}': {e}; using compiled defaults",
                path.display()
            ));
            return (ConfigSource::CompiledBaseline, fallback, diag);
        }
    };

    if log::config_trace_enabled() {
        log::config_trace(
            "source_identity",
            format_args!(
                "bytes={} fingerprint={:016x}",
                source.len(),
                log::config_fingerprint(source.as_bytes())
            ),
        );
    }
    log::config_trace(
        "deserialize_start",
        format_args!("parser=maverick_toml_event_stream"),
    );
    let parsed = parse_user(&source, &mut diag);
    log::config_trace(
        "deserialize_end",
        format_args!(
            "ok={} warnings={} errors={}",
            parsed.is_ok(),
            diag.warnings.len(),
            diag.errors.len()
        ),
    );
    let user = match parsed {
        Ok(user) => user,
        Err(e) => {
            diag.errors.push(format!(
                "invalid TOML in '{}' (line {}, {}); using compiled defaults",
                path.display(),
                e.line,
                e.kind
            ));
            return (ConfigSource::CompiledBaseline, fallback, diag);
        }
    };

    log::config_trace("validation_start", format_args!("phase=semantic_merge"));
    // The merge baseline is the compiled keymap, NOT the fail-safe `fallback`
    // above: `merge_config` generates the workspace binds itself, for the
    // `n_tags` the file ended up with and only when `auto_workspace_binds`
    // allows it. Starting from `fallback` would pre-seed a bind for every digit
    // of the compiled tag count, which no flag could then take away — the
    // generated binds are indistinguishable from the user's own once merged in.
    let cfg = merge_config(compiled_config(), user, &mut diag);
    log::config_trace(
        "validation_end",
        format_args!(
            "warnings={} errors={}",
            diag.warnings.len(),
            diag.errors.len()
        ),
    );
    log::config_snapshot("normalized_config", &cfg);
    (ConfigSource::UserFile, cfg, diag)
}

/// The BOOT view of [`load_from_path_classified`]: the `Cfg` and its
/// `Diagnostics`, with the [`ConfigSource`] thrown away. At startup a fallback to
/// the compiled baseline is the intended outcome, not a fault, so nothing needs
/// the distinction.
///
/// The reload path must NOT use this view. A reload *replaces* the
/// configuration that is already live, and a compiled baseline returned for a
/// missing, unreadable or unparseable file is not that configuration — adopting
/// it would silently swap the user's keybinds, rules, theme and tag count for
/// the compiled ones. `reload_config` therefore calls
/// [`load_from_path_classified`] and keeps the configuration it is already
/// running when the source is [`ConfigSource::CompiledBaseline`].
pub fn load_from_path(path: &Path) -> (Cfg, Diagnostics) {
    let (_source, cfg, diag) = load_from_path_classified(path);
    (cfg, diag)
}

/// Consume the strict TOML-subset event stream and build the user model.
/// A `ParseError` here means the whole file is rejected by the caller.
/// Type-mismatch warnings discovered while mapping keys are accumulated into
/// `diag` (they are isolated to the offending entry, never fatal).
fn parse_user(source: &str, diag: &mut Diagnostics) -> Result<UserConfig, ParseError> {
    let mut user = UserConfig::default();
    let mut cur: Option<Cur<'_>> = None;

    for event in parse(source) {
        let event = event?;
        match event {
            Event::Section(name) => cur = Some(Cur::Plain(name)),
            Event::ArraySection(name) => {
                match name {
                    "keybindings" => user.keybindings.push(KeybindEntry::default()),
                    "rules" => user.rules.push(RuleEntry::default()),
                    _ => {}
                }
                cur = Some(Cur::Row(name));
            }
            Event::KeyValue(key, value) => match cur {
                Some(Cur::Plain("general")) => {
                    let g = user.general.get_or_insert_with(GeneralCfg::default);
                    apply_general_key(g, key, &value, diag);
                }
                Some(Cur::Plain("colors")) => {
                    let c = user.colors.get_or_insert_with(ColorsCfg::default);
                    apply_color_key(c, key, &value, diag);
                }
                Some(Cur::Plain("autostart")) => {
                    if matches!(key, "commands" | "apps" | "programs") {
                        user.autostart.get_or_insert_with(AutostartCfg::default);
                        if let Some(grid) = grid_strings(&value) {
                            user.autostart.as_mut().unwrap().commands = grid;
                        } else {
                            diag.warnings.push(format!(
                                "[autostart].{key} must be a list of string lists; ignoring it"
                            ));
                        }
                    } else {
                        warn_unknown_key(diag, "[autostart]", key);
                    }
                }
                Some(Cur::Row("keybindings")) => {
                    if let Some(row) = user.keybindings.last_mut() {
                        apply_keybind_key(row, key, &value, diag);
                    }
                }
                Some(Cur::Row("rules")) => {
                    if let Some(row) = user.rules.last_mut() {
                        apply_rule_key(row, key, &value, diag);
                    }
                }
                // A key with no table open is a different mistake from a key in
                // a table Maverick has never heard of, and only the first is a
                // typo. No valid config places a key at file scope, so say so
                // rather than dropping it on the floor the way an unknown table
                // is dropped.
                None => diag.warnings.push(format!(
                    "key '{key}' appears before any [table] header; ignoring it"
                )),
                // Unknown sections and rows are ignored: a config written for
                // a newer Maverick must still load on an older one. That is
                // also where `[compositor]` and `[animations]` land — the
                // subsystems they configured no longer exist, so there is no
                // key of theirs to honour and no reason to name them here.
                // The trade is deliberate: a whole table Maverick has never
                // heard of is a forward-compatibility case, while a key inside
                // a table it *does* know is a typo only the user can fix — so
                // that one is reported (see `warn_unknown_key`).
                _ => {}
            },
        }
    }
    Ok(user)
}

/// Map one `[general]` key onto the model. Deprecated spellings (`border_w`,
/// `default_col_w`, …) resolve to their canonical field here, a
/// wrong-typed value is skipped with a warning rather than rejecting the file,
/// and a key this table has no field for is reported instead of dropped.
fn apply_general_key(g: &mut GeneralCfg, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    match key {
        "border_width" | "border_w" => set_u32(&mut g.border_width, key, value, diag),
        "gaps" => set_u32(&mut g.gaps, key, value, diag),
        "gaps_inner" => set_u32(&mut g.gaps_inner, key, value, diag),
        "gaps_outer" => set_u32(&mut g.gaps_outer, key, value, diag),
        "smart_gaps" => set_bool(&mut g.smart_gaps, key, value, diag),
        "corner_radius" => set_u32(&mut g.corner_radius, key, value, diag),
        "theme" => set_string(&mut g.theme, key, value, diag),
        "n_tags" => set_usize(&mut g.n_tags, key, value, diag),
        "column_width" => set_f32(&mut g.column_width, key, value, diag),
        "default_col_width" | "default_col_w" => {
            set_u32(&mut g.default_col_width, key, value, diag)
        }
        "split_bias" => set_f32(&mut g.split_bias, key, value, diag),
        "accordion_boost" => set_f32(&mut g.accordion_boost, key, value, diag),
        "overview_zoom_min" => set_f32(&mut g.overview_zoom_min, key, value, diag),
        "overview_scale" => set_f32(&mut g.overview_scale, key, value, diag),
        "auto_workspace_binds" => set_bool(&mut g.auto_workspace_binds, key, value, diag),
        "focus_mouse" => set_bool(&mut g.focus_mouse, key, value, diag),
        "warp_cursor" => set_bool(&mut g.warp_cursor, key, value, diag),
        "honor_initial_state" => set_bool(&mut g.honor_initial_state, key, value, diag),
        "tag_names" => {
            if let Some(list) = value.as_str_list() {
                g.tag_names = Some(list.iter().map(|s| s.as_ref().to_string()).collect());
            } else {
                warn_bad(diag, key);
            }
        }
        _ => warn_unknown_key(diag, "[general]", key),
    }
}

/// Map one `[colors]` key onto the model.
fn apply_color_key(c: &mut ColorsCfg, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    match key {
        "normal" | "col_normal" => set_u32(&mut c.normal, key, value, diag),
        "focused" | "col_focused" => set_u32(&mut c.focused, key, value, diag),
        "urgent" | "col_urgent" => set_u32(&mut c.urgent, key, value, diag),
        _ => warn_unknown_key(diag, "[colors]", key),
    }
}

/// Map one `[[keybindings]]` key onto the current row.
fn apply_keybind_key(row: &mut KeybindEntry, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    match key {
        "key" => row.key = value.as_str().map_or_else(String::new, str::to_string),
        "action" => row.action = value.as_str().map_or_else(String::new, str::to_string),
        _ => warn_unknown_key(diag, "[[keybindings]]", key),
    }
}

/// Map one `[[rules]]` key onto the current row.
fn apply_rule_key(row: &mut RuleEntry, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    // Rule bools are plain `bool` (not `Option`), so they need their own
    // setter: warn on a wrong type instead of silently defaulting to false,
    // which is what every neighbouring `Option` setter does.
    fn set_rule_bool(slot: &mut bool, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
        if let Some(v) = value.as_bool() {
            *slot = v;
        } else {
            warn_bad(diag, key);
        }
    }
    match key {
        "class" => set_string(&mut row.class, key, value, diag),
        "instance" => set_string(&mut row.instance, key, value, diag),
        "window_type" | "type" => set_string(&mut row.window_type, key, value, diag),
        "title" => set_string(&mut row.title, key, value, diag),
        "float" => set_rule_bool(&mut row.float, key, value, diag),
        "sticky" => set_rule_bool(&mut row.sticky, key, value, diag),
        "workspace" | "ws" => set_usize(&mut row.workspace, key, value, diag),
        "size" => {
            if let Some([w, h]) = int_pair(value) {
                row.size = Some([w as u32, h as u32]);
            } else {
                warn_bad(diag, key);
            }
        }
        "position" => {
            if let Some([x, y]) = int_pair(value) {
                row.position = Some([x as i32, y as i32]);
            } else {
                warn_bad(diag, key);
            }
        }
        "opacity" => set_f32(&mut row.opacity, key, value, diag),
        "border_width" | "border_w" => set_u32(&mut row.border_width, key, value, diag),
        "ignore_initial_state" | "no_initial_state" | "no_maximize" => {
            set_rule_bool(&mut row.ignore_initial_state, key, value, diag);
        }
        "honor_initial_state" => set_bool(&mut row.honor_initial_state, key, value, diag),
        "deny_fullscreen" | "no_fullscreen" => {
            set_rule_bool(&mut row.deny_fullscreen, key, value, diag);
        }
        "true_fullscreen" | "exclusive_fullscreen" => {
            set_rule_bool(&mut row.true_fullscreen, key, value, diag);
        }
        _ => warn_unknown_key(diag, "[[rules]]", key),
    }
}

// Typed setters: a wrong-typed value leaves the slot at `None` and records a
// warning, so a typo degrades one key instead of the whole file.

/// Parse a two-element integer array, e.g. `size`/`position`.
fn int_pair(value: &Value<'_>) -> Option<[i64; 2]> {
    let list = value.as_int_list()?;
    Some([*list.first()?, *list.get(1)?])
}

fn grid_strings(value: &Value<'_>) -> Option<Vec<Vec<String>>> {
    Some(
        value
            .as_grid()?
            .iter()
            .map(|row| row.iter().map(|s| s.as_ref().to_string()).collect())
            .collect(),
    )
}

/// Assign `key`'s `u32` value into `slot`, warning (via `diag`) when the value
/// has the wrong type (the key is then left untouched).
fn set_u32(slot: &mut Option<u32>, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    if let Some(v) = value.as_u32() {
        *slot = Some(v);
    } else {
        warn_bad(diag, key);
    }
}

fn set_bool(slot: &mut Option<bool>, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    if let Some(v) = value.as_bool() {
        *slot = Some(v);
    } else {
        warn_bad(diag, key);
    }
}

fn set_string(slot: &mut Option<String>, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    if let Some(v) = value.as_str() {
        *slot = Some(v.to_string());
    } else {
        warn_bad(diag, key);
    }
}

/// Assign `key`'s float value into `slot`. Accepts a decimal float *or* an
/// integer literal (e.g. `column_width = 1`) and coerces it to `f32` — the
/// strict TOML-subset parser keeps the two types separate, but config should
/// not force a trailing `.0` on integer-valued fractions.
fn set_f32(slot: &mut Option<f32>, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    if let Some(v) = value.as_f64() {
        *slot = Some(v as f32);
    } else if let Some(i) = value.as_i64() {
        *slot = Some(i as f32);
    } else {
        warn_bad(diag, key);
    }
}

fn set_usize(slot: &mut Option<usize>, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    if let Some(v) = value.as_u32() {
        *slot = Some(v as usize);
    } else {
        warn_bad(diag, key);
    }
}

fn warn_bad(diag: &mut Diagnostics, key: &str) {
    diag.warnings.push(format!(
        "value for '{key}' has an unexpected type; ignoring it"
    ));
}

/// Report a key that a known table has no field for. The value is accepted and
/// the rest of the file still merges, so this is a warning rather than an error:
/// what the user gets is a configuration that works minus the one setting they
/// believe they made, which is exactly the case a silent drop hides best.
/// Naming the table and the key is what makes it fixable — `honour_initial_state`
/// and `[general].honor_initial_state` are one keystroke apart.
///
/// Unknown *tables* are not reported: see `parse_user`, where forward
/// compatibility with a config written for a newer Maverick takes precedence.
fn warn_unknown_key(diag: &mut Diagnostics, section: &str, key: &str) {
    diag.warnings
        .push(format!("unknown key '{section}.{key}'; ignoring it"));
}

/// Fold [`UserConfig`] into the baseline [`Cfg`], recording per-entry
/// diagnostics. The resulting `Cfg` is what the WM runs on: it is handed to
/// `Engine::new` at startup and replaces the live one on reload.
fn merge_config(mut cfg: Cfg, user: UserConfig, diag: &mut Diagnostics) -> Cfg {
    let auto_ws = user
        .general
        .as_ref()
        .is_none_or(|g| g.auto_workspace_binds.unwrap_or(true));

    if let Some(general) = user.general {
        apply_general(&mut cfg, general, diag);
    }
    if let Some(colors) = user.colors {
        apply_colors(&mut cfg, colors, diag);
    }

    if !user.keybindings.is_empty() {
        // Any user keybinding discards the compiled keymap entirely; the
        // generated workspace binds are re-added into the free slots.
        cfg.keybinds = parse_keybindings(&user.keybindings, cfg.n_tags, auto_ws, diag);
    } else if auto_ws {
        // No user keybindings: keep the compiled binds and still synthesize the
        // workspace 1..n binds, so a config that only sets `[general]` options
        // does not lose the compiled keymap.
        append_numeric_keybindings(&mut cfg.keybinds, cfg.n_tags);
    }
    if !user.rules.is_empty() {
        cfg.rules = parse_rules(user.rules, cfg.n_tags, diag);
    }
    if let Some(autostart) = user.autostart {
        cfg.autostart = autostart
            .commands
            .into_iter()
            .filter(|cmd| {
                if cmd.first().is_some_and(|bin| !bin.trim().is_empty()) {
                    true
                } else {
                    diag.warnings
                        .push("discarded empty autostart command".into());
                    false
                }
            })
            .collect();
    }
    normalize_tag_names(&mut cfg);
    cfg
}

fn apply_general(cfg: &mut Cfg, general: GeneralCfg, diag: &mut Diagnostics) {
    if let Some(v) = general.border_width {
        cfg.border_w = v;
    }
    if let Some(v) = general.gaps {
        cfg.gaps_inner = v;
        cfg.gaps_outer = v;
    }
    if let Some(v) = general.gaps_inner {
        cfg.gaps_inner = v;
    }
    if let Some(v) = general.gaps_outer {
        cfg.gaps_outer = v;
    }
    if let Some(v) = general.smart_gaps {
        cfg.smart_gaps = v;
    }
    if let Some(v) = general.corner_radius {
        cfg.corner_radius = v;
    }
    if let Some(name) = &general.theme {
        match crate::config::theme_palette(name) {
            Some((normal, focused, urgent)) => {
                cfg.col_normal = normal;
                cfg.col_focused = focused;
                cfg.col_urgent = urgent;
            }
            None => {
                diag.warnings.push(format!(
                    "general.theme '{name}' is not a known preset; ignoring it"
                ));
            }
        }
    }
    if let Some(v) = general.n_tags {
        if (1..=9).contains(&v) {
            cfg.n_tags = v;
        } else {
            diag.errors.push(format!(
                "general.n_tags must be between 1 and 9; ignoring {v}"
            ));
        }
    }
    if let Some(v) = general.column_width {
        if (0.1..=1.0).contains(&v) {
            cfg.column_width = v;
        } else {
            diag.errors.push(format!(
                "general.column_width must be between 0.1 and 1.0; ignoring {v}"
            ));
        }
    }
    // Legacy `default_col_width` / `default_col_w` (pixels) → fraction. The
    // conversion needs a workarea width that does not exist at parse time, so
    // it assumes 1920px and then clamps into the valid fraction range.
    if let Some(px) = general.default_col_width {
        if px > 0 {
            cfg.column_width = (px as f32 / 1920.0).clamp(0.1, 1.0);
            diag.warnings.push(
                "general.default_col_width is deprecated; use [general].column_width \
                 (fraction 0.1–1.0)"
                    .into(),
            );
        } else {
            diag.errors
                .push("general.default_col_width must be greater than zero".into());
        }
    }
    // Legacy `split_bias` (fraction) → column_width. A 0.0 `split_bias` is
    // accepted here and clamped up to the 0.1 minimum, which is why this key
    // validates over 0.0–1.0 but never stores 0.0.
    if let Some(v) = general.split_bias {
        if (0.0..=1.0).contains(&v) {
            cfg.column_width = v.clamp(0.1, 1.0);
            diag.warnings.push(
                "general.split_bias is deprecated; use [general].column_width \
                 (fraction 0.1–1.0)"
                    .into(),
            );
        } else {
            diag.errors.push(format!(
                "general.split_bias must be between 0.0 and 1.0; ignoring {v}"
            ));
        }
    }
    if let Some(v) = general.accordion_boost {
        if (0.0..=0.9).contains(&v) {
            cfg.accordion_boost = v;
        } else {
            diag.errors.push(format!(
                "general.accordion_boost must be between 0.0 and 0.9; ignoring {v}"
            ));
        }
    }
    if let Some(v) = general.overview_zoom_min {
        if (0.05..=1.0).contains(&v) {
            cfg.overview_zoom_min = v;
        } else {
            diag.errors.push(format!(
                "general.overview_zoom_min must be between 0.05 and 1.0; ignoring {v}"
            ));
        }
    }
    if let Some(v) = general.overview_scale {
        if (0.05..=1.0).contains(&v) {
            cfg.overview_scale = v;
        } else {
            diag.errors.push(format!(
                "general.overview_scale must be between 0.05 and 1.0; ignoring {v}"
            ));
        }
    }
    // The pair invariant `ALPHA_MIN <= floor <= target <= 1.0` (see
    // `layout::sanitized_overview_scales`, the runtime backstop for a `Cfg`
    // assembled in code): a floor above the target would widen the entry back
    // toward the settled view the target asked to leave, silently cancelling
    // the reduction. Diagnose it and normalize the floor toward the target
    // instead of pretending the combination is valid.
    if cfg.overview_zoom_min > cfg.overview_scale {
        diag.errors.push(format!(
            "general.overview_zoom_min ({}) must not exceed general.overview_scale ({}); \
             using {} as the floor",
            cfg.overview_zoom_min, cfg.overview_scale, cfg.overview_scale
        ));
        cfg.overview_zoom_min = cfg.overview_scale;
    }
    if let Some(v) = general.focus_mouse {
        cfg.focus_mouse = v;
    }
    if let Some(v) = general.warp_cursor {
        cfg.warp_cursor = v;
    }
    if let Some(names) = general.tag_names {
        if names.is_empty() || names.iter().any(String::is_empty) {
            diag.warnings
                .push("general.tag_names must contain non-empty names; ignoring it".into());
        } else {
            cfg.tag_names = names;
        }
    }
    if let Some(v) = general.honor_initial_state {
        cfg.honor_initial_state = v;
    }
}

fn apply_colors(cfg: &mut Cfg, colors: ColorsCfg, _diag: &mut Diagnostics) {
    if let Some(v) = colors.normal {
        cfg.col_normal = v;
    }
    if let Some(v) = colors.focused {
        cfg.col_focused = v;
    }
    if let Some(v) = colors.urgent {
        cfg.col_urgent = v;
    }
}

/// Force `tag_names` to be exactly `n_tags` long: surplus names are dropped and
/// missing ones are auto-numbered, so the bar/IPC can index it by workspace
/// without a range check.
fn normalize_tag_names(cfg: &mut Cfg) {
    cfg.tag_names.truncate(cfg.n_tags);
    while cfg.tag_names.len() < cfg.n_tags {
        cfg.tag_names.push((cfg.tag_names.len() + 1).to_string());
    }
}

fn parse_rules(entries: Vec<RuleEntry>, n_tags: usize, diag: &mut Diagnostics) -> Vec<Rule> {
    entries
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            if entry.class.as_deref().is_none_or(str::is_empty)
                && entry.instance.as_deref().is_none_or(str::is_empty)
                && entry.window_type.as_deref().is_none_or(str::is_empty)
                && entry.title.as_deref().is_none_or(str::is_empty)
            {
                diag.errors.push(format!(
                    "discarded rule #{}: class, instance, window_type and title are all empty",
                    index + 1
                ));
                return None;
            }
            if let Some(wt) = entry.window_type.as_deref() {
                // The EWMH atom names only; a typo would otherwise become a
                // criterion that silently never matches.
                const KNOWN_TYPES: [&str; 8] = [
                    "normal", "desktop", "dock", "toolbar", "menu", "utility", "splash", "dialog",
                ];
                if !KNOWN_TYPES.contains(&wt.to_ascii_lowercase().as_str()) {
                    diag.errors.push(format!(
                        "discarded rule #{}: unknown window_type '{wt}'",
                        index + 1
                    ));
                    return None;
                }
            }
            // `workspace` is 1-based in the file, 0-based in `Rule::ws`; a
            // reload that shrinks `n_tags` re-clamps it at manage time.
            let ws = match entry.workspace {
                Some(ws) if ws == 0 || ws > n_tags => {
                    diag.errors.push(format!(
                        "discarded rule #{}: workspace {ws} is outside 1..={n_tags}",
                        index + 1
                    ));
                    return None;
                }
                Some(ws) => Some(ws - 1),
                None => None,
            };
            Some(Rule {
                class: entry.class.filter(|s| !s.is_empty()),
                instance: entry.instance.filter(|s| !s.is_empty()),
                window_type: entry
                    .window_type
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_ascii_lowercase()),
                title: entry.title.filter(|s| !s.is_empty()),
                float: entry.float,
                sticky: entry.sticky,
                ws,
                size: entry.size.map(|[w, h]| (w, h)),
                position: entry.position.map(|[x, y]| (x, y)),
                opacity: entry.opacity,
                border_w: entry.border_width,
                ignore_initial_state: entry.ignore_initial_state,
                deny_fullscreen: entry.deny_fullscreen,
                true_fullscreen: entry.true_fullscreen,
                honor_initial_state: entry.honor_initial_state,
            })
        })
        .collect()
}

/// Parse `[[keybindings]]` rows into the `(mods, keysym, action)` list.
///
/// Workspace binds (Super+1..n view, Super+Shift+1..n move) are auto-generated
/// whenever `[general].auto_workspace_binds` is true (the default) — in the
/// *free* slots only, so they never clobber a combination the user already
/// claimed. Duplicate `(mods, keysym)` pairs within the user's own list resolve
/// first-wins, and the loser is reported via `diag`. A bind whose action targets
/// a workspace beyond `n_tags` is dropped: `n_tags` is known here, so there is
/// no reason to keep a shortcut that can only ever be a no-op.
fn parse_keybindings(
    entries: &[KeybindEntry],
    n_tags: usize,
    auto_workspace_binds: bool,
    diag: &mut Diagnostics,
) -> Vec<(u16, u32, Action)> {
    let mut parsed: Vec<(u16, u32, Action)> = Vec::new();

    for entry in entries {
        let Some((mods, keysym)) = keybind_from_str(&entry.key) else {
            diag.errors.push(format!(
                "discarded keybinding '{}': invalid key combination",
                entry.key
            ));
            continue;
        };
        let Some(action) = action_from_str(&entry.action) else {
            diag.errors.push(format!(
                "discarded keybinding '{}': invalid action '{}'",
                entry.key, entry.action
            ));
            continue;
        };
        if !action_workspace_is_valid(&action, n_tags) {
            diag.errors.push(format!(
                "discarded keybinding '{}': action workspace is outside 1..={n_tags}",
                entry.key
            ));
            continue;
        }
        // First-wins on duplicate combinations; later entries are dropped.
        if parsed.iter().any(|(m, k, _)| *m == mods && *k == keysym) {
            diag.errors.push(format!(
                "keybinding '{}' (mods={mods:#x}, keysym={keysym:#x}) duplicates an earlier \
                 bind and was ignored; keeping {}",
                entry.key,
                action_name_of(&parsed, mods, keysym)
            ));
            continue;
        }
        parsed.push((mods, keysym, action));
    }

    if auto_workspace_binds {
        append_numeric_keybindings(&mut parsed, n_tags);
    }
    parsed
}

/// Human-readable name of the action already bound to `(mods, keysym)`, for the
/// conflict diagnostic.
fn action_name_of(list: &[(u16, u32, Action)], mods: u16, keysym: u32) -> String {
    list.iter()
        .find(|(m, k, _)| *m == mods && *k == keysym)
        .map_or_else(String::new, |(_, _, a)| {
            crate::core::action::name(a).to_string()
        })
}

fn action_workspace_is_valid(action: &Action, n_tags: usize) -> bool {
    match action {
        Action::View(ws) | Action::MoveToWs(ws) | Action::ViewRemove(ws) => *ws < n_tags,
        _ => true,
    }
}

/// Add `Super+1..n` / `Super+Shift+1..n` for `n_tags`, capped at 9 because the
/// digit row has no tenth key. Only unclaimed combinations are filled, so a
/// user bind on e.g. `Super+1` wins and the generated `View(0)` is skipped.
fn append_numeric_keybindings(keybinds: &mut Vec<(u16, u32, Action)>, n_tags: usize) {
    let sup = u16::from(ModMask::M4);
    let shift_sup = sup | u16::from(ModMask::SHIFT);
    for ws in 0..n_tags.min(9) {
        let keysym = b'1' as u32 + ws as u32;
        let view = (sup, keysym);
        let move_to = (shift_sup, keysym);
        if !keybinds.iter().any(|(m, k, _)| (*m, *k) == view) {
            keybinds.push((view.0, view.1, Action::View(ws)));
        }
        if !keybinds.iter().any(|(m, k, _)| (*m, *k) == move_to) {
            keybinds.push((move_to.0, move_to.1, Action::MoveToWs(ws)));
        }
    }
}

/// Curated, layout-independent key name → X11 keysym table: the vocabulary TOML
/// keybindings may use by name. Anything not listed here can still be expressed
/// with the raw `0x<hex>` escape. Letters (`a`–`z`) and digits (`0`–`9`) are
/// handled by computation, so they are not listed here.
///
/// Order is not significant (lookup is a linear scan, which is fine — this runs
/// only at config load); the only requirement is that every keysym the compiled
/// defaults rely on has a name here (see `keysym_name_exists` + the contract
/// test).
pub static KEYSYMS: &[(&str, u32)] = &[
    // ASCII symbols and named non-printing keys
    ("ampersand", 0x26),
    ("apostrophe", 0x27),
    ("asciicircum", 0x5e),
    ("asciitilde", 0x7e),
    ("asterisk", 0x2a),
    ("at", 0x40),
    ("backslash", 0x5c),
    ("backspace", 0xff08),
    ("bar", 0x7c),
    ("braceleft", 0x7b),
    ("braceright", 0x7d),
    ("bracketleft", 0x5b),
    ("bracketright", 0x5d),
    ("colon", 0x3a),
    ("comma", 0x2c),
    ("delete", 0xffff),
    ("dollar", 0x24),
    ("down", 0xff54),
    ("end", 0xff57),
    ("equal", 0x3d),
    ("escape", 0xff1b),
    ("exclam", 0x21),
    ("greater", 0x3e),
    ("grave", 0x60),
    ("home", 0xff50),
    ("insert", 0xff63),
    ("left", 0xff51),
    ("less", 0x3c),
    ("menu", 0xff67),
    ("minus", 0x2d),
    ("next", 0xff56),
    ("numbersign", 0x23),
    ("parenleft", 0x28),
    ("parenright", 0x29),
    ("pause", 0xff13),
    ("percent", 0x25),
    ("period", 0x2e),
    ("plus", 0x2b),
    ("print", 0xff61),
    ("prior", 0xff55),
    ("question", 0x3f),
    ("quotedbl", 0x22),
    ("right", 0xff53),
    ("scroll_lock", 0xff14),
    ("semicolon", 0x3b),
    ("slash", 0x2f),
    ("space", 0x20),
    ("tab", 0xff09),
    ("underscore", 0x5f),
    ("up", 0xff52),
    // function keys
    ("f1", 0xffbe),
    ("f2", 0xffbf),
    ("f3", 0xffc0),
    ("f4", 0xffc1),
    ("f5", 0xffc2),
    ("f6", 0xffc3),
    ("f7", 0xffc4),
    ("f8", 0xffc5),
    ("f9", 0xffc6),
    ("f10", 0xffc7),
    ("f11", 0xffc8),
    ("f12", 0xffc9),
    // enter, and keys that need an alias because their X11 name differs
    ("return", 0xff0d),
    ("enter", 0xff0d),
    ("pageup", 0xff55),
    ("pagedown", 0xff56),
    // keypad
    ("kp_0", 0xffb0),
    ("kp_1", 0xffb1),
    ("kp_2", 0xffb2),
    ("kp_3", 0xffb3),
    ("kp_4", 0xffb4),
    ("kp_5", 0xffb5),
    ("kp_6", 0xffb6),
    ("kp_7", 0xffb7),
    ("kp_8", 0xffb8),
    ("kp_9", 0xffb9),
    ("kp_enter", 0xff8d),
    ("kp_add", 0xffab),
    ("kp_subtract", 0xffad),
    ("kp_multiply", 0xffaa),
    ("kp_divide", 0xffaf),
    ("kp_decimal", 0xffae),
    // XF86 multimedia / brightness (with the unprefixed spellings users expect)
    ("xf86audioraisevolume", 0x1008ff13),
    ("audioraisevolume", 0x1008ff13),
    ("xf86audiolowervolume", 0x1008ff11),
    ("audiolowervolume", 0x1008ff11),
    ("xf86audiomute", 0x1008ff12),
    ("audiomute", 0x1008ff12),
    ("xf86audioplay", 0x1008ff14),
    ("audioplay", 0x1008ff14),
    ("xf86audiostop", 0x1008ff15),
    ("audiostop", 0x1008ff15),
    ("xf86audionext", 0x1008ff17),
    ("audionext", 0x1008ff17),
    ("xf86audioprev", 0x1008ff16),
    ("audioprev", 0x1008ff16),
    ("xf86monbrightnessup", 0x1008ff02),
    ("monbrightnessup", 0x1008ff02),
    ("xf86monbrightnessdown", 0x1008ff03),
    ("monbrightnessdown", 0x1008ff03),
];

/// Convert a supported key name to an X11 keysym. Accepts:
/// - a single `a`–`z` letter or `0`–`9` digit (computed),
/// - any name in `KEYSYMS`,
/// - a raw `0x<hex>` escape for keysyms not in the table.
pub fn keysym_from_name(name: &str) -> Option<u32> {
    let lower = name.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    if let Some(hex) = lower.strip_prefix("0x") {
        return u32::from_str_radix(hex, 16).ok();
    }
    if lower.len() == 1 {
        let byte = lower.as_bytes()[0];
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            return Some(u32::from(byte));
        }
    }
    KEYSYMS.iter().find(|(n, _)| *n == lower).map(|(_, k)| *k)
}

/// True when `ksym` is reachable by name (a letter/digit or a `KEYSYMS` entry).
/// Used by the contract test that every compiled-default keysym is expressible.
#[cfg(test)]
fn keysym_name_exists(ksym: u32) -> bool {
    (ksym as u8).is_ascii_lowercase()
        || (ksym as u8).is_ascii_digit()
        || KEYSYMS.iter().any(|(_, k)| *k == ksym)
}

/// Reverse of `keysym_from_name`, for diagnostics only. Falls back to the raw
/// `0x<hex>` escape, which is itself valid config syntax — so whatever this
/// prints can be pasted straight back into a keybinding.
pub fn keysym_name(ksym: u32) -> String {
    if let Some((name, _)) = KEYSYMS.iter().find(|(_, k)| *k == ksym) {
        return (*name).to_string();
    }
    if ksym < 0x80 {
        let byte = ksym as u8;
        if byte.is_ascii_alphanumeric() {
            return (byte as char).to_string();
        }
    }
    format!("0x{ksym:x}")
}

/// Render a modifier mask the way a user writes it in the TOML
/// (`Super+Shift`). Returns an empty string for a bind with no modifiers.
pub fn mods_name(mask: u16) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(4);
    for (bit, name) in [
        (u16::from(ModMask::M4), "Super"),
        (u16::from(ModMask::CONTROL), "Control"),
        (u16::from(ModMask::M1), "Alt"),
        (u16::from(ModMask::SHIFT), "Shift"),
    ] {
        if mask & bit != 0 {
            parts.push(name);
        }
    }
    parts.join("+")
}

/// Parse a `Mod+Shift+key` binding into `(ModMask, keysym)`. A repeated
/// modifier (`Super+Super+a`) is rejected rather than folded, so a typo cannot
/// silently change which chord is grabbed.
fn keybind_from_str(input: &str) -> Option<(u16, u32)> {
    let parts: Vec<_> = input
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let (key, modifiers) = parts.split_last()?;
    let keysym = keysym_from_name(key)?;
    let mut mask = 0;
    for modifier in modifiers {
        let bit = match modifier.to_ascii_lowercase().as_str() {
            "super" | "mod" | "mod4" => u16::from(ModMask::M4),
            "shift" => u16::from(ModMask::SHIFT),
            "control" | "ctrl" => u16::from(ModMask::CONTROL),
            "alt" | "mod1" => u16::from(ModMask::M1),
            _ => return None,
        };
        if mask & bit != 0 {
            return None;
        }
        mask |= bit;
    }
    Some((mask, keysym))
}

/// Parse the TOML action vocabulary (`spawn:...`, `focus:left`, `view:2`, …).
///
/// Delegates to the single shared vocabulary in `core::action` (the same one
/// the IPC channel uses), so a new action is available in both places and the
/// two cannot diverge. The TOML form is colon separated (`focus:left`); the IPC
/// form is dash/space separated (`focus-left`) — both resolve to the same
/// `Action`.
pub fn action_from_str(input: &str) -> Option<Action> {
    crate::core::action::parse(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Dir;

    /// Parse TOML text straight into the user model, panicking on a syntax
    /// error: these tests only ever feed well-formed input.
    fn parse_string(source: &str) -> UserConfig {
        let mut d = Diagnostics::default();
        parse_user(source, &mut d).expect("valid TOML")
    }

    fn write_temp(contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "maverick-config-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::write(&path, contents).expect("write temporary config");
        path
    }

    #[test]
    fn mod_alias_matches_super_for_legibility_chords() {
        assert_eq!(
            keybind_from_str("Mod+Ctrl+h"),
            keybind_from_str("Super+Ctrl+h")
        );
    }

    #[test]
    fn missing_file_uses_entire_compiled_config() {
        let path = std::env::temp_dir().join("maverick-config-definitely-missing.toml");
        let _ = std::fs::remove_file(&path);
        let (cfg, _diag) = load_from_path(&path);
        assert_eq!(cfg.keybinds.len(), default_config().keybinds.len());
        assert_eq!(cfg.rules.len(), compiled_config().rules.len());
    }

    #[test]
    fn broken_toml_uses_entire_compiled_config() {
        let path = write_temp("[general\ngaps = nope");
        let (cfg, _diag) = load_from_path(&path);
        let baseline = default_config();
        assert_eq!(cfg.gaps_inner, baseline.gaps_inner);
        assert_eq!(cfg.gaps_outer, baseline.gaps_outer);
        assert_eq!(cfg.keybinds.len(), baseline.keybinds.len());
        assert_eq!(cfg.rules.len(), baseline.rules.len());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn fallback_config_has_reachable_workspace_binds() {
        // Every fail-safe path (no file / missing / unreadable / broken TOML)
        // must still yield a WM whose workspaces are reachable: the baseline
        // carries no workspace keybinding of its own.
        let path = std::env::temp_dir().join(format!(
            "maverick-config-missing-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = std::fs::remove_file(&path);
        let (cfg, _diag) = load_from_path(&path);
        let sup = u16::from(ModMask::M4);
        let shift_sup = sup | u16::from(ModMask::SHIFT);
        for i in 0..cfg.n_tags.min(9) {
            let ksym = b'1' as u32 + i as u32;
            assert!(
                cfg.keybinds.iter().any(|(m, k, a)| {
                    *m == sup && *k == ksym && matches!(a, &Action::View(v) if v == i)
                }),
                "fallback config missing View({i}) on Super+{}",
                i + 1
            );
            assert!(
                cfg.keybinds.iter().any(|(m, k, a)| {
                    *m == shift_sup && *k == ksym && matches!(a, &Action::MoveToWs(v) if v == i)
                }),
                "fallback config missing MoveToWs({i}) on Super+Shift+{}",
                i + 1
            );
        }
    }

    #[test]
    fn invalid_binding_is_dropped_without_losing_valid_file_values() {
        let path = write_temp(
            r#"
[general]
gaps = 17

[[keybindings]]
key = "super+not-a-key"
action = "grow_col:abc"

[[keybindings]]
key = "super+q"
action = "kill"
"#,
        );
        let (cfg, _diag) = load_from_path(&path);
        assert_eq!(cfg.gaps_inner, 17);
        assert_eq!(cfg.gaps_outer, 17);
        assert!(cfg
            .keybinds
            .iter()
            .any(|(_, key, action)| { *key == u32::from(b'q') && matches!(action, Action::Kill) }));
        assert_eq!(cfg.keybinds.len(), 19); // valid q plus 18 generated workspace binds
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn user_numeric_bind_keeps_other_auto_binds() {
        // A user binding on a digit slot claims only that slot; the other
        // auto-generated workspace binds survive (there is no full
        // suppression), and the claimed slot keeps the user's action.
        let user = parse_string(
            r#"
[[keybindings]]
key = "super+1"
action = "view:2"
"#,
        );
        let mut diag = Diagnostics::default();
        let cfg = merge_config(compiled_config(), user, &mut diag);
        // 1 user bind + 17 remaining generated (the other 8 super-view binds
        // plus the 9 super+shift move binds; super+1's generated view is skipped).
        assert_eq!(cfg.keybinds.len(), 18, "other auto binds must survive");
        let sup = u16::from(ModMask::M4);
        assert!(
            cfg.keybinds
                .iter()
                .any(|(m, k, a)| *m == sup && *k == b'1' as u32 && matches!(a, Action::View(1))),
            "user's claimed slot keeps view:2"
        );
        assert!(
            !cfg.keybinds.iter().any(|(m, k, a)| {
                *m == sup && *k == b'1' as u32 && matches!(a, Action::View(0))
            }),
            "generated view:0 for the claimed slot must be suppressed"
        );
        assert!(
            cfg.keybinds
                .iter()
                .any(|(m, k, a)| *m == sup && *k == b'2' as u32 && matches!(a, Action::View(1))),
            "a non-claimed slot keeps its generated bind"
        );
    }

    #[test]
    fn auto_workspace_binds_false_disables_generation() {
        // Both branches that can generate binds: the no-user-keybindings path
        // keeps the compiled keymap as-is, and the `[[keybindings]]` path
        // rebuilds the list from the user's own entries.
        let user = parse_string(
            r"
[general]
auto_workspace_binds = false
",
        );
        let mut diag = Diagnostics::default();
        let cfg = merge_config(compiled_config(), user, &mut diag);
        assert!(!cfg.keybinds.iter().any(|(_, k, a)| {
            (b'1'..=b'9').contains(&(*k as u8))
                && matches!(a, Action::View(_) | Action::MoveToWs(_))
        }));

        let user = parse_string(
            r#"
[general]
auto_workspace_binds = false

[[keybindings]]
key = "super+q"
action = "quit"
"#,
        );
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert_eq!(cfg.keybinds.len(), 1, "only the user's own bind may remain");
        assert!(matches!(cfg.keybinds[0].2, Action::Quit));
    }

    /// `auto_workspace_binds = false` has to hold on the config the loader
    /// actually returns. The merge baseline is the compiled keymap, which never
    /// contained a workspace bind; the fail-safe `default_config` does, so a
    /// baseline that already carried them would make every generated chord
    /// unreachable to this flag — and indistinguishable from the user's own once
    /// merged, so nothing downstream could take them away again.
    #[test]
    fn auto_workspace_binds_false_survives_the_loader() {
        let path = write_temp("[general]\nauto_workspace_binds = false\n");
        let (cfg, diag) = load_from_path(&path);
        assert!(
            diag.is_clean(),
            "disabling a feature is not a fault: {diag:?}"
        );
        assert!(
            !cfg.keybinds.iter().any(|(_, k, a)| {
                (b'1'..=b'9').contains(&(*k as u8))
                    && matches!(a, Action::View(_) | Action::MoveToWs(_))
            }),
            "an explicit false must leave no generated workspace binding"
        );
        // The compiled keymap is untouched: the flag suppresses generation, it
        // does not disable the WM.
        assert_eq!(cfg.keybinds.len(), compiled_config().keybinds.len());
        let _ = std::fs::remove_file(path);
    }

    /// Generated binds must cover the file's own `n_tags`. A baseline that
    /// generated them before the file was read would keep chords for workspaces
    /// the user just removed (`View(8)` on a three-tag WM).
    #[test]
    fn generated_workspace_binds_follow_the_files_n_tags() {
        let path = write_temp("[general]\nn_tags = 3\n");
        let (cfg, diag) = load_from_path(&path);
        assert!(diag.is_clean(), "{diag:?}");
        assert_eq!(cfg.n_tags, 3);
        let workspaces: Vec<usize> = cfg
            .keybinds
            .iter()
            .filter_map(|(_, k, a)| {
                let digit = *k as u8;
                if !(b'1'..=b'9').contains(&digit) {
                    return None;
                }
                match a {
                    Action::View(ws) | Action::MoveToWs(ws) => Some(*ws),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(workspaces.len(), 6, "3 tags = 3 view + 3 move binds");
        assert!(
            workspaces.iter().all(|ws| *ws < cfg.n_tags),
            "a bind for a workspace the file removed: {workspaces:?}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn n_tags_limits_generated_workspace_binds() {
        let user = parse_string(
            r"
[general]
n_tags = 3
",
        );
        let mut diag = Diagnostics::default();
        let cfg = merge_config(compiled_config(), user, &mut diag);
        let numeric = cfg
            .keybinds
            .iter()
            .filter(|(_, k, a)| {
                (b'1'..=b'3').contains(&(*k as u8))
                    && matches!(a, Action::View(_) | Action::MoveToWs(_))
            })
            .count();
        assert_eq!(numeric, 6);
        assert_eq!(cfg.n_tags, 3);
    }

    #[test]
    fn parses_supported_keys_and_actions() {
        assert_eq!(keysym_from_name("F12"), Some(0xffc9));
        assert_eq!(keysym_from_name("z"), Some(u32::from(b'z')));
        assert_eq!(keysym_from_name("unknown"), None);
        assert!(matches!(
            action_from_str("focus:left"),
            Some(Action::FocusDir(Dir::Left))
        ));
        assert!(matches!(
            action_from_str("grow_col:-50"),
            Some(Action::GrowCol(-50))
        ));
        assert!(action_from_str("grow_col:abc").is_none());
        assert!(action_from_str("spawn:").is_none());
    }

    #[test]
    fn list_sections_replace_compiled_lists() {
        let user = parse_string(
            r#"
[[rules]]
class = "Firefox"
float = true

[autostart]
commands = [["example", "--flag"]]
"#,
        );
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].class.as_deref(), Some("Firefox"));
        assert_eq!(cfg.autostart, vec![vec!["example", "--flag"]]);
    }

    #[test]
    fn rule_fullscreen_policy_parses_and_all_aliases_agree() {
        // Deny — refuse the app's own runtime fullscreen (F11/EWMH), but leave
        // the built-in `Mod4+Shift+F` working.
        for key in ["deny_fullscreen", "no_fullscreen"] {
            let user = parse_string(&format!("[[rules]]\nclass = \"firefox\"\n{key} = true\n"));
            let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
            assert_eq!(cfg.rules.len(), 1);
            assert!(
                cfg.rules[0].deny_fullscreen,
                "'{key}' must set Rule::deny_fullscreen"
            );
            assert!(!cfg.rules[0].true_fullscreen);
        }
        // True — exclusive, ribbon-free fullscreen for games. `True` wins over
        // `Deny` when both are set.
        for key in ["true_fullscreen", "exclusive_fullscreen"] {
            let user = parse_string(&format!(
                "[[rules]]\nclass = \"game\"\n{key} = true\ndeny_fullscreen = true\n"
            ));
            let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
            assert_eq!(cfg.rules.len(), 1);
            assert!(
                cfg.rules[0].true_fullscreen,
                "'{key}' must set Rule::true_fullscreen"
            );
            assert!(cfg.rules[0].deny_fullscreen);
        }
        // Defaults stay false.
        let user = parse_string("[[rules]]\nclass = \"firefox\"\n");
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert!(!cfg.rules[0].deny_fullscreen);
        assert!(!cfg.rules[0].true_fullscreen);
    }

    /// The documented per-rule opt-in (`config/config.toml`: "opt a single app
    /// in with `[[rules]] honor_initial_state = true`") has to reach
    /// `Rule::honor_initial_state`, which is the only thing `manage::apply_rules`
    /// reads before it either keeps or strips the client's map-time
    /// `_NET_WM_STATE`. A rule field the parser never fills is indistinguishable
    /// from one that does not exist, so the escape hatch silently does nothing
    /// while the documentation promises otherwise.
    #[test]
    fn rule_honor_initial_state_reaches_the_runtime_policy() {
        // Both directions, because the rule is an `Option`: `Some(true)` has to
        // let one window keep its launch state under the global `false`, and
        // `Some(false)` has to normalize it under a global `true`.
        for (rule_value, global) in [("true", false), ("false", true)] {
            let user = parse_string(&format!(
                "[general]\nhonor_initial_state = {global}\n\n\
                 [[rules]]\nclass = \"firefox\"\nhonor_initial_state = {rule_value}\n"
            ));
            let mut diag = Diagnostics::default();
            let cfg = merge_config(compiled_config(), user, &mut diag);
            assert!(diag.is_clean(), "unexpected diagnostics: {diag:?}");
            assert_eq!(cfg.honor_initial_state, global);
            assert_eq!(cfg.rules.len(), 1);
            assert_eq!(
                cfg.rules[0].honor_initial_state,
                Some(rule_value == "true"),
                "'honor_initial_state = {rule_value}' must survive parse and merge"
            );
        }
        // Absent stays `None`, which is what makes the rule defer to the global
        // policy instead of silently normalizing every other window too.
        let user = parse_string("[[rules]]\nclass = \"firefox\"\n");
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert_eq!(cfg.rules[0].honor_initial_state, None);
        assert!(!cfg.honor_initial_state, "global default must normalize");
        // A wrong-typed value leaves the rule deferring to the global rather
        // than reading as `false`, which would force normalization.
        let mut diag = Diagnostics::default();
        let user = parse_user(
            "[[rules]]\nclass = \"firefox\"\nhonor_initial_state = \"yes\"\n",
            &mut diag,
        )
        .expect("valid TOML");
        let cfg = merge_config(compiled_config(), user, &mut diag);
        assert_eq!(cfg.rules[0].honor_initial_state, None);
        assert!(
            diag.warnings
                .iter()
                .any(|w| w.contains("honor_initial_state")),
            "the bad value must be reported: {:?}",
            diag.warnings
        );
    }

    #[test]
    fn rule_ignore_initial_state_parses_and_all_aliases_agree() {
        for key in ["ignore_initial_state", "no_initial_state", "no_maximize"] {
            let user = parse_string(&format!("[[rules]]\nclass = \"firefox\"\n{key} = true\n"));
            let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
            assert_eq!(cfg.rules.len(), 1);
            assert!(
                cfg.rules[0].ignore_initial_state,
                "'{key}' must set Rule::ignore_initial_state"
            );
        }
        // Default (key absent) stays false.
        let user = parse_string("[[rules]]\nclass = \"firefox\"\n");
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert!(!cfg.rules[0].ignore_initial_state);
    }

    #[test]
    fn theme_preset_fills_colors_but_explicit_colors_win() {
        let user = parse_string(
            r#"
[general]
theme = "nord"
"#,
        );
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        let (normal, focused, urgent) = crate::config::theme_palette("nord").unwrap();
        assert_eq!(cfg.col_normal, normal);
        assert_eq!(cfg.col_focused, focused);
        assert_eq!(cfg.col_urgent, urgent);

        // [colors] applied after [general], so it overrides the theme.
        let user = parse_string(
            r#"
[general]
theme = "nord"

[colors]
focused = 0x00ff00
"#,
        );
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert_eq!(cfg.col_focused, 0x00ff00);
        assert_eq!(cfg.col_normal, normal); // untouched field still from the theme
    }

    #[test]
    fn unknown_theme_name_is_ignored() {
        let baseline = compiled_config();
        let user = parse_string(
            r#"
[general]
theme = "not-a-real-theme"
"#,
        );
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert_eq!(cfg.col_normal, baseline.col_normal);
        assert_eq!(cfg.col_focused, baseline.col_focused);
    }

    /// A floor above the target is contradictory: the parser diagnoses it and
    /// normalizes the floor toward the target, so the file cannot silently
    /// cancel the reduction the target asked for. Valid pairs — including the
    /// explicit full-size `1.0 / 1.0` — merge untouched and stay
    /// diagnostic-clean.
    #[test]
    fn overview_floor_above_target_is_diagnosed_and_normalized() {
        // Contradictory: floor 1.0 above the default 0.76 target.
        let user = parse_string("[general]\noverview_zoom_min = 1.0\n");
        let mut diag = Diagnostics::default();
        let cfg = merge_config(compiled_config(), user, &mut diag);
        assert_eq!(cfg.overview_scale, 0.76);
        assert_eq!(
            cfg.overview_zoom_min, cfg.overview_scale,
            "the floor must normalize toward the target, not cancel the reduction"
        );
        assert!(
            diag.errors
                .iter()
                .any(|e| e.contains("overview_zoom_min") && e.contains("overview_scale")),
            "the contradiction must be diagnosed: {:?}",
            diag.errors
        );
        // Explicit full-size pair: valid, silent.
        let user = parse_string("[general]\noverview_scale = 1.0\noverview_zoom_min = 1.0\n");
        let mut diag = Diagnostics::default();
        let cfg = merge_config(compiled_config(), user, &mut diag);
        assert_eq!(cfg.overview_scale, 1.0);
        assert_eq!(cfg.overview_zoom_min, 1.0);
        assert!(diag.is_clean(), "{diag:?}");
        // Ordinary valid pair: silent.
        let user = parse_string("[general]\noverview_scale = 0.76\noverview_zoom_min = 0.25\n");
        let mut diag = Diagnostics::default();
        let cfg = merge_config(compiled_config(), user, &mut diag);
        assert_eq!(cfg.overview_scale, 0.76);
        assert_eq!(cfg.overview_zoom_min, 0.25);
        assert!(diag.is_clean(), "{diag:?}");
    }

    #[test]
    fn gaps_legacy_alias_sets_both_inner_and_outer() {
        let user = parse_string(
            r"
[general]
gaps = 20
",
        );
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert_eq!(cfg.gaps_inner, 20);
        assert_eq!(cfg.gaps_outer, 20);
    }

    #[test]
    fn gaps_inner_outer_can_be_set_independently() {
        let user = parse_string(
            r"
[general]
gaps_inner = 4
gaps_outer = 12
smart_gaps = true
corner_radius = 10
",
        );
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert_eq!(cfg.gaps_inner, 4);
        assert_eq!(cfg.gaps_outer, 12);
        assert!(cfg.smart_gaps);
        assert_eq!(cfg.corner_radius, 10);
    }

    #[test]
    fn rule_opacity_and_border_width_are_parsed() {
        let user = parse_string(
            r#"
[[rules]]
class = "mpv"
float = true
opacity = 0.9
border_width = 0
"#,
        );
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].opacity, Some(0.9));
        assert_eq!(cfg.rules[0].border_w, Some(0));
    }

    #[test]
    fn shipped_example_config_parses() {
        // The example at config/config.toml must always parse and exercise the
        // documented features (themes, hex colors, viewport keybinds, rules
        // with deny/true fullscreen, autostart grid).
        let (cfg, diag) = load_from_path(Path::new("config/config.toml"));
        assert!(!cfg.keybinds.is_empty(), "keybindings must parse");
        // Diagnostic-clean, because this is the file users copy and
        // `--check-config` is documented as a CI gate: a shipped example that
        // exits 1 (or, worse, one whose keys quietly do nothing) teaches the
        // wrong thing twice over.
        assert!(
            diag.is_clean(),
            "the shipped example must not warn: {diag:?}"
        );
        // The example must not reintroduce a per-`WM_CLASS` workaround for
        // map-time state: normalization is a global invariant, so the example
        // carries no `firefox` rule and leaves `honor_initial_state` at its
        // `false` default, while still pinning Steam to exclusive fullscreen.
        assert!(
            !cfg.honor_initial_state,
            "default must normalize initial state"
        );
        assert!(!cfg
            .rules
            .iter()
            .any(|r| r.class.as_deref() == Some("firefox")));
        assert!(cfg
            .rules
            .iter()
            .any(|r| r.class.as_deref() == Some("steam") && r.true_fullscreen));
        assert_eq!(cfg.col_focused, 0x89b4fa);
        // A second compositor alongside an external one breaks compositing.
        assert!(!cfg
            .autostart
            .iter()
            .any(|c| c.first().is_some_and(|b| b == "picom")));
        // Numeric workspace binds are auto-restored when no digit keybinds exist.
        assert!(cfg
            .keybinds
            .iter()
            .any(|(_, k, a)| { *k == b'1' as u32 && matches!(a, crate::types::Action::View(0)) }));
        // Quitting is an in-process action: the sample must bind `Mod4+Shift+q`
        // to `Action::Quit`, never to a `spawn:` of a helper that would put a
        // prompt and a second process on the quit path.
        let sup_shift = u16::from(ModMask::M4) | u16::from(ModMask::SHIFT);
        let q = u32::from(b'q');
        assert_eq!(
            cfg.keybinds
                .iter()
                .find(|(m, k, _)| *m == sup_shift && *k == q),
            Some(&(sup_shift, q, Action::Quit)),
            "example config must quit natively"
        );
        assert!(
            !cfg.keybinds.iter().any(|(_, _, a)| matches!(
                a,
                Action::Spawn(cmd) if cmd.iter().any(|c| c == "maverickctl")
            )),
            "example config must not shell out to maverickctl"
        );
    }

    #[test]
    fn every_compiled_default_keysym_is_named() {
        // Contract test for `KEYSYMS`: every keysym the compiled config binds
        // must be expressible by name (a letter/digit or a `KEYSYMS` entry),
        // or that default bind would be literally unconfigurable.
        let cfg = compiled_config();
        for (_mods, ksym, action) in &cfg.keybinds {
            assert!(
                keysym_name_exists(*ksym),
                "keysym {ksym:#x} used by action {action:?} has no name in KEYSYMS"
            );
        }
    }

    #[test]
    fn classified_load_marks_a_broken_file_as_the_compiled_baseline() {
        let path = write_temp("[general\ngaps = nope");
        let (source, cfg, diag) = load_from_path_classified(&path);
        assert_eq!(source, ConfigSource::CompiledBaseline);
        assert_eq!(cfg.keybinds.len(), default_config().keybinds.len());
        assert!(
            diag.errors.iter().any(|e| e.contains("invalid TOML")),
            "the syntax fault must be reported: {:?}",
            diag.errors
        );
        // Boot keeps the documented baseline fallback — same config, same
        // diagnostic; only the reason is dropped.
        let (boot_cfg, boot_diag) = load_from_path(&path);
        assert_eq!(boot_cfg.keybinds.len(), cfg.keybinds.len());
        assert_eq!(boot_diag.errors, diag.errors);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn classified_load_marks_a_missing_file_as_the_compiled_baseline() {
        let path = std::env::temp_dir().join("maverick-config-classified-missing.toml");
        let _ = std::fs::remove_file(&path);
        let (source, cfg, diag) = load_from_path_classified(&path);
        assert_eq!(source, ConfigSource::CompiledBaseline);
        assert_eq!(cfg.keybinds.len(), default_config().keybinds.len());
        // A missing file yields a CLEAN diagnostic: the reason has to come from
        // `ConfigSource`, never from the diagnostic lists.
        assert!(diag.is_clean(), "{diag:?}");
        let (boot_cfg, _) = load_from_path(&path);
        assert_eq!(boot_cfg.keybinds.len(), cfg.keybinds.len());
    }

    #[test]
    fn classified_load_marks_a_partly_rejected_file_as_the_users_file() {
        // `diag.errors` is NOT an unusable signal: a rejected value is recorded
        // while every other value in the file is merged in, so a reload must
        // still adopt this config.
        let path = write_temp("[general]\ngaps = 17\nn_tags = 42\n");
        let (source, cfg, diag) = load_from_path_classified(&path);
        assert_eq!(source, ConfigSource::UserFile);
        assert_eq!(cfg.gaps_inner, 17);
        assert_eq!(cfg.gaps_outer, 17);
        assert!(
            diag.errors.iter().any(|e| e.contains("n_tags")),
            "{:?}",
            diag.errors
        );
        let _ = std::fs::remove_file(path);
    }

    /// A table for a subsystem Maverick no longer has must not cost the user
    /// their whole config. An unknown section is skipped, so the rest of the
    /// file still merges and the compiled values survive untouched.
    #[test]
    fn a_table_for_a_removed_subsystem_loads_without_touching_the_config() {
        let body = "[compositor]\nenabled = true\n[animations]\nstiffness = 300.0\n\
                    [[keybindings]]\nkey = \"super+q\"\naction = \"quit\"\n\
                    [general]\ngaps_inner = 17\n";
        let mut diag = Diagnostics::default();
        let user = parse_user(body, &mut diag).expect("valid TOML");
        let cfg = merge_config(compiled_config(), user, &mut diag);
        // The keys Maverick does know still applied, so the file was merged
        // rather than discarded along with the tables it does not.
        assert_eq!(cfg.gaps_inner, 17);
        // Silent, and not merely error-free: a config written for a newer
        // Maverick names tables this build has never heard of, and reporting
        // them would make every such file fail `--check-config`. Only a key
        // inside a table Maverick *does* know is reported (see
        // `an_unknown_key_in_a_known_table_is_reported`).
        assert!(diag.is_clean(), "unknown tables must stay silent: {diag:?}");
    }

    /// A key no known table has a field for is a setting the user believes they
    /// made and did not, so it has to be named — silently dropping it is how a
    /// documented key like `honor_initial_state` can be missing from the parser
    /// for years while every test still passes.
    #[test]
    fn an_unknown_key_in_a_known_table_is_reported() {
        let path = write_temp("[general]\nhonour_initial_state = true\ngaps_inner = 17\n");
        let (source, cfg, diag) = load_from_path_classified(&path);
        assert_eq!(
            source,
            ConfigSource::UserFile,
            "one bad key is not a bad file"
        );
        // Everything else in the file still merges: the value is degraded, not
        // discarded.
        assert_eq!(cfg.gaps_inner, 17);
        assert!(diag.errors.is_empty(), "{:?}", diag.errors);
        assert_eq!(diag.warnings.len(), 1, "{:?}", diag.warnings);
        let warning = &diag.warnings[0];
        assert!(
            warning.contains("[general].honour_initial_state"),
            "the warning must name the table and the key: {warning}"
        );
        // `--check-config` exits on `!is_clean()`, so a warning alone is enough
        // to fail the CI gate: "the file loads" is not "the file is what you
        // wrote".
        assert!(!diag.is_clean(), "{diag:?}");
        let _ = std::fs::remove_file(path);
    }

    /// A key written before any table header is a typo, not a forward-
    /// compatibility case, and must be reported the way a typo inside a known
    /// table is. The two are different mistakes: an unknown `[table]` has keys
    /// Maverick cannot judge, but a bare key at file scope matches nothing at
    /// all and was being dropped without a word.
    #[test]
    fn a_key_before_any_table_is_reported() {
        let path = write_temp("n_tgas = 3\n[general]\ngaps_inner = 11\n");
        let (_source, cfg, diag) = load_from_path_classified(&path);
        // The rest of the file must still merge: the bad key is degraded, not
        // discarded, and it must not abort the tables after it.
        assert_eq!(cfg.gaps_inner, 11);
        assert!(diag.errors.is_empty(), "{:?}", diag.errors);
        assert_eq!(diag.warnings.len(), 1, "{:?}", diag.warnings);
        let warning = &diag.warnings[0];
        assert!(
            warning.contains("n_tgas"),
            "the warning must name the key: {warning}"
        );
        assert!(
            !diag.is_clean(),
            "--check-config must fail the gate on a bare key: {diag:?}"
        );
        let _ = std::fs::remove_file(path);
    }

    /// Every table with a dispatcher reports its unknown keys, so the guarantee
    /// cannot be lost one table at a time as tables are added.
    #[test]
    fn every_known_table_reports_its_unknown_keys() {
        for (body, expected) in [
            ("[general]\ntypo = 1\n", "[general].typo"),
            ("[colors]\nfocus = 1\n", "[colors].focus"),
            ("[autostart]\nprogrms = [[\"x\"]]\n", "[autostart].progrms"),
            (
                "[[keybindings]]\nkey = \"super+q\"\nact = \"quit\"\n",
                "[[keybindings]].act",
            ),
            (
                "[[rules]]\nclass = \"x\"\nfloatt = true\n",
                "[[rules]].floatt",
            ),
        ] {
            let mut diag = Diagnostics::default();
            parse_user(body, &mut diag).expect("valid TOML");
            assert_eq!(diag.warnings.len(), 1, "{body} -> {:?}", diag.warnings);
            assert!(
                diag.warnings[0].contains(expected),
                "{body} -> {:?}",
                diag.warnings[0]
            );
            assert!(diag.errors.is_empty(), "{body} is degraded, not rejected");
        }
    }
}
