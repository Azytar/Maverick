//! Unit tests for the pure keyboard-mapping logic: which keycodes a bind grabs,
//! which keysym a press resolves to, and the modifier-mask hygiene both depend
//! on. All of it runs against synthetic keymaps, so no X server is involved.

use super::*;
use crate::types::Dir;

const XK_Z: u32 = 0x7a;
const XK_Z_UPPER: u32 = 0x5a;
const XK_Y: u32 = 0x79;
const XK_Y_UPPER: u32 = 0x59;
const XK_A: u32 = 0x61;
const XK_A_UPPER: u32 = 0x41;
const XK_BRACKETLEFT: u32 = 0x5b;
const XK_BRACKETRIGHT: u32 = 0x5d;
const XK_DEAD_GRAVE: u32 = 0xfe50;
const XK_DEAD_CIRCUMFLEX: u32 = 0xfe52;

const MIN: u8 = 8;
const SUPER: u16 = 1 << 6; // ModMask::M4
const SHIFT: u16 = 1 << 0; // ModMask::SHIFT

/// `setxkbmap us,de`: four columns per keycode, where columns 2/3 are the
/// *second group*, not levels 3/4 of the first. `z` and `y` swap between the
/// two layouts, which is what makes an "any column" grab steal a key.
///
/// Rows (keycodes 8..): `a`, `z`(us)/`y`(de), `y`(us)/`z`(de).
fn keymap_us_de() -> Vec<u32> {
    vec![
        XK_A, XK_A_UPPER, XK_A, XK_A_UPPER, // keycode 8
        XK_Z, XK_Z_UPPER, XK_Y, XK_Y_UPPER, // keycode 9
        XK_Y, XK_Y_UPPER, XK_Z, XK_Z_UPPER, // keycode 10
    ]
}

/// `setxkbmap es`-flavoured single-group keymap where columns 2/3 are levels
/// 3/4 (`AltGr`) of group 1. `[` and `]` live *only* behind `AltGr`, so nothing in
/// group 1 can reach them.
///
/// Rows (keycodes 8..): `a`, `dead_grave`/`dead_circumflex` + `[`/`]`.
fn keymap_es() -> Vec<u32> {
    vec![
        XK_A,
        XK_A_UPPER,
        0,
        0, // keycode 8
        XK_DEAD_GRAVE,
        XK_DEAD_CIRCUMFLEX,
        XK_BRACKETLEFT,
        XK_BRACKETRIGHT, // keycode 9
    ]
}

// ── 1. Strict group 1 vs. full scan ────────────────────────────────────────────

#[test]
fn group1_scan_ignores_the_second_group() {
    let km = keymap_us_de();

    // `z` is in group 1 of keycode 9 and in group *2* of keycode 10.
    let g1 = keysym_to_codes_group1(&km, MIN, 4, XK_Z);
    assert_eq!(g1, vec![9], "group-1 scan must return exactly one keycode");

    // The old behaviour: both keycodes, so pressing physical `y` under `us,de`
    // was swallowed by a grab that `on_key` then failed to resolve (R1).
    let any = keysym_to_codes_any(&km, MIN, 4, XK_Z);
    assert_eq!(any, vec![9, 10], "full scan must still see both groups");
}

#[test]
fn group1_scan_includes_the_shifted_column() {
    let km = keymap_us_de();
    assert_eq!(keysym_to_codes_group1(&km, MIN, 4, XK_Z_UPPER), vec![9]);
    assert_eq!(keysym_to_codes_group1(&km, MIN, 4, XK_A_UPPER), vec![8]);
}

#[test]
fn scans_tolerate_an_empty_keymap() {
    assert!(keysym_to_codes_group1(&[], MIN, 0, XK_Z).is_empty());
    assert!(keysym_to_codes_any(&[], MIN, 0, XK_Z).is_empty());
}

// ── 2. Keysym-directed fallback ────────────────────────────────────────────────

#[test]
fn altgr_only_keysym_falls_back_and_is_recorded() {
    let km = keymap_es();

    // Group 1 cannot reach `[` at all on this layout.
    assert!(keysym_to_codes_group1(&km, MIN, 4, XK_BRACKETLEFT).is_empty());

    let plan = plan_key_grabs(&km, MIN, 4, &[(SUPER, XK_BRACKETLEFT)]);
    assert_eq!(plan.grabs, vec![(SUPER, XK_BRACKETLEFT, 9)]);
    assert!(plan.missing.is_empty());
    assert_eq!(
        plan.code_bindings.get(&9),
        Some(&vec![XK_BRACKETLEFT]),
        "a fallback grab must record what the keycode was grabbed for"
    );
}

#[test]
fn group1_binds_are_not_recorded_in_code_bindings() {
    let plan = plan_key_grabs(&keymap_us_de(), MIN, 4, &[(SUPER, XK_Z)]);
    assert_eq!(plan.grabs, vec![(SUPER, XK_Z, 9)]);
    assert!(
        plan.code_bindings.is_empty(),
        "group-1 binds resolve through the keymap columns, no side table needed"
    );
}

#[test]
fn a_keysym_absent_from_the_layout_is_reported_not_grabbed() {
    // `[` does not exist anywhere in the us,de keymap.
    let plan = plan_key_grabs(&keymap_us_de(), MIN, 4, &[(SUPER, XK_BRACKETLEFT)]);
    assert!(plan.grabs.is_empty(), "never grab a key we cannot dispatch");
    assert_eq!(plan.missing, vec![(SUPER, XK_BRACKETLEFT)]);
}

#[test]
fn duplicate_mask_keycode_pairs_are_grabbed_once() {
    // `z` and `Z` are the same physical key under the same modifiers: grabbing
    // twice would earn a BadAccess that reads like a real conflict.
    let plan = plan_key_grabs(
        &keymap_us_de(),
        MIN,
        4,
        &[(SUPER, XK_Z), (SUPER, XK_Z_UPPER)],
    );
    assert_eq!(plan.grabs, vec![(SUPER, XK_Z, 9)]);
}

// ── 3. Two fallback keysyms on one keycode ─────────────────────────────────────

#[test]
fn two_altgr_keysyms_on_one_keycode_stay_distinguishable() {
    let km = keymap_es();
    let binds = [(SUPER, XK_BRACKETLEFT), (SUPER | SHIFT, XK_BRACKETRIGHT)];
    let plan = plan_key_grabs(&km, MIN, 4, &binds);

    assert_eq!(
        plan.code_bindings.get(&9),
        Some(&vec![XK_BRACKETLEFT, XK_BRACKETRIGHT]),
        "both AltGr-level binds must be recorded on the shared keycode"
    );
    assert_eq!(
        plan.grabs,
        vec![
            (SUPER, XK_BRACKETLEFT, 9),
            (SUPER | SHIFT, XK_BRACKETRIGHT, 9),
        ]
    );

    let cfg = Cfg {
        keybinds: vec![
            (SUPER, XK_BRACKETLEFT, Action::PageSnap(Dir::Left)),
            (SUPER | SHIFT, XK_BRACKETRIGHT, Action::PageSnap(Dir::Right)),
        ],
        ..Cfg::default()
    };
    let keymap = build_keymap(&cfg);

    // Group 1 of keycode 9 is the dead keys — neither is bound, so resolution
    // has to reach `code_bindings`, and each modifier mask must pick its own.
    let left = resolve_binding(
        &keymap,
        &plan.code_bindings,
        SUPER,
        9,
        XK_DEAD_GRAVE,
        XK_DEAD_GRAVE,
    );
    assert_eq!(
        left,
        Some(((SUPER, XK_BRACKETLEFT), Action::PageSnap(Dir::Left)))
    );

    let right = resolve_binding(
        &keymap,
        &plan.code_bindings,
        SUPER | SHIFT,
        9,
        XK_DEAD_GRAVE,
        XK_DEAD_CIRCUMFLEX,
    );
    assert_eq!(
        right,
        Some((
            (SUPER | SHIFT, XK_BRACKETRIGHT),
            Action::PageSnap(Dir::Right)
        ))
    );
}

#[test]
fn resolution_prefers_group1_over_the_fallback_table() {
    let cfg = Cfg {
        keybinds: vec![
            (SUPER, XK_A, Action::Kill),
            (SUPER, XK_BRACKETLEFT, Action::ToggleFloat),
        ],
        ..Cfg::default()
    };
    let keymap = build_keymap(&cfg);
    let mut code_bindings = std::collections::HashMap::new();
    code_bindings.insert(9u8, vec![XK_BRACKETLEFT]);

    // Column 0 says `a`, which is bound: the side table must not win.
    let hit = resolve_binding(&keymap, &code_bindings, SUPER, 9, XK_A, XK_A_UPPER);
    assert_eq!(hit, Some(((SUPER, XK_A), Action::Kill)));
}

#[test]
fn an_unbound_press_resolves_to_nothing() {
    let cfg = Cfg {
        keybinds: vec![(SUPER, XK_A, Action::Kill)],
        ..Cfg::default()
    };
    let keymap = build_keymap(&cfg);
    let hit = resolve_binding(
        &keymap,
        &std::collections::HashMap::new(),
        SUPER,
        9,
        XK_Z,
        XK_Z_UPPER,
    );
    assert!(hit.is_none());
}

// ── 4. Modifier-mask hygiene ───────────────────────────────────────────────────

#[test]
fn clean_mask_strips_the_xkb_group_bits() {
    // 0x2000/0x4000 are XKB's group indicators, not modifiers. A press made
    // with a non-default group active carries them, and without masking they
    // would never match a bind's mask.
    let numlock = 1u16 << 4;
    let state = SUPER | 0x2000 | 0x4000;
    assert_eq!(clean_mask(state, numlock), SUPER);

    // NumLock and CapsLock are removed too, so binds work in any lock state.
    let lock = 1u16 << 1;
    assert_eq!(clean_mask(SUPER | numlock | lock, numlock), SUPER);
    assert_eq!(clean_mask(SUPER | SHIFT | 0x2000, numlock), SUPER | SHIFT);
}

// ── 5. Config keysym normalisation ─────────────────────────────────────────────

#[test]
fn a_raw_uppercase_keysym_bind_is_indexed_lowercase() {
    // `key = "0x41"` in the TOML. `on_key` normalises what it reads from the
    // keymap to lowercase, so the index has to be normalised as well or the
    // bind could never fire (R8).
    let cfg = Cfg {
        keybinds: vec![(SUPER, XK_A_UPPER, Action::Kill)],
        ..Cfg::default()
    };
    let keymap = build_keymap(&cfg);
    assert_eq!(keymap.get(&(SUPER, XK_A)), Some(&Action::Kill));
    assert!(!keymap.contains_key(&(SUPER, XK_A_UPPER)));

    // …and the grab side still searches for the raw keysym, which really does
    // live in column 1 of the `a` keycode.
    let plan = plan_key_grabs(&keymap_us_de(), MIN, 4, &[(SUPER, XK_A_UPPER)]);
    assert_eq!(plan.grabs, vec![(SUPER, XK_A_UPPER, 8)]);

    // End to end: the press reports column 0 (`a`), the bind was written `0x41`.
    let hit = resolve_binding(
        &keymap,
        &std::collections::HashMap::new(),
        SUPER,
        8,
        XK_A,
        XK_A_UPPER,
    );
    assert_eq!(hit, Some(((SUPER, XK_A), Action::Kill)));
}

#[test]
fn first_duplicate_bind_still_wins_after_normalisation() {
    let cfg = Cfg {
        keybinds: vec![
            (SUPER, XK_A_UPPER, Action::Kill),
            (SUPER, XK_A, Action::ToggleFloat),
        ],
        ..Cfg::default()
    };
    assert_eq!(build_keymap(&cfg).get(&(SUPER, XK_A)), Some(&Action::Kill));
}

// ── 6. Dispatch column choice ──────────────────────────────────────────────────

#[test]
fn dispatch_never_reads_past_group1() {
    for kpk in [1usize, 2, 4, 6] {
        for shift in [false, true] {
            for lock in [false, true] {
                let col = dispatch_col(shift, lock, kpk);
                assert!(
                    col <= 1,
                    "kpk={kpk} shift={shift} lock={lock} selected column {col}"
                );
                assert!(col < kpk, "column {col} out of range for kpk={kpk}");
            }
        }
    }
    // Shift alone (or CapsLock alone) picks the shifted column; both together
    // cancel out, as they do on a real keyboard.
    assert_eq!(dispatch_col(false, false, 4), 0);
    assert_eq!(dispatch_col(true, false, 4), 1);
    assert_eq!(dispatch_col(false, true, 4), 1);
    assert_eq!(dispatch_col(true, true, 4), 0);
    // A degenerate one-column keymap must not index out of bounds.
    assert_eq!(dispatch_col(true, false, 1), 0);
}

// ── Diagnostics ────────────────────────────────────────────────────────────────

#[test]
fn bind_names_round_trip_into_config_syntax() {
    assert_eq!(bind_name(SUPER | SHIFT, XK_A), "Super+Shift+a");
    assert_eq!(bind_name(SUPER, XK_BRACKETLEFT), "Super+bracketleft");
    // No name in the table → the hex escape, which is valid config syntax.
    assert_eq!(bind_name(SUPER, 0x1008_ff30), "Super+0x1008ff30");
    assert_eq!(bind_name(0, XK_Z), "z");
}
