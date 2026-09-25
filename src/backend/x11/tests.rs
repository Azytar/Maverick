//! Deterministic tests for the shared XKB resolver and its core fallback.

use super::*;
use x11rb::protocol::xkb::{KTMapEntry, KeySymMap, KeyType, ModDef};

const MIN: u8 = 8;
const SUPER: u16 = 0x0040;
const SHIFT: u16 = 0x0001;
const CONTROL: u16 = 0x0004;
const LOCK: u16 = 0x0002;
const NUM: u16 = 0x0010;
const M3: u16 = 0x0020;
const M5: u16 = 0x0080;
const XK_A: u32 = 0x0061;
const XK_A_UPPER: u32 = 0x0041;
const XK_B: u32 = 0x0062;
const XK_C: u32 = 0x0063;
const XK_D: u32 = 0x0064;
const XK_G: u32 = 0x0067;
const XK_H: u32 = 0x0068;
const XK_J: u32 = 0x006a;
const XK_K: u32 = 0x006b;
const XK_Z: u32 = 0x007a;
const XK_Z_UPPER: u32 = 0x005a;
const XK_BRACKETRIGHT: u32 = 0x5d;
const XK_DEAD_GRAVE: u32 = 0xfe50;
const XK_DEAD_ACUTE: u32 = 0xfe51;
const XK_DEAD_CIRCUMFLEX: u32 = 0xfe52;
const XK_DEAD_TILDE: u32 = 0xfe7e;
const XK_BRACKETLEFT: u32 = 0x5b;
const XK_BRACELEFT: u32 = 0x7b;
const XK_BRACERIGHT: u32 = 0x7d;
const XK_KP_HOME: u32 = 0xff95;
const XK_KP_END: u32 = 0xff9c;
const XK_KP_1: u32 = 0xffb1;
const XK_KP_7: u32 = 0xffb7;
const XK_F1: u32 = 0xffbe;

const KILL: Action = Action::Kill;
const QUIT: Action = Action::Quit;

fn kt_entry(mask: u16, level: u8) -> KTMapEntry {
    KTMapEntry {
        active: true,
        mods_mask: mask.into(),
        level,
        mods_mods: mask.into(),
        mods_vmods: 0u16.into(),
    }
}

fn preserve_shift(entry: &KTMapEntry) -> ModDef {
    let real_mods = u16::from(entry.mods_mask) & SHIFT;
    ModDef {
        mask: 0u16.into(),
        real_mods: real_mods.into(),
        vmods: 0u16.into(),
    }
}

fn key_type(mask: u16, num_levels: u8, entries: Vec<KTMapEntry>) -> KeyType {
    KeyType {
        mods_mask: mask.into(),
        mods_mods: mask.into(),
        mods_vmods: 0u16.into(),
        num_levels,
        has_preserve: true,
        preserve: entries.iter().map(preserve_shift).collect(),
        map: entries,
    }
}

fn one_level_type() -> KeyType {
    key_type(0, 1, vec![kt_entry(0, 0)])
}

fn two_level_type() -> KeyType {
    key_type(SHIFT, 2, vec![kt_entry(0, 0), kt_entry(SHIFT, 1)])
}

fn five_level_type() -> KeyType {
    key_type(
        SHIFT | M3,
        5,
        vec![kt_entry(0, 0), kt_entry(SHIFT, 1), kt_entry(M3, 4)],
    )
}

fn control_super_type() -> KeyType {
    key_type(
        CONTROL | SUPER,
        3,
        vec![
            kt_entry(0, 0),
            kt_entry(CONTROL, 1),
            kt_entry(SUPER, 1),
            kt_entry(CONTROL | SUPER, 2),
        ],
    )
}

fn alphabet_type() -> KeyType {
    key_type(
        SHIFT | LOCK | M5,
        4,
        vec![
            kt_entry(0, 0),
            kt_entry(SHIFT, 1),
            kt_entry(LOCK, 1),
            kt_entry(SHIFT | LOCK, 0),
            kt_entry(M5, 2),
            kt_entry(SHIFT | M5, 3),
        ],
    )
}

fn keypad_type() -> KeyType {
    key_type(
        SHIFT | LOCK | NUM,
        4,
        vec![
            kt_entry(0, 0),
            kt_entry(NUM, 1),
            kt_entry(SHIFT, 2),
            kt_entry(SHIFT | NUM, 3),
            kt_entry(LOCK, 0),
            kt_entry(SHIFT | LOCK, 2),
        ],
    )
}

fn row(width: u8, groups: u8, type_index: u8, syms: &[u32]) -> KeySymMap {
    KeySymMap {
        kt_index: [type_index; 4],
        group_info: groups,
        width,
        syms: syms.to_vec(),
    }
}

fn keyboard_map(first_key_sym: u8, rows: Vec<KeySymMap>, key_type: KeyType) -> XkbKeyboardMap {
    XkbKeyboardMap {
        first_type: 0,
        first_key_sym,
        types: vec![one_level_type(), key_type],
        keys: rows,
    }
}

fn xkb_layout(map: &XkbKeyboardMap, group: u8, numlock: u16, scroll: u16) -> ActiveLayout<'_> {
    ActiveLayout {
        keysyms: &[],
        min: map.first_key_sym,
        kpk: 0,
        xkb: Some(map),
        group,
        numlock,
        scroll,
    }
}

fn core_layout(keysyms: &[u32]) -> ActiveLayout<'_> {
    ActiveLayout {
        keysyms,
        min: MIN,
        kpk: 4,
        ..ActiveLayout::default()
    }
}

fn bindings(entries: &[(u16, u32, Action)]) -> BTreeMap<(u16, u32), Action> {
    entries
        .iter()
        .map(|(mask, keysym, action)| ((*mask, normalize_ksym(*keysym)), action.clone()))
        .collect()
}

fn resolved_for(map: &XkbKeyboardMap, code: u8, core: u16, group: u8) -> ResolvedKey {
    xkb_layout(map, group, 0, 0)
        .resolve_key(code, KeyLookupState { core, group })
        .expect("synthetic keymap must resolve")
}

fn assert_every_planned_grab_dispatches(
    plan: &KeyGrabPlan,
    layout: ActiveLayout<'_>,
    keymap: &BTreeMap<(u16, u32), Action>,
) {
    for &(raw_mask, keysym, code) in &plan.grabs {
        let resolved = layout
            .resolve_key(
                code,
                KeyLookupState {
                    core: raw_mask,
                    group: layout.group,
                },
            )
            .unwrap_or_else(|| panic!("planned grab {raw_mask:#x} on {code} has no resolved key"));
        let mods = clean_mask(raw_mask, layout.numlock, layout.scroll);
        assert!(
            resolve_binding(keymap, layout, resolved, mods).is_some(),
            "planned grab {raw_mask:#x} on {code} for {keysym:#x} cannot dispatch: resolved={resolved:?} binding_mods={mods:#x}"
        );
    }
}

#[test]
fn resolver_uses_all_xkb_levels_and_groups() {
    let map = keyboard_map(
        9,
        vec![row(
            4,
            1,
            1,
            &[XK_A, XK_A_UPPER, XK_BRACKETLEFT, XK_BRACELEFT],
        )],
        alphabet_type(),
    );
    let layout = xkb_layout(&map, 0, 0, 0);

    let cases = [
        (0, 0, XK_A, 0),
        (SHIFT, 1, XK_A_UPPER, SHIFT),
        (M5, 2, XK_BRACKETLEFT, M5),
        (SHIFT | M5, 3, XK_BRACELEFT, SHIFT | M5),
    ];
    for (core, level, keysym, required_core) in cases {
        let resolved = layout
            .resolve_key(9, KeyLookupState { core, group: 0 })
            .expect("level must resolve");
        assert_eq!(resolved.level, level);
        assert_eq!(resolved.keysym, keysym);
        assert_eq!(resolved.required_core, required_core);
    }
}

#[test]
fn resolver_handles_level_five_without_clamping() {
    let map = keyboard_map(
        9,
        vec![row(5, 1, 1, &[XK_A, XK_A_UPPER, XK_B, XK_C, XK_D])],
        five_level_type(),
    );
    let resolved = resolved_for(&map, 9, M3, 0);
    assert_eq!(resolved.level, 4);
    assert_eq!(resolved.keysym, XK_D);
    assert_eq!(resolved.required_core, M3);
}

#[test]
fn a_level_missing_from_the_row_width_resolves_to_no_key() {
    let map = keyboard_map(9, vec![row(1, 1, 1, &[XK_A])], two_level_type());
    let layout = xkb_layout(&map, 0, 0, 0);
    assert!(layout
        .resolve_key(9, KeyLookupState { core: 0, group: 0 })
        .is_some());
    assert!(layout
        .resolve_key(
            9,
            KeyLookupState {
                core: SHIFT,
                group: 0,
            }
        )
        .is_none());
}

#[test]
fn planner_dispatch_inverse_handles_multiple_configurable_selectors() {
    let map = keyboard_map(
        9,
        vec![row(3, 1, 1, &[XK_A, XK_B, XK_C])],
        control_super_type(),
    );
    let layout = xkb_layout(&map, 0, NUM, 0);
    let binds = [(CONTROL, XK_C, KILL)];
    let plan = plan_key_grabs(
        &binds
            .iter()
            .map(|(mask, keysym, _)| (*mask, *keysym))
            .collect::<Vec<_>>(),
        layout,
    );
    assert!(plan.grabs.contains(&(CONTROL | SUPER, XK_C, 9)));
    assert!(plan.missing.is_empty());

    let keymap = bindings(&binds);
    assert_every_planned_grab_dispatches(&plan, layout, &keymap);
    let resolved = layout
        .resolve_key(
            9,
            KeyLookupState {
                core: CONTROL | SUPER,
                group: 0,
            },
        )
        .unwrap();
    assert_eq!(resolved.level, 2);
    assert_eq!(resolved.required_core, CONTROL | SUPER);
    assert_eq!(
        resolve_binding(
            &keymap,
            layout,
            resolved,
            clean_mask(CONTROL | SUPER, NUM, 0),
        ),
        Some(((CONTROL, XK_C), KILL))
    );
}

#[test]
fn planner_rejects_a_final_grab_state_that_falls_outside_the_xkb_rules() {
    let key_type = key_type(
        SHIFT | LOCK,
        3,
        vec![kt_entry(0, 0), kt_entry(LOCK, 1), kt_entry(SHIFT, 2)],
    );
    let map = keyboard_map(9, vec![row(3, 1, 1, &[XK_A, XK_K, XK_B])], key_type);
    let layout = xkb_layout(&map, 0, 0, 0);
    let plan = plan_key_grabs(&[(SHIFT, XK_K)], layout);
    assert!(plan.grabs.is_empty());
    assert_eq!(plan.missing, vec![(SHIFT, XK_K)]);
}

#[test]
fn shift_and_capslock_keep_existing_letter_binding_semantics() {
    let map = keyboard_map(
        9,
        vec![row(
            4,
            1,
            1,
            &[XK_A, XK_A_UPPER, XK_BRACKETLEFT, XK_BRACELEFT],
        )],
        alphabet_type(),
    );
    let keymap = bindings(&[(SUPER, XK_A, KILL), (SUPER | SHIFT, XK_A, KILL)]);
    let layout = xkb_layout(&map, 0, NUM, 0);

    for core in [0, SHIFT, LOCK, SHIFT | LOCK] {
        let resolved = layout
            .resolve_key(9, KeyLookupState { core, group: 0 })
            .unwrap();
        let mods = clean_mask(core | SUPER, NUM, 0);
        assert!(
            resolve_binding(&keymap, layout, resolved, mods).is_some(),
            "core state {core:#x} resolved {resolved:?} with binding mask {mods:#x}"
        );
    }

    let plan = plan_key_grabs(&[(SUPER, XK_A)], layout);
    assert!(plan.grabs.iter().any(|&(mask, keysym, code)| {
        mask & (SHIFT | M5 | NUM) == 0 && keysym == XK_A && code == 9
    }));
    assert!(plan.missing.is_empty());

    let uppercase = plan_key_grabs(&[(SUPER, XK_A_UPPER)], layout);
    assert!(uppercase.grabs.contains(&(SUPER | SHIFT, XK_A_UPPER, 9)));
    assert!(!uppercase.grabs.iter().any(|&(mask, _, _)| mask == SUPER));
    let uppercase_keymap = bindings(&[(SUPER, XK_A_UPPER, KILL)]);
    assert_every_planned_grab_dispatches(&uppercase, layout, &uppercase_keymap);
    let resolved = layout
        .resolve_key(
            9,
            KeyLookupState {
                core: SUPER | SHIFT,
                group: 0,
            },
        )
        .unwrap();
    assert_eq!(
        resolve_binding(
            &uppercase_keymap,
            layout,
            resolved,
            clean_mask(SUPER | SHIFT, NUM, 0),
        ),
        Some(((SUPER, XK_A), KILL))
    );
}

#[test]
fn level_three_planner_and_dispatch_share_the_same_resolution() {
    let map = keyboard_map(
        9,
        vec![
            row(
                4,
                1,
                1,
                &[
                    XK_DEAD_CIRCUMFLEX,
                    XK_DEAD_TILDE,
                    XK_BRACKETLEFT,
                    XK_BRACELEFT,
                ],
            ),
            row(
                4,
                1,
                1,
                &[XK_DEAD_GRAVE, XK_DEAD_ACUTE, XK_BRACKETRIGHT, XK_BRACERIGHT],
            ),
        ],
        alphabet_type(),
    );
    let layout = xkb_layout(&map, 0, NUM, 0);
    let binds = [
        (SUPER, XK_BRACKETLEFT, KILL),
        (SUPER, XK_BRACKETRIGHT, QUIT),
        (SUPER | SHIFT, XK_BRACELEFT, KILL),
    ];
    let shortcut_bindings = binds.iter().map(|(m, k, _)| (*m, *k)).collect::<Vec<_>>();
    let plan = plan_key_grabs(&shortcut_bindings, layout);

    assert!(plan.grabs.contains(&(SUPER | M5, XK_BRACKETLEFT, 9)));
    assert!(plan.grabs.contains(&(SUPER | M5, XK_BRACKETRIGHT, 10)));
    assert!(plan.grabs.contains(&(SUPER | SHIFT | M5, XK_BRACELEFT, 9)));
    assert!(plan.grabs.iter().all(|&(mask, keysym, code)| {
        match (keysym, code) {
            (XK_BRACKETLEFT, 9) | (XK_BRACKETRIGHT, 10) => mask & M5 != 0,
            (XK_BRACELEFT, 9) => mask & (SHIFT | M5) == SHIFT | M5,
            _ => false,
        }
    }));
    assert!(plan.missing.is_empty());
    assert!(
        !plan.grabs.iter().any(|&(mask, keysym, _)| {
            matches!(keysym, XK_BRACKETLEFT | XK_BRACKETRIGHT) && mask == SUPER
        }),
        "a LevelThree keysym must never be grabbed without Mod5"
    );

    let keymap = bindings(&binds);
    assert_every_planned_grab_dispatches(&plan, layout, &keymap);
    for (code, dead, keysym) in [
        (9, XK_DEAD_CIRCUMFLEX, XK_BRACKETLEFT),
        (10, XK_DEAD_GRAVE, XK_BRACKETRIGHT),
    ] {
        let without_level_three = resolved_for(&map, code, SUPER, 0);
        assert_eq!(without_level_three.keysym, dead);
        assert!(resolve_binding(
            &keymap,
            layout,
            without_level_three,
            clean_mask(SUPER, NUM, 0),
        )
        .is_none());

        let level_three = resolved_for(&map, code, SUPER | M5, 0);
        assert_eq!(level_three.level, 2);
        assert_eq!(level_three.keysym, keysym);
        assert_eq!(level_three.required_core, M5);
        assert_eq!(
            resolve_binding(&keymap, layout, level_three, clean_mask(SUPER | M5, NUM, 0),),
            Some(((SUPER, keysym), if code == 9 { KILL } else { QUIT }))
        );
    }

    let level_four = resolved_for(&map, 9, SUPER | SHIFT | M5, 0);
    assert_eq!(level_four.level, 3);
    assert_eq!(level_four.keysym, XK_BRACELEFT);
    assert_eq!(
        resolve_binding(
            &keymap,
            layout,
            level_four,
            clean_mask(SUPER | SHIFT | M5, NUM, 0),
        ),
        Some(((SUPER | SHIFT, XK_BRACELEFT), KILL))
    );
}

#[test]
fn keypad_numlock_selects_the_planned_and_dispatched_level() {
    let map = keyboard_map(
        87,
        vec![row(4, 1, 1, &[XK_KP_END, XK_KP_1, XK_KP_HOME, XK_KP_7])],
        keypad_type(),
    );
    let layout = xkb_layout(&map, 0, NUM, 0);
    let binds = [
        (SUPER, XK_KP_END, KILL),
        (SUPER, XK_KP_1, QUIT),
        (SUPER, XK_KP_HOME, KILL),
        (SUPER | SHIFT, XK_KP_7, KILL),
    ];
    let shortcut_bindings = binds.iter().map(|(m, k, _)| (*m, *k)).collect::<Vec<_>>();
    let plan = plan_key_grabs(&shortcut_bindings, layout);

    assert!(plan
        .grabs
        .iter()
        .any(|&(mask, keysym, code)| mask & NUM == 0 && keysym == XK_KP_END && code == 87));
    assert!(plan
        .grabs
        .iter()
        .any(|&(mask, keysym, code)| mask & NUM != 0 && keysym == XK_KP_1 && code == 87));
    assert!(
        !plan
            .grabs
            .iter()
            .any(|&(mask, keysym, _)| mask & NUM != 0 && keysym == XK_KP_END),
        "NumLock must not be added to the level-zero keypad binding"
    );

    let keymap = bindings(&binds);
    assert_every_planned_grab_dispatches(&plan, layout, &keymap);
    let cases = [
        (SUPER, XK_KP_END, KILL),
        (SUPER | NUM, XK_KP_1, QUIT),
        (SUPER | SHIFT, XK_KP_HOME, KILL),
        (SUPER | SHIFT | NUM, XK_KP_7, KILL),
    ];
    for (core, keysym, _) in cases {
        let resolved = layout
            .resolve_key(87, KeyLookupState { core, group: 0 })
            .unwrap();
        assert_eq!(resolved.keysym, keysym);
        let mods = clean_mask(core, NUM, 0);
        assert!(resolve_binding(&keymap, layout, resolved, mods).is_some());
    }
}

#[test]
fn shift_binding_keeps_the_named_level_zero_keysym_compatibility() {
    let map = keyboard_map(
        34,
        vec![row(2, 1, 1, &[XK_BRACKETLEFT, XK_BRACELEFT])],
        two_level_type(),
    );
    let layout = xkb_layout(&map, 0, 0, 0);
    let binds = [(SUPER | SHIFT, XK_BRACKETLEFT, KILL)];
    let plan = plan_key_grabs(
        &binds
            .iter()
            .map(|(mask, keysym, _)| (*mask, *keysym))
            .collect::<Vec<_>>(),
        layout,
    );
    assert!(plan.grabs.contains(&(SUPER | SHIFT, XK_BRACKETLEFT, 34)));
    let keymap = bindings(&binds);
    assert_every_planned_grab_dispatches(&plan, layout, &keymap);

    let effective = layout
        .resolve_key(
            34,
            KeyLookupState {
                core: SUPER | SHIFT,
                group: 0,
            },
        )
        .unwrap();
    assert_eq!(effective.keysym, XK_BRACELEFT);
    assert_eq!(
        resolve_binding(&keymap, layout, effective, SUPER | SHIFT),
        Some(((SUPER | SHIFT, XK_BRACKETLEFT), KILL))
    );
}

#[test]
fn lock_variants_dispatch_even_when_the_type_does_not_select_lock() {
    let map = keyboard_map(9, vec![row(1, 1, 0, &[XK_A])], one_level_type());
    let layout = xkb_layout(&map, 0, NUM, 0);
    let binds = [(SUPER, XK_A, KILL)];
    let plan = plan_key_grabs(
        &binds
            .iter()
            .map(|(mask, keysym, _)| (*mask, *keysym))
            .collect::<Vec<_>>(),
        layout,
    );
    assert!(plan.grabs.contains(&(SUPER, XK_A, 9)));
    assert!(plan.grabs.contains(&(SUPER | LOCK, XK_A, 9)));
    let keymap = bindings(&binds);
    assert_every_planned_grab_dispatches(&plan, layout, &keymap);
}

#[test]
fn planner_and_dispatch_follow_the_active_group_for_dvorak() {
    let map = keyboard_map(
        43,
        vec![row(1, 2, 0, &[XK_H, XK_D]), row(1, 2, 0, &[XK_G, XK_H])],
        one_level_type(),
    );

    let qwerty = xkb_layout(&map, 0, 0, 0);
    let dvorak = xkb_layout(&map, 1, 0, 0);
    let qwerty_plan = plan_key_grabs(&[(SUPER, XK_H)], qwerty);
    let dvorak_plan = plan_key_grabs(&[(SUPER, XK_H)], dvorak);

    assert!(qwerty_plan
        .grabs
        .iter()
        .any(|&(_, keysym, code)| keysym == XK_H && code == 43));
    assert!(dvorak_plan
        .grabs
        .iter()
        .any(|&(_, keysym, code)| keysym == XK_H && code == 44));
    assert!(!dvorak_plan.grabs.iter().any(|&(_, _, code)| code == 43));

    let resolved = dvorak
        .resolve_key(
            44,
            KeyLookupState {
                core: SUPER,
                group: 1,
            },
        )
        .unwrap();
    assert_eq!(resolved.group, 1);
    assert_eq!(resolved.keysym, XK_H);
}

#[test]
fn group_redirect_policy_is_applied_before_lookup() {
    let mut redirected = row(1, 2, 0, &[XK_H, XK_J]);
    redirected.group_info |= 0x80 | (1 << 4);
    let map = keyboard_map(43, vec![redirected], one_level_type());
    let resolved = xkb_layout(&map, 2, 0, 0).resolve_key(43, KeyLookupState { core: 0, group: 2 });
    assert!(resolved.is_some_and(|resolved| resolved.group == 1 && resolved.keysym == XK_J));
}

#[test]
fn core_fallback_preserves_level_zero_and_shift() {
    let keysyms = [
        XK_A, XK_A_UPPER, XK_Z, XK_Z_UPPER, XK_Z, XK_Z_UPPER, XK_A, XK_A_UPPER, XK_A, XK_A_UPPER,
        XK_Z, XK_Z_UPPER, XK_A, XK_A_UPPER, XK_Z, XK_Z_UPPER,
    ];
    let layout = core_layout(&keysyms);

    assert_eq!(
        layout
            .resolve_key(MIN, KeyLookupState { core: 0, group: 0 })
            .unwrap()
            .keysym,
        XK_A
    );
    assert_eq!(
        layout
            .resolve_key(
                MIN,
                KeyLookupState {
                    core: SHIFT,
                    group: 0,
                },
            )
            .unwrap()
            .keysym,
        XK_A_UPPER
    );
    assert_eq!(dispatch_col(false, false, 4), 0);
    assert_eq!(dispatch_col(true, false, 4), 1);
    assert_eq!(dispatch_col(false, true, 4), 1);
    assert_eq!(dispatch_col(true, true, 4), 0);

    let plan = plan_key_grabs(&[(SUPER, XK_Z)], layout);
    assert!(
        plan.grabs
            .iter()
            .any(|&(mask, keysym, code)| mask == SUPER && keysym == XK_Z && code == MIN + 1),
        "core planner must retain the base grab: {plan:?}"
    );
}

#[test]
fn core_fallback_plans_every_keymap_row() {
    let mut keysyms = vec![XK_A; 20];
    keysyms[16..20].copy_from_slice(&[XK_Z, XK_Z_UPPER, XK_A, XK_A_UPPER]);
    let plan = plan_key_grabs(&[(SUPER, XK_Z)], core_layout(&keysyms));
    assert!(plan
        .grabs
        .iter()
        .any(|&(mask, keysym, code)| mask & (SHIFT | LOCK | NUM) == 0
            && keysym == XK_Z
            && code == MIN + 4));
    assert!(plan.missing.is_empty());
}

#[test]
fn num_and_scroll_lock_columns_use_full_protocol_keysyms() {
    let mut keysyms = vec![0u32; 24];
    keysyms[16] = 0xff7f;
    keysyms[20] = 0xff14;
    let mut modifier_codes = vec![0u8; 24];
    modifier_codes[8] = MIN + 4;
    modifier_codes[16] = MIN + 5;
    assert_eq!(
        compute_numlock(&modifier_codes, 8, &keysyms, 4, MIN, MIN + 19),
        0x02
    );
    assert_eq!(
        compute_scroll(&modifier_codes, 8, &keysyms, 4, MIN, MIN + 23),
        0x04
    );
}

#[test]
fn missing_keysym_is_reported_without_a_grab() {
    let keysyms = [
        XK_A, XK_A_UPPER, XK_Z, XK_Z_UPPER, XK_A, XK_A_UPPER, XK_Z, XK_Z_UPPER, XK_A, XK_A_UPPER,
        XK_Z, XK_Z_UPPER, XK_A, XK_A_UPPER, XK_Z, XK_Z_UPPER,
    ];
    let plan = plan_key_grabs(&[(SUPER, XK_F1)], core_layout(&keysyms));
    assert!(plan.grabs.is_empty());
    assert_eq!(plan.missing, vec![(SUPER, XK_F1)]);

    let empty = plan_key_grabs(
        &[(SUPER, XK_Z)],
        ActiveLayout {
            min: MIN,
            ..ActiveLayout::default()
        },
    );
    assert!(empty.grabs.is_empty());
    assert_eq!(empty.missing, vec![(SUPER, XK_Z)]);
}

#[test]
fn action_keys_are_normalized_for_dispatch() {
    let cfg = Cfg {
        keybinds: vec![(0, 0x0041, Action::Kill)],
        ..Cfg::default()
    };
    let keymap = build_keymap(&cfg);
    assert!(keymap.contains_key(&(0, XK_A)));
    assert!(!keymap.contains_key(&(0, XK_A_UPPER)));

    let duplicate = Cfg {
        keybinds: vec![(0, XK_A, Action::Kill), (0, 0x0061, Action::Quit)],
        ..Cfg::default()
    };
    let duplicates = build_keymap(&duplicate);
    assert_eq!(duplicates.get(&(0, XK_A)), Some(&Action::Kill));
}

#[test]
fn clean_mask_strips_groups_and_configured_locks() {
    let numlock = NUM;
    let mod3 = 0x0020;
    assert_eq!(clean_mask(SUPER | 0x2000 | 0x4000, numlock, 0), SUPER);
    assert_eq!(clean_mask(SUPER | NUM | LOCK, numlock, 0), SUPER);
    assert_eq!(
        clean_mask(SUPER | SHIFT | mod3, numlock, mod3),
        SUPER | SHIFT
    );
}

#[test]
fn core_dispatch_column_never_reads_past_level_one() {
    for kpk in [1usize, 2, 4, 6] {
        for shift in [false, true] {
            for lock in [false, true] {
                let col = dispatch_col(shift, lock, kpk);
                assert!(col <= 1);
                assert!(col < kpk);
            }
        }
    }
}

#[test]
fn bind_names_round_trip_into_config_syntax() {
    assert_eq!(bind_name(SUPER | SHIFT, XK_A), "Super+Shift+a");
    assert_eq!(bind_name(SUPER, XK_BRACKETLEFT), "Super+bracketleft");
    assert_eq!(bind_name(SUPER, 0x1008_ff30), "Super+0x1008ff30");
    assert_eq!(bind_name(0, XK_Z), "z");
}

#[test]
fn keypress_state_separates_group_from_core_modifiers() {
    assert_eq!(
        KeyLookupState::from_x11_with_group(SUPER | SHIFT | M5, 0),
        KeyLookupState {
            core: SUPER | SHIFT | M5,
            group: 0,
        }
    );
    assert_eq!(
        KeyLookupState::from_x11_with_group(SUPER | (3 << 13), 0),
        KeyLookupState {
            core: SUPER,
            group: 3,
        }
    );
    assert_eq!(
        KeyLookupState::from_x11_with_group(SUPER, 1),
        KeyLookupState {
            core: SUPER,
            group: 1,
        }
    );
}
