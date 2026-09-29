# TEST-AUDIT — what the Maverick suite actually proves

Agent E, multi-agent architectural audit. Read-only audit: no `.rs`, `Cargo.toml` or `Cargo.lock`
was modified and no test was added or removed. This file is the only artefact.

## 1. Summary

### 1.1 Counts, by kind

| kind | count | evidence |
|---|---:|---|
| `#[test]` attributes in Rust source | **1094** | `rg -c "^\s*#\[test\]\s*$" --glob "*.rs"` -> 1094 across 72 files |
| distinct executable Rust test functions | **1090** | `cargo test --offline --workspace -- --list` -> 1090 `…: test` lines; the 4-name gap is `multi_step_invariants`, `small_dt_single_step`, `tick_consumes_substeps`, `zero_and_negative_yields_empty`, written twice — once in the gated `compositor_gl::substep_tests`, once in the ungated `compositor::placeholder_substep_tests` |
| of which proptest bodies (`proptest!`) | **234** | 211 named `proptest! { #[test] fn … }` + 23 closure-form `proptest!(|(x in …)| { … })` inside a `#[test]` fn, over 30 files |
| declared `[[test]]` targets | **1** | `maverick-sys/Cargo.toml:46-48` (`name = "ctl_props"`) |
| root integration test binaries | **3** | `tests/child_lifecycle.rs`, `tests/no_wait_in_wm.rs`, `tests/source_constraints.rs` (auto-discovered) |
| proptest regression seed files | **4** | `proptest-regressions/{core/layout,core/present,core/tests,backend/x11/render}.txt`, 48 `cc` seeds total |
| `#[ignore]`d tests | **2** | `maverick-vk/tests/smoke.rs:37,101` — both skipped unless `MAVERICK_VK_SMOKE=1` |
| C harness sources | **13** | `tests/*.c` (see §2.4) |
| shell suites | **27** (26 suites + `common.sh` helper) | `tests/*.sh` |
| python suites | **4** | `tests/*.py` |
| CI workflows | **1** (3 jobs) | `.github/workflows/ci.yml` |

### 1.2 Per-package counts (measured from the built test binaries, not estimated)

`cargo test --offline --workspace --no-run` then `<binary> --list` per target:

| package | unit (`src/`) | integration (`tests/`) | total |
|---|---:|---:|---:|
| `maverick (root bin+src)` | 588 | 9 | **597** |
| `maverick-core` | 39 | 51 | **90** |
| `maverick-sys` | 157 | 42 | **199** |
| `maverick-x11` | 4 | 13 | **17** |
| `maverick-toml` | 27 | 9 | **36** |
| `maverick-img` | 48 | 2 | **50** |
| `maverick-gl` | 41 | 21 | **62** |
| `maverick-render` | 0 | 3 | **3** |
| `maverick-vk` | 8 | 28 | **36** |
| **total** | **906** | **184** | **1090** |

### 1.3 The headline finding

**The suite is not primarily a window-manager suite.** Of 1094 `#[test]` attributes, only
**387** (`CORE`) assert window-manager behaviour with no X, no GL and no OS boundary in the way;
**127** more (`X11`) assert X11-backend behaviour a non-composited WM still needs. The remaining
**580** either test a compositor that is about to be deleted (**234**), test `maverick-sys` plus
the two surviving root integration binaries (**206** — these survive for reasons unrelated to
compositing), are proptests of crates that die (**24**), or are diagnostic/low-value (**16**).

More sharply, **272 of the 1094 tests (25 %) protect nothing a non-composited WM has:**

- **101** in three crates the replacement will not have: `maverick-gl` (62), `maverick-vk` (36),
  `maverick-render` (3). All 101 are still built, linted and *run* by CI today, because
  `ci.yml:24,27` say `--workspace` with default features.
- **153** in compositor modules inside the root crate: `compositor_gl.rs` 74,
  `compositor_policy.rs` 24, `framesched.rs` 24, `render.rs` Shape/render-list 24,
  `compositor.rs` (the no-GL stub's substep integrator) 4, `framebench.rs` 3.
  Only `compositor_gl.rs` is feature-gated — measured: 70 of its 74 disappear under
  `--no-default-features` (`src/backend/x11/compositor.rs:21-23`), and the 4 stub tests named
  identically appear in their place. The other **79** sit in **ungated** modules, so a
  `--no-default-features` build still compiles and runs all 79 of them.
- **14** diagnostic: `trace.rs` 11, `mod.rs` 3.
- **4** that parse keys or grep sources that go with the compositor: `userconfig.rs` 2,
  `source_constraints.rs` 2.

The second headline: **the tests that would catch a real window-manager bug are concentrated in
five files** — `src/core/tests.rs` (167), `src/core/layout.rs` (38),
`src/backend/x11/reconciler.rs` (38), `maverick-core/src/types.rs` (36),
`src/core/invariants.rs` (24) — while the window manager's actual *entry points* have **no unit
tests at all**: `src/backend/x11/manage.rs` (1367 lines, 0), `src/backend/x11/events.rs` (1097, 0),
`src/backend/x11/pointer.rs` (729, 0), `src/backend/x11/actions.rs` (551, 0),
`src/backend/x11/input.rs` (446, 0). **4,911 lines with no `#[cfg(test)]` block at all.** See §6.1.

## 2. Full inventory

Every row is `name | file:line | crate | class | asserts | verdict`.
`class` is the *primary* class; `PROPERTY` marks a proptest-driven test, `LEGACY` a
low-value/diagnostic one, `INTEGRATION` one that crosses a crate or process boundary.
A test with a pinned proptest seed or a `audit_*` / explicit-regression name is tagged
`REGRESSION` in the verdict column as `KEEP(R)`.

### 2.1 Rust tests, grouped by file

#### `maverick-core/src/types.rs` — 36 tests, crate `maverick-core`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `top_dock_reserves_top_only` | 2757 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `bottom_dock_reserves_bottom_only` | 2771 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `two_docks_stack_on_same_edge` | 2785 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `removing_external_dock_restores_workarea` | 2795 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `left_and_right_docks_shrink_width` | 2807 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `zero_thickness_region_is_removal` | 2815 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `b4_single_dock_reserves_multiple_edges` | 2825 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `hostile_strut_values_never_escape_the_screen` | 2851 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `reservation_larger_than_the_screen_collapses_onto_its_edge` | 2879 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `screen_origin_near_i32_max_survives_a_reservation` | 2902 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `union_spanning_two_rects_is_the_bounding_box` | 2917 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `union_with_overlapping_rect_is_their_bounds` | 2924 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `union_with_itself_is_unchanged` | 2931 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `contains_rect_is_true_only_when_fully_inside` | 2937 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `splitting_a_column_at_the_band_floor_keeps_every_half_in_band` | 2995 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `splitting_a_column_with_room_still_divides_it_evenly` | 3028 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `stiffness_zero_negative_and_non_finite_fall_back_to_default` | 3042 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `damping_negative_and_non_finite_fall_back_to_default` | 3051 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `sanitize_spring_is_idempotent_for_invalid_damping` | 3058 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `damping_extreme_is_bounded_relative_to_stiffness` | 3075 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. *(impl-detail)* | KEEP |
| `camera_analytic_regimes_finite_and_convergent` | 3089 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `camera_snap_is_exact_even_with_tiny_dt` | 3117 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `camera_step_survives_non_finite_dt_and_poisoned_state` | 3128 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `camera_parks_at_the_offsets_where_the_old_rounding_could_not` | 3170 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP(R) |
| `camera_parks_through_the_slowest_sanitized_springs` | 3204 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP(R) |
| `camera_near_equilibrium_is_neither_cut_short_nor_ignored` | 3231 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `camera_trajectory_is_frame_rate_independent` | 3265 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `camera_retarget_keeps_visual_state_and_resets_velocity` | 3279 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `camera_snap_and_zero_delta_are_not_motion` | 3297 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `camera_repeated_direction_changes_do_not_teleport` | 3315 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `camera_step_with_extreme_spring_does_not_diverge` | 3329 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `camera_step_terminates_with_valid_configuration` | 3346 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `camera_step_terminates_even_with_zero_or_negative_damping` | 3358 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `spring_smooth_ignores_non_finite_inputs` | 3382 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `spring_smooth_zero_dt_keeps_pending_state_and_snaps_endpoint` | 3394 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |
| `camera_step_terminates_and_converges_for_every_degenerate_direct_mutation` | 3413 | CORE | workarea derivation, column weights, spring/camera convergence or float placement. | KEEP |

#### `maverick-core/src/wallpaper.rs` — 3 tests, crate `maverick-core`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `fill_covers_output` | 249 | CORE | wallpaper fit mode (fill / fit / stretch) output rect. | KEEP |
| `fit_letterboxes` | 264 | CORE | wallpaper fit mode (fill / fit / stretch) output rect. | KEEP |
| `stretch_ignores_aspect` | 274 | CORE | wallpaper fit mode (fill / fit / stretch) output rect. | KEEP |

#### `maverick-core/tests/animation_props.rs` — 15 tests, crate `maverick-core`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `a_poisoned_camera_snaps_back_to_its_target` | 35 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `retarget_keeps_the_visual_position_and_drops_stale_momentum` | 100 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `a_reversal_through_retarget_turns_around_immediately_and_a_field_write_does_not` | 142 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `retarget_refuses_a_poisoned_destination` | 193 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `sanitize_spring_lands_in_the_effective_domain_and_is_idempotent` | 217 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `a_rejected_damper_always_yields_the_same_spring` | 239 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `a_camera_always_converges_onto_its_target` | 310 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `a_camera_stays_bounded_at_any_magnitude` | 360 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `a_camera_parks_itself_however_far_it_travelled` | 410 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `arrival_is_reported_honestly_and_a_non_positive_delta_never_moves_the_camera` | 441 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `a_camera_poisoned_in_any_single_field_still_looks_animated` | 512 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `a_camera_exactly_at_both_settle_thresholds_is_settled` | 533 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `one_representable_step_outside_a_settle_threshold_still_animates` | 554 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `the_settle_verdict_does_not_depend_on_the_direction_of_motion` | 578 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `spring_smooth_converges_without_poisoning_or_overshooting` | 603 | PROPERTY | proptest over the named pure invariant. | KEEP |

#### `maverick-core/tests/rect_geometry_props.rs` — 8 tests, crate `maverick-core`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `edges_never_move_backwards_past_the_origin` | 23 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `containment_is_half_open_on_the_right_and_bottom_edges` | 42 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `rect_containment_agrees_with_point_containment` | 68 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `union_is_the_commutative_minimal_envelope` | 88 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `area_is_computed_without_wrapping` | 135 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `containment_covers_the_whole_box_the_fields_describe` | 154 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `rect_containment_agrees_with_the_boxes_it_describes` | 193 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `the_union_covers_both_rects_whatever_the_coordinates` | 232 | PROPERTY | proptest over the named pure invariant. | KEEP |

#### `maverick-core/tests/reservation_props.rs` — 10 tests, crate `maverick-core`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `per_edge_totals_only_grow_and_do_not_depend_on_order` | 24 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `an_empty_area_means_no_reservation_reserves_anything` | 59 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `workarea_never_escapes_its_screen` | 73 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `more_reservation_never_yields_a_larger_workarea` | 89 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `reservation_mutations_keep_the_derived_geometry_in_sync` | 137 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `registering_and_removing_a_dock_restores_the_workarea` | 200 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `reconcile_workspaces_bounds_the_slots_and_keeps_the_survivors` | 219 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `a_stale_active_workspace_is_clamped_rather_than_fatal` | 260 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `the_selected_monitor_accessor_clamps_or_reports_nothing` | 278 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `a_point_resolves_to_the_output_under_it` | 308 | PROPERTY | proptest over the named pure invariant. | KEEP |

#### `maverick-core/tests/state_model_props.rs` — 18 tests, crate `maverick-core`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `a_monitor_is_usable_from_its_constructor_at_any_tag_count` | 31 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `states_built_through_the_public_api_satisfy_every_invariant` | 68 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `every_single_field_corruption_is_reported_by_its_own_check` | 86 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `removing_a_window_leaves_no_reference_anywhere` | 119 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `removing_the_x11_focused_client_clears_the_focus_mirror` | 186 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `removing_another_client_leaves_the_focus_mirror_alone` | 214 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `a_workspace_script_keeps_the_focus_pointers_honest` | 246 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `dropping_empty_columns_keeps_the_focus_on_the_same_window` | 330 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `a_tiling_request_always_yields_an_in_bounds_column` | 377 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `rebalancing_repairs_only_the_broken_weights` | 405 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `splitting_a_column_keeps_every_weight_inside_the_documented_band` | 438 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `a_move_never_changes_the_focused_window_or_the_window_set` | 507 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `a_stale_focus_pointer_turns_every_move_into_a_no_op` | 555 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `every_flag_predicate_reads_only_its_documented_bit` | 585 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `the_ewmh_and_icccm_bit_layout_is_the_one_the_protocol_defines` | 637 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `adding_a_column_and_removing_it_again_restores_the_workspace` | 718 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `the_flag_mutators_form_a_set_algebra` | 758 | PROPERTY | proptest over the named pure invariant. | KEEP |
| `a_position_claim_needs_both_the_protocol_bit_and_a_valid_hint_word` | 823 | PROPERTY | proptest over the named pure invariant. | KEEP |

#### `maverick-gl/src/dl.rs` — 2 tests, crate `maverick-gl`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `gl_candidates_are_absolute_paths_first_and_nul_free` | 230 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `a_symbol_name_with_an_embedded_nul_never_resolves` | 266 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |

#### `maverick-gl/src/lib.rs` — 1 tests, crate `maverick-gl`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `extension_matching_is_token_exact` | 86 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |

#### `maverick-gl/src/renderer.rs` — 38 tests, crate `maverick-gl`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `a_positive_count_with_a_list_is_usable` | 2457 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `no_list_or_no_count_is_not_usable` | 2470 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `argb32_never_binds_through_a_10bit_config` | 2556 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `rgb24_binds_through_a_32bit_buffer_with_alpha_bits` | 2577 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `depth_must_match_the_configs_own_visual` | 2587 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `exact_visual_wins_over_same_depth` | 2602 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `a_narrower_config_is_rejected_a_wider_one_is_allowed` | 2614 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `dont_care_bind_targets_are_usable` | 2632 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `bind_capability_follows_the_texture_format` | 2644 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `colour_index_and_window_only_configs_are_skipped` | 2658 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `software_rasterizers_classify_as_software` | 2668 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `real_gpus_classify_as_gpu_regardless_of_vendor` | 2695 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `empty_renderer_info_classifies_unknown` | 2712 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `renderer_info_display_matches_startup_block` | 2717 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. *(impl-detail)* | REMOVE |
| `filter_maps_to_gl_constants` | 2734 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. *(impl-detail)* | REMOVE |
| `scissor_box_never_leaves_the_viewport` | 2818 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `scissor_box_passes_an_interior_damage_rect_through_unchanged` | 2851 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `scissor_box_never_shrinks_as_the_damage_grows` | 2883 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `visual_colour_bits_are_the_wide_sum_of_its_channels` | 2925 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `fbconfig_acceptance_is_exactly_what_the_visual_requires` | 3118 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `accepted_fbconfigs_rank_by_how_well_they_match` | 3150 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `a_pixmap_is_only_ever_bound_in_a_format_its_config_can_bind` | 3180 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `only_y_inverted_false_needs_a_flip` | 3208 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `reject_tally_reports_each_found_reason_with_its_count` | 3221 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. *(impl-detail)* | REMOVE |
| `premultiplied_upload_keeps_one_texel_per_pixel_and_the_source_alpha` | 3275 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `premultiplied_channels_never_exceed_their_alpha` | 3295 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `premultiplied_channels_are_rounded_to_nearest_and_monotone` | 3330 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `a_cpu_texture_is_only_the_handle_it_was_given` | 3369 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `glx_context_attribute_list_is_paired_and_zero_terminated` | 3396 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. *(impl-detail)* | REMOVE |
| `glx_context_is_new_enough_for_the_builtin_shaders` | 3431 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `draw_shader_hands_the_window_program_back` | 3712 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `a_shader_wallpaper_frame_still_reports_no_gl_error` | 3758 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `the_window_quad_after_a_shader_wallpaper_raises_no_invalid_operation` | 3795 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `a_window_quad_after_a_shader_wallpaper_writes_its_destination_rect` | 3839 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `draw_raw_then_shader_then_draw_leaves_the_window_program_current` | 3907 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `a_shader_wallpaper_drawn_last_leaves_a_clean_frame` | 3962 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `a_leak_does_not_cross_a_frame_boundary` | 4011 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `a_uniform_write_against_a_non_current_program_never_reaches_that_program` | 4075 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |

#### `maverick-gl/tests/loader.rs` — 8 tests, crate `maverick-gl`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `every_loaded_entry_point_is_pointer_sized_and_aligned` | 122 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. *(impl-detail)* | REMOVE |
| `a_name_argument_is_a_borrowed_c_string_that_is_already_terminated` | 140 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. *(impl-detail)* | REMOVE |
| `an_optional_entry_point_is_an_option_over_a_c_abi_function_pointer` | 168 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. *(impl-detail)* | REMOVE |
| `probing_for_a_driver_is_stable_and_never_fails` | 191 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `both_tables_load_completely_when_there_is_a_driver` | 216 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `symbol_resolution_works_without_a_context_and_names_what_is_missing` | 258 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `a_stubbed_symbol_still_needs_the_extension_token_to_be_called` | 318 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `the_re_exported_display_is_the_shared_bootstrap_type` | 350 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |

#### `maverick-gl/tests/props.rs` — 10 tests, crate `maverick-gl`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `extension_matching_reads_names_not_whitespace` | 82 | PROPERTY | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `an_extension_that_is_only_part_of_a_name_is_not_found` | 110 | PROPERTY | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `a_software_renderer_is_recognised_whichever_field_names_it` | 153 | PROPERTY | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `acceleration_classification_ignores_ascii_case` | 178 | PROPERTY | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `an_unknown_acceleration_needs_both_fields_empty` | 197 | PROPERTY | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `the_startup_report_has_one_line_per_field` | 217 | PROPERTY | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. *(impl-detail)* | REMOVE |
| `a_visual_only_claims_colour_it_carries` | 255 | PROPERTY | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `extension_matching_answers_odd_driver_strings` | 282 | PROPERTY | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |
| `filter_modes_map_to_distinct_gl_samplers` | 314 | PROPERTY | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. *(impl-detail)* | REMOVE |
| `capability_probe_answers_the_same_every_time` | 329 | PROPERTY | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. | REMOVE |

#### `maverick-gl/tests/shared_bootstrap.rs` — 3 tests, crate `maverick-gl`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `the_gl_display_handle_is_the_shared_one` | 19 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. *(impl-detail)* | REMOVE |
| `the_error_cell_is_the_installed_one` | 31 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. *(impl-detail)* | REMOVE |
| `x_error_names_come_from_the_shared_table` | 45 | COMPOSITOR | GLX/GL selection, fbconfig, scissor, premultiply or loader invariant. *(impl-detail)* | REMOVE |

#### `maverick-img/src/lib.rs` — 10 tests, crate `maverick-img`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `ppm_roundtrip_trivial` | 1283 | CORE | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `png_rgba2x2` | 1299 | CORE | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `png_rgb3x1` | 1309 | CORE | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `png_grayscale_alpha` | 1317 | CORE | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `png_palette_with_trns` | 1324 | CORE | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `png_paeth_filter` | 1331 | CORE | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `png_sub_filter` | 1340 | CORE | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `qoi_inline` | 1347 | CORE | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `bmp_inline` | 1377 | CORE | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `farbfeld_inline` | 1408 | CORE | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |

#### `maverick-img/tests/decode_dispatch.rs` — 2 tests, crate `maverick-img`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `decode_selects_the_native_decoder_named_by_the_extension` | 159 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `every_documented_extension_reaches_its_native_decoder` | 185 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |

#### `maverick-img/tests/properties/mod.rs` — 38 tests, crate `maverick-img`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `arbitrary_bytes_never_panic_or_decode_to_a_malformed_image` | 654 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `truncation_never_panics` | 684 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `trailing_garbage_does_not_change_the_decoded_pixels` | 701 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `decoding_is_deterministic` | 718 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `farbfeld_rejects_out_of_range_dimensions` | 739 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `a_zero_side_is_rejected` | 772 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `farbfeld_rejects_dimensions_past_max_dim` | 798 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `ppm_rejects_out_of_range_dimensions` | 811 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `ppm_rejects_unsupported_maxval` | 834 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `bmp_validates_the_dib_depth_compression_and_offset_fields` | 853 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `bmp_rejects_a_zero_side` | 897 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `qoi_rejects_unsupported_channel_counts` | 911 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `qoi_rejects_out_of_range_dimensions` | 931 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `png_rejects_unsupported_bit_depths_and_colour_types` | 961 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `png_validates_the_chunk_length_before_using_it` | 1001 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `ppm_rejects_a_short_pixel_region` | 1017 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `farbfeld_rejects_a_short_pixel_region` | 1030 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `bmp_row_stride_and_orientation_match_the_bytes_written` | 1047 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `qoi_decodes_to_the_pixels_the_encoder_was_given` | 1083 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `farbfeld_keeps_the_high_byte_of_every_16_bit_sample` | 1129 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `ppm_header_comments_and_whitespace_do_not_shift_the_pixels` | 1155 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `png_filters_reconstruct_the_scanlines_they_encode` | 1192 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `png_sample_expansion_matches_the_declared_bit_depth` | 1237 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `png_rejects_unknown_filter_bytes` | 1279 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `png_palette_indices_are_used_verbatim` | 1318 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `png_survives_hostile_compressed_data` | 1364 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `qoi_rejects_ops_past_the_declared_pixel_count` | 1395 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `qoi_run_length_cannot_overrun_the_declared_pixel_count` | 1421 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `a_png_cut_after_its_pixel_data_still_decodes` | 1445 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `degenerate_inputs_are_rejected_cleanly` | 1484 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `every_truncation_of_a_valid_file_is_safe` | 1514 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `one_bit_indices_share_one_byte` | 1590 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `two_bit_indices_share_one_byte` | 1614 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `four_bit_indices_in_one_byte_are_not_rescaled` | 1640 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `eight_bit_indices_use_one_byte_per_index` | 1663 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `every_index_of_every_depth_is_reachable` | 1688 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `a_row_ending_mid_byte_does_not_carry_its_bits_into_the_next_row` | 1726 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |
| `every_truncation_of_a_packed_indexed_file_is_safe` | 1770 | PROPERTY | image decoder: a format case, or a proptest that no byte string panics/mis-decodes. | KEEP |

#### `maverick-render/tests/contract_types.rs` — 3 tests, crate `maverick-render`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `acceleration_label_is_one_plain_line_and_unique` | 43 | COMPOSITOR | a neutral renderer-trait contract type behaves as documented. | REMOVE |
| `renderer_info_report_carries_every_field` | 72 | COMPOSITOR | a neutral renderer-trait contract type behaves as documented. | REMOVE |
| `default_draw_quad_draws_nothing` | 125 | COMPOSITOR | a neutral renderer-trait contract type behaves as documented. | REMOVE |

#### `maverick-sys/src/control.rs` — 16 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `the_server_records_this_process_uid_as_the_only_authorised_peer` | 713 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `peer_credentials_identify_the_process_not_the_path` | 735 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_undescribable_descriptor_is_an_error_not_a_uid` | 750 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `server_full_protocol` | 766 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_handler_slot_is_released_even_when_the_handler_unwinds` | 818 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `subscribers_streaming_oversized_events_leave_the_server_serving` | 860 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `subscribe_receives_events` | 908 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `subscribe_cap_holds_under_concurrent_registration` | 962 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `subscribe_cap_rejects_extras_from_concurrent_connections` | 1012 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `subscribe_cap_rejects_beyond_max` | 1071 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `every_reply_is_exactly_one_bounded_frame` | 1169 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_oversized_payload_is_still_answered_in_one_bounded_line` | 1189 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_character_straddling_the_bound_is_cut_back_to_its_prefix` | 1208 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `unknown_command_replies_echo_nothing_but_its_printable_prefix` | 1289 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_event_straddling_the_bound_is_streamed_as_its_utf8_prefix` | 1406 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_event_of_any_character_width_is_streamed_as_one_bounded_frame` | 1448 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/src/ctl/mod.rs` — 2 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `parse_opts_recovers_every_flag_and_positional` | 1142 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `parse_opts_invents_nothing_and_never_panics` | 1222 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/src/ctl/session.rs` — 15 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `a_commands_own_flags_reach_the_command` | 1410 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_explicit_session_makes_the_first_positional_the_command` | 1430 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_double_dash_hands_the_rest_over_untouched` | 1446 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_session_with_no_command_yields_no_command` | 1462 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_window_selector_is_never_the_session_name` | 1474 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `everything_after_the_separator_goes_to_maverick` | 1492 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `the_sessions_own_debug_flag_is_separate` | 1522 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `create_parses_every_option` | 1530 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `create_reports_bad_options_with_the_offending_value` | 1558 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `the_session_view_carries_the_xauth_path_and_never_its_contents` | 1589 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `the_session_view_is_valid_json_with_every_documented_field` | 1642 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `process_json_round_trips_through_the_parser` | 1689 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_window_filter_keeps_lines_that_name_no_window` | 1716 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `event_lines_are_matched_by_either_id_spelling` | 1730 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `table_cells_are_truncated_with_a_mark` | 1738 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/src/ctl/windows.rs` — 8 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `an_id_selector_addresses_exactly_that_window` | 693 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `names_resolve_exactly_or_not_at_all` | 712 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_unmatched_name_lists_what_is_there` | 744 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `flattening_reads_the_hierarchy_including_floats` | 763 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_windows_own_placement_beats_the_walk_position` | 807 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `flattening_a_missing_tree_yields_nothing` | 822 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `every_window_op_becomes_a_targeted_action` | 832 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `directions_are_recognised_case_insensitively` | 880 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/src/hub.rs` — 7 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `commands_round_trip` | 312 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `state_snapshot_publishes` | 328 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `events_reach_subscribers_and_prune` | 336 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `hub_clones_share_state` | 348 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `command_wakes_the_x11_poll_loop` | 358 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `command_queue_is_bounded_and_never_blocks` | 375 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `slow_subscriber_drops_but_stays_connected` | 396 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/src/identity.rs` — 18 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `meta_roundtrip` | 605 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `session_id_is_unique` | 629 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `session_dirs_are_isolated_per_sid` | 637 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `sock_path_fits_sun_len` | 651 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `sock_path_is_stable_and_isolated` | 671 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `start_time_reads_self` | 678 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `the_runtime_directory_is_private` | 686 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `runtime_dir_never_tmp` | 697 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_explicit_sid_names_every_path_the_instance_publishes` | 709 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_unsafe_sid_is_refused_rather_than_replaced` | 734 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_written_ficha_reads_back_identically` | 868 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `reading_a_hostile_ficha_never_panics_and_never_invents_a_session` | 905 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_escaped_quote_survives_the_roundtrip` | 941 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_escaped_backslash_survives_the_roundtrip` | 952 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_value_ending_in_a_backslash_survives_the_roundtrip` | 968 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_whitespace_only_value_survives_the_roundtrip` | 983 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_absent_field_defaults_instead_of_failing` | 1000 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `the_identity_record_is_owner_only` | 1029 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/src/json.rs` — 26 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `escape_covers_the_usual_specials` | 673 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `control_characters_use_unicode_escapes` | 679 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `unescape_handles_unicode_escapes` | 684 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `escape_unescape_roundtrip` | 690 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `scans_scalars_of_every_type` | 714 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_comma_inside_a_string_does_not_split_the_object` | 726 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_escaped_quote_stays_inside_the_value` | 735 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_trailing_backslash_does_not_eat_the_delimiter` | 744 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `arrays_round_trip_through_quote_array` | 751 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_array_element_ending_in_an_escaped_quote_survives` | 769 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_null_field_reads_as_absent_not_as_the_text_null` | 781 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_empty_array_is_not_an_absent_field` | 793 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_pretty_printed_object_reads_like_a_compact_one` | 808 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `hostile_input_terminates_without_inventing_fields` | 824 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `parses_every_value_kind` | 853 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `absent_and_wrongly_typed_fields_degrade_to_neutral_values` | 871 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `integers_survive_exactly` | 889 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `strings_decode_and_escapes_do_not_end_them_early` | 908 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `whitespace_between_tokens_is_insignificant` | 919 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `deep_nesting_is_refused_rather_than_overflowing` | 928 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_truncated_document_is_not_a_document` | 941 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_trailing_fragment_is_refused` | 959 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `the_number_grammar_is_enforced` | 971 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `the_literal_keywords_are_not_prefix_matched` | 981 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `values_round_trip_through_to_json` | 992 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `the_scanner_and_the_parser_agree_on_field_values` | 1010 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/src/lib.rs` — 4 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `a_zero_timeout_returns_without_blocking` | 119 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_timeout_longer_than_a_second_is_not_truncated_to_nanoseconds` | 135 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_signal_interrupting_the_wait_wakes_the_caller` | 156 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_readable_descriptor_is_reported` | 200 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/src/session/lifecycle.rs` — 8 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `a_relative_binary_is_made_absolute` | 755 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_bare_name_resolves_through_path` | 780 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_missing_binary_is_named_in_the_error` | 790 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_non_executable_file_is_not_a_binary` | 802 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `path_lookup_rejects_a_embedded_separator` | 817 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_session_that_was_never_written_is_not_removable` | 827 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `ownership_comes_from_the_process_not_from_input` | 838 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `only_the_backends_this_build_knows_are_accepted` | 849 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/src/session/mod.rs` — 18 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `a_stopped_session_owns_nothing` | 1280 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_root_without_a_start_time_is_not_a_root` | 1307 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `registered_groups_need_a_live_session` | 1325 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `resolution_round_trips_and_validates` | 1337 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_malformed_resolution_is_refused` | 1354 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `session_names_are_safe_path_components` | 1378 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_session_record_round_trips` | 1408 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_partial_record_still_parses` | 1444 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_record_without_a_usable_name_is_refused` | 1458 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `the_session_environment_is_exactly_what_a_client_needs` | 1480 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `states_round_trip_and_default_safely` | 1521 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_recorded_running_session_with_a_dead_wm_reads_as_crashed` | 1538 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_pid_ref_is_live_only_for_the_exact_process` | 1559 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `tail_returns_the_end_of_a_file` | 1575 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `tail_survives_chunk_boundaries_and_a_missing_final_newline` | 1591 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_empty_log_tails_to_nothing` | 1608 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `removal_refuses_a_symlink_where_the_directory_should_be` | 1621 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `errors_name_the_session_and_the_way_out` | 1642 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/src/session/proc.rs` — 17 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `reads_this_process` | 509 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_signal_that_was_not_delivered_is_an_error` | 527 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_missing_pid_is_not_an_error` | 549 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `stat_parsing_is_anchored_at_the_last_paren` | 560 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `closure_walks_transitively` | 571 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `closure_does_not_depend_on_enumeration_order` | 591 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `groups_find_a_reparented_child` | 607 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_zero_root_owns_nothing` | 621 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_zero_group_owns_nothing` | 636 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_refused_signal_is_not_reported_as_delivered` | 667 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_zombie_is_the_same_process_but_not_a_running_one` | 686 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `cpu_percent_is_bounded_by_the_process_lifetime` | 711 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `human_bytes_rounds_to_the_unit_a_listing_expects` | 727 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. *(impl-detail)* | KEEP |
| `display_name_never_returns_empty` | 736 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. *(impl-detail)* | KEEP |
| `pid_is_gates_on_the_recorded_start_time` | 768 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `marked_pids_separates_sessions` | 785 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_real_scan_sees_this_process` | 795 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/src/session/xserver.rs` — 18 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `display_round_trips_through_its_string_form` | 736 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `only_local_displays_are_accepted` | 748 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `backends_parse_and_report_themselves` | 758 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `cookies_are_sixteen_bytes_of_entropy` | 770 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `the_authority_file_has_the_documented_byte_layout` | 783 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `the_authority_file_is_private_from_the_first_byte` | 810 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_malformed_cookie_is_refused` | 823 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `screen_args_carry_the_refresh_rate_only_where_it_is_real` | 833 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `allocation_starts_at_one_and_honours_a_hint` | 863 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `the_readiness_wait_reports_a_dead_server_and_keeps_waiting_for_a_live_one` | 881 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_dead_server_is_not_ready_just_because_its_display_is_serving` | 916 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_live_server_on_a_serving_display_is_ready_at_once` | 941 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `only_one_creator_holds_a_display_claim` | 958 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_dead_handle_does_not_release_a_live_servers_display` | 983 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `only_the_pid_in_the_lock_marks_the_display_as_ours` | 1024 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `spawning_onto_a_claimed_display_is_refused` | 1048 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_claim_path_that_is_a_symlink_is_refused_rather_than_followed` | 1076 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_claim_on_a_free_display_is_still_granted` | 1118 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/tests/control_props.rs` — 5 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `send_command_refuses_embedded_newlines` | 39 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `send_command_bounds_the_command_length` | 56 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `send_command_refuses_traversal_before_connecting` | 73 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `dispatch_and_query_refuse_payloads_that_could_inject_a_command` | 86 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `identity_json_stays_a_single_escaped_line` | 108 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/tests/ctl_props.rs` — 5 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `the_admin_tool_answers_a_documented_exit_code` | 74 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `every_command_group_handles_its_own_help` | 101 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_listing_of_a_session_that_does_not_exist_fails` | 126 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_global_option_before_the_verb_is_not_read_as_the_verb` | 155 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_global_before_a_session_verb_reaches_the_verb` | 192 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/tests/ctl_replies.rs` — 4 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `an_error_reply_is_a_failed_command` | 118 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_json_reply_is_a_successful_command` | 125 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_silent_peer_is_a_failed_command` | 135 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_refused_socket_is_a_failed_command` | 142 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/tests/hub_props.rs` — 7 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `subscriber_cap_is_never_exceeded` | 58 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `subscriber_cap_holds_under_concurrent_registration` | 90 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `command_queue_is_bounded_and_preserves_order` | 128 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_slow_subscriber_drops_overflow_without_blocking` | 159 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `events_reach_live_subscribers_verbatim_and_dead_ones_are_pruned` | 183 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_enqueued_command_is_always_visible_to_the_next_poll` | 223 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `published_state_is_shared_by_every_clone` | 276 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/tests/identity_props.rs` — 8 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `is_valid_sid_matches_its_documented_charset` | 58 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `accepted_sids_stay_inside_the_runtime_dir` | 74 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `socket_path_spends_the_sid_exactly_once` | 94 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `rejected_sids_are_refused_before_touching_the_filesystem` | 117 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `generated_session_ids_are_always_usable` | 138 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `the_account_maverick_treats_as_its_own_is_the_real_uid` | 186 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `the_uid_a_control_socket_authorises_is_the_uid_that_owns_it` | 216 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_session_record_is_answered_by_its_owner_against_the_kernel` | 259 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/tests/json_props.rs` — 5 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `escape_unescape_roundtrip_is_identity` | 57 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `quoted_token_delimits_and_roundtrips_its_payload` | 68 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `encoded_text_never_breaks_the_string_grammar` | 85 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `unescape_normalises_adversarial_input` | 105 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `unicode_escapes_decode_by_contract` | 116 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-sys/tests/signal_install.rs` — 8 tests, crate `maverick-sys`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `a_good_install_reports_nothing_and_really_installs` | 112 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_ignored_signal_is_really_ignored` | 138 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `children_are_auto_reaped_rather_than_left_as_zombies` | 181 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_stop_signal_reaches_the_quit_flag_and_clearing_it_is_observed` | 217 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `sigint_and_sigquit_reach_the_same_quit_flag` | 240 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `sigcont_reaches_the_regrab_flag` | 254 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `a_repeat_install_is_not_mistaken_for_a_failure` | 271 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |
| `an_invalid_signal_is_reported_rather_than_swallowed` | 280 | INTEGRATION | instance identity, control socket, session record or maverickctl behaviour. | KEEP |

#### `maverick-toml/src/lib.rs` — 27 tests, crate `maverick-toml`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `parses_sections_and_simple_pairs` | 698 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `hex_is_decoded` | 707 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `booleans_and_floats` | 713 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `negative_integers` | 724 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `comments_stripped_everywhere` | 730 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `unicode_survives_in_strings_and_comments` | 738 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `strings_borrow_without_escapes` | 744 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `strings_unescape_owned` | 753 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `single_quoted_strings_are_rejected` | 762 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `flat_string_list` | 768 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `flat_int_list` | 777 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `multiline_grid_with_comments_and_trailing_comma` | 784 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `grid_with_trailing_comma_after_last_row` | 803 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `empty_array_is_ok` | 818 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `array_of_tables` | 826 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `header_allows_spaces` | 840 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `comments_only_file_is_empty` | 846 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `malformed_header_is_error` | 851 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `unterminated_string_is_error` | 859 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `malformed_number_is_error` | 865 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `mixed_type_array_is_error` | 871 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `duplicate_key_is_error` | 877 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `parser_is_fused_after_error` | 883 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `error_line_is_accurate_after_multiline_array` | 892 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `accessors_return_none_on_mismatch` | 898 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `hex_and_decimal_both_read_as_u32` | 908 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |
| `key_names_reject_quotes_and_spaces` | 915 | CORE | a TOML-subset grammar case (section, value, array, error). | KEEP |

#### `maverick-toml/tests/parser_props.rs` — 9 tests, crate `maverick-toml`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `parse_is_total_and_its_events_stay_inside_the_documented_grammar` | 180 | PROPERTY | proptest: parse is total, fused after a fault, round-trips. | KEEP |
| `the_stream_is_fused_after_a_fault_and_after_its_end` | 214 | PROPERTY | proptest: parse is total, fused after a fault, round-trips. | KEEP |
| `a_fault_is_reported_on_a_line_that_exists_in_the_source` | 237 | PROPERTY | proptest: parse is total, fused after a fault, round-trips. | KEEP |
| `a_repeated_key_collides_only_inside_one_table` | 255 | PROPERTY | proptest: parse is total, fused after a fault, round-trips. | KEEP |
| `a_string_is_borrowed_exactly_when_it_carries_no_escape` | 305 | PROPERTY | proptest: parse is total, fused after a fault, round-trips. | KEEP |
| `escaping_and_unescaping_a_literal_is_the_identity` | 335 | PROPERTY | proptest: parse is total, fused after a fault, round-trips. | KEEP |
| `a_literal_is_accepted_on_its_bound_and_rejected_one_past_it` | 355 | PROPERTY | proptest: parse is total, fused after a fault, round-trips. | KEEP |
| `a_string_literal_is_accepted_on_its_bound_and_rejected_one_past_it` | 401 | PROPERTY | proptest: parse is total, fused after a fault, round-trips. | KEEP |
| `a_canonical_document_parses_back_into_the_events_it_encodes` | 529 | PROPERTY | proptest: parse is total, fused after a fault, round-trips. | KEEP |

#### `maverick-vk/src/device.rs` — 3 tests, crate `maverick-vk`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `device_type_score_is_total_over_the_whole_type_space` | 362 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `known_device_types_form_a_strict_order_with_discrete_first` | 386 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `unknown_device_type_never_outranks_a_discrete_gpu` | 435 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |

#### `maverick-vk/src/pacing.rs` — 5 tests, crate `maverick-vk`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `a_failed_frame_never_leaves_the_fence_unwaitable` | 243 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `the_record_matches_what_the_driver_did` | 268 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `a_fresh_fence_owes_nothing` | 287 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `a_submitted_frame_owes_exactly_one_completion` | 297 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `a_frame_that_fails_after_the_previous_submit_leaves_a_waitable_fence` | 316 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |

#### `maverick-vk/tests/properties.rs` — 14 tests, crate `maverick-vk`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `surface_format_is_unavailable_only_when_the_surface_lists_nothing` | 392 | PROPERTY | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `surface_format_follows_the_documented_preference_order` | 411 | PROPERTY | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `present_mode_never_names_a_mode_outside_mailbox_and_fifo` | 452 | PROPERTY | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `present_mode_prefers_mailbox_exactly_when_the_driver_offers_it` | 470 | PROPERTY | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `surface_owned_extent_overrides_any_request` | 488 | PROPERTY | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `a_sentinel_on_one_axis_only_is_not_the_you_choose_sentinel` | 511 | PROPERTY | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `extent_is_total_and_lands_in_the_advertised_window` | 531 | PROPERTY | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `extent_is_clamped_to_the_surface_limits` | 572 | PROPERTY | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `extent_clamp_is_monotone_in_the_request` | 612 | PROPERTY | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `image_count_stays_inside_what_the_driver_allows` | 640 | PROPERTY | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `selection_helpers_are_pure_functions_of_their_input` | 673 | PROPERTY | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `every_result_code_maps_to_an_error_that_names_it` | 695 | PROPERTY | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `each_error_variant_is_identifiable_from_its_message` | 717 | PROPERTY | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `device_report_shows_every_field_it_was_built_from` | 768 | PROPERTY | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |

#### `maverick-vk/tests/smoke.rs` — 2 tests, crate `maverick-vk`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `smoke_init_and_present` | 36 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `drop_after_present_is_validation_clean` | 100 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |

#### `maverick-vk/tests/unit.rs` — 12 tests, crate `maverick-vk`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `format_prefers_bgra8_srgb` | 13 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. *(impl-detail)* | REMOVE |
| `format_undefined_single_allows_any` | 25 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `format_falls_back_to_first` | 36 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `format_empty_is_none_not_panic` | 48 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `present_mode_prefers_mailbox_then_fifo` | 53 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. *(impl-detail)* | REMOVE |
| `extent_clamps_within_bounds` | 71 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. *(impl-detail)* | REMOVE |
| `extent_uses_current_when_fixed` | 93 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `extent_uses_current_when_only_one_axis_is_real` | 117 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `extent_survives_an_inverted_window` | 147 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `image_count_plus_one_capped` | 172 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. *(impl-detail)* | REMOVE |
| `image_count_does_not_overflow_at_the_top_of_the_range` | 190 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. | REMOVE |
| `vk_error_from_vk_result_is_descriptive` | 206 | COMPOSITOR | Vulkan capability/extent/present-mode selection or fence pacing. *(impl-detail)* | REMOVE |

#### `maverick-x11/src/lib.rs` — 4 tests, crate `maverick-x11`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `every_error_byte_has_a_name` | 485 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. | KEEP |
| `each_core_error_code_has_a_name_of_its_own` | 497 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. | KEEP |
| `taking_an_x_error_never_invents_one` | 519 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. | KEEP |
| `a_stale_error_is_discarded_and_a_taken_one_reported_once` | 537 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. | KEEP |

#### `maverick-x11/tests/error_handler_scope.rs` — 4 tests, crate `maverick-x11`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `installing_the_silent_handler_replaces_the_slot_and_does_not_wrap_it` | 109 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. | KEEP |
| `installing_the_silent_handler_twice_leaves_the_same_handler_in_the_slot` | 144 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. | KEEP |
| `open_x_replaces_a_handler_that_was_already_installed` | 163 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. | KEEP |
| `the_recorded_code_is_the_protocol_error_code_byte` | 204 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. | KEEP |

#### `maverick-x11/tests/io_error_scope.rs` — 1 tests, crate `maverick-x11`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `a_synchronous_failing_request_ends_the_connection_rather_than_reporting_it` | 125 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. | KEEP |

#### `maverick-x11/tests/x_error_signal.rs` — 8 tests, crate `maverick-x11`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `an_asynchronous_failure_never_reaches_the_error_handler` | 133 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. | KEEP |
| `a_successful_request_reports_no_error` | 163 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. | KEEP |
| `a_recorded_error_code_is_a_protocol_byte` | 197 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. | KEEP |
| `the_error_cell_is_per_thread` | 213 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. | KEEP |
| `xlib_thread_support_is_available` | 242 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. | KEEP |
| `the_display_handle_is_send` | 261 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. *(impl-detail)* | KEEP |
| `every_alias_of_the_display_is_non_owning` | 275 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. *(impl-detail)* | KEEP |
| `a_missing_display_is_named_in_the_failure` | 324 | X11 | X error-cell / error-handler-slot behaviour on the shared bootstrap. | KEEP |

#### `src/backend/x11/compositor.rs` — 4 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `zero_and_negative_yields_empty` | 241 | COMPOSITOR | substep integrator tick bounds (stub build). | REMOVE |
| `small_dt_single_step` | 249 | COMPOSITOR | substep integrator tick bounds (stub build). | REMOVE |
| `multi_step_invariants` | 258 | COMPOSITOR | substep integrator tick bounds (stub build). | REMOVE |
| `tick_consumes_substeps` | 273 | COMPOSITOR | substep integrator tick bounds (stub build). | REMOVE |

#### `src/backend/x11/compositor_gl.rs` — 74 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `viewport_cull_matches_edges` | 4222 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `raising_b_draws_b_above_a` | 4264 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `above_none_means_bottom_not_unknown` | 4271 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `restack_is_idempotent` | 4281 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `moving_a_window_up_accounts_for_its_own_removal` | 4294 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `moving_a_window_down_keeps_the_sibling_relation` | 4308 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `an_untracked_window_with_a_known_sibling_is_inserted_exactly` | 4315 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `an_unknown_sibling_demands_a_resync_instead_of_a_guess` | 4322 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `never_duplicates_an_entry` | 4335 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `create_goes_on_top_and_destroy_forgets` | 4351 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `moving_window_damages_old_and_new` | 4381 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `occluded_moving_window_still_damages_old_and_new` | 4395 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `fractional_damage_uses_outward_enclosing_bounds` | 4408 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `fractional_map_damage_expands_without_truncation` | 4434 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `stationary_window_damages_only_current` | 4461 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `freshly_visible_window_damages_only_current` | 4472 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `scrolling_pair_union_spans_old_and_new` | 4484 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `fully_covered_by_single_occluder_only` | 4506 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `fresh_region_is_empty` | 4520 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `adding_a_rect_makes_it_non_empty` | 4526 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `zero_size_rects_are_ignored` | 4534 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `overlapping_rects_are_merged_without_losing_disjoint_regions` | 4542 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `full_short_circuits_the_region` | 4552 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `overflow_falls_back_to_full` | 4563 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `bounding_rect_spans_all_rects` | 4575 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `damage_bypassed_still_subtracts` | 4593 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `damage_normal_subtracts_and_marks_dirty` | 4600 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `bypass_damage_sequence_always_subtracts` | 4607 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `empty_equals_screen` | 4660 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `one_bypass_punches_hole` | 4668 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `two_bypasses_non_overlapping` | 4681 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `overlapping_holes_handled` | 4695 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `updated_rect_recalculates` | 4705 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `remove_bypass_restores` | 4719 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `subtract_rect_no_overlap` | 4730 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `state_machine_engage_resize_destroy` | 4737 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `case_a_single_output` | 4765 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `case_b_two_positive_outputs` | 4774 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `case_c_negative_x_output` | 4783 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `case_d_negative_y_output` | 4797 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `global_to_local_identity_and_negative` | 4810 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `nothing_damaged_is_idle` | 4831 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `no_buffer_age_forces_full` | 4837 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `structural_change_forces_full` | 4844 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `buffer_age_plus_damage_is_partial` | 4850 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `age_two_replays_one_previous_region` | 4855 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `age_too_old_for_journal_forces_full` | 4866 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `full_history_marker_stays_full_for_multi_buffer_age` | 4874 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `camera_motion_never_reuses_a_translated_integer_cache` | 4929 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `visual_x_projection_preserves_fraction_and_is_not_accumulated` | 4944 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `visual_rect_encloses_fractional_bounds_outwards` | 4969 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `renderer_quad_keeps_fractional_dst` | 4981 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `projection_is_allocation_free_and_within_frame_budget` | 5047 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `damage_region_and_plan_is_allocation_free_and_cheap` | 5080 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `occlusion_pass_is_cheap_and_allocation_free` | 5116 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `visual_transform_preserves_fraction_until_draw` | 5173 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `a8_fullscreen_presentation_has_intermediate_frame_and_exact_endpoint` | 5211 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `presentation_retargets_from_unrounded_value_without_return_drift` | 5234 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `ribbon_radius_fades_with_motion_and_target_does_not_restart_each_pixel` | 5254 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `finished_progress_waits_for_installed_camera_endpoint` | 5281 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `navigation_away_from_fullscreen_glides_back_to_the_ribbon` | 5320 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `navigation_into_fullscreen_starts_from_the_presented_geometry` | 5364 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `disabled_animation_and_unmapped_first_placement_are_immediate` | 5408 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `observe_configure_is_pure_and_reports_resize_only` | 5432 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `observe_configure_expands_by_border` | 5473 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `set_transform_keeps_outer_origin_without_bw_shift` | 5500 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `set_transform_squares_only_full_screen_coverage` | 5546 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `set_transform_squares_fullscreen_on_each_monitor` | 5581 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `rounded_radius_policy_covers_union_monitors_and_clamp` | 5622 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `border_color_updates_feed_stroke_state` | 5662 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `zero_and_negative_yields_empty` | 5708 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `small_dt_single_step` | 5720 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `multi_step_invariants` | 5729 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |
| `tick_consumes_substeps` | 5744 | COMPOSITOR | damage region, stack diff, buffer-age frame plan, projection. | REMOVE |

#### `src/backend/x11/ewmh.rs` — 3 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `the_leftover_order_is_stable_across_insertion_orders` | 350 | X11 | _NET_CLIENT_LIST_STACKING leftover pruning determinism. | KEEP |
| `an_empty_client_map_yields_no_leftovers` | 368 | X11 | _NET_CLIENT_LIST_STACKING leftover pruning determinism. | KEEP |
| `a_single_client_is_its_own_only_leftover` | 373 | X11 | _NET_CLIENT_LIST_STACKING leftover pruning determinism. | KEEP |

#### `src/backend/x11/framesched.rs` — 24 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `a_frame_is_requested_exactly_when_the_turn_has_work` | 321 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `reasons_coalesce_into_one_pending_frame` | 370 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `the_animation_bit_tracks_the_last_turn_and_never_latches` | 426 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `the_clamped_dt_is_always_a_usable_spring_step` | 456 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `transition_started_during_frame_does_not_idle_before_next_frame` | 491 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `finishing_presentation_preserves_wallpaper_animation` | 502 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `empty_scheduler_needs_no_frame` | 511 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `animation_alone_needs_a_frame` | 521 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `idle_scheduler_parks_without_a_heartbeat_timeout` | 530 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `from_compositor_maps_reasons` | 538 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `reasons_iter_reports_only_pending` | 551 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `coalesces_multiple_requests_into_one_pending` | 562 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `dirty_without_animation_renders_once_then_idles` | 582 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `animation_keeps_requesting_frames` | 602 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `ending_animation_returns_to_idle` | 622 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `wallpaper_animation_alone_needs_a_frame` | 639 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `clear_dirty_preserves_wallpaper_animation` | 648 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `stopping_wallpaper_shader_returns_to_idle` | 665 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `static_wallpaper_shader_does_not_request_frames` | 683 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `animated_wallpaper_shader_requests_frames` | 696 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `vsync_on_relies_on_swap_instead_of_a_second_timer` | 714 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `endpoint_transition_gets_one_terminal_frame` | 720 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `idle_to_animating_produces_no_absurd_dt` | 730 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE |
| `springs_need_a_whole_frame_of_dt_not_the_loop_overhead` | 752 | COMPOSITOR | when a frame is requested and coalesced. | REMOVE(R) |

#### `src/backend/x11/mod.rs` — 3 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `reasons_read_in_scheduler_order` | 2289 | LEGACY | the human-readable FrameReason phrase. *(impl-detail)* | REMOVE |
| `a_single_reason_has_no_separator` | 2298 | LEGACY | the human-readable FrameReason phrase. *(impl-detail)* | REMOVE |
| `no_reasons_read_as_nothing` | 2307 | LEGACY | the human-readable FrameReason phrase. *(impl-detail)* | REMOVE |

#### `src/backend/x11/reconciler.rs` — 38 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `first_apply_always_emits` | 322 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `changed_rect_emits_only_the_delta` | 335 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `geometry_dirty_forces_emit_on_identical_rect` | 353 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `forget_clears_applied` | 369 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `matching_echo_is_compliant` | 391 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `diverging_echo_is_stale_not_client_intent` | 413 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `stale_echo_causes_no_configure_storm` | 438 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP(R) |
| `observe_records_real_geometry_without_emitting` | 469 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `ab_independent` | 486 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `fullscreen_echo_is_not_special` | 512 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `desired_equals_applied_produces_no_effect` | 537 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `desired_differs_from_applied_emits_configure` | 572 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `desired_same_rect_force_reapply_emits_when_required` | 624 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `multiple_windows_diff_independent` | 670 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `destroy_window_removes_desired_and_applied_cleanly` | 728 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `tiled_zero_size_configure_request_is_rejected` | 780 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `tiled_huge_configure_request_is_rejected` | 785 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `tiled_off_monitor_configure_request_is_rejected` | 790 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `float_invalid_configure_request_is_followed_then_clamped` | 805 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `drag_authority_table` | 839 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `float_dragged_does_not_follow` | 876 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `drag_ends_restores_float_authority` | 899 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `clamp_zero_size_never_reaches_x11` | 933 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `clamp_huge_size_fits_workarea` | 948 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `clamp_negative_position_stays_inside_workarea` | 960 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `clamp_overflow_offscreen_bottom_right_stays_inside` | 969 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `reconcile_writes_exactly_the_pending_configures` | 1202 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `applied_records_the_geometry_the_wire_can_carry` | 1274 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `a_clamped_geometry_still_converges_in_one_request` | 1327 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `a_duplicate_desired_entry_still_converges` | 1377 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `a_stale_echo_is_repaired_and_the_repair_is_bounded` | 1447 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `a_repeated_reconcile_emits_nothing` | 1496 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `only_a_dirty_client_is_re_poked` | 1520 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `reconcile_never_touches_the_logical_state` | 1550 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `reconciliation_converges_on_the_latest_desired_state` | 1570 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `a_forgotten_window_is_re_emitted` | 1628 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `the_configure_verdict_depends_only_on_geometry_equality` | 1664 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |
| `a_clamped_float_is_always_x11_valid` | 1716 | X11 | desired-vs-applied diff, ConfigureRequest authority, convergence. | KEEP |

#### `src/backend/x11/render.rs` — 57 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `float_inside_workarea_is_untouched` | 1645 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `rounded_mask_spans_exactly_the_outer_frame` | 1672 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `rounded_mask_is_horizontally_symmetric` | 1680 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `rounded_mask_radius_is_bounded_by_half_the_smaller_side` | 1701 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `zero_radius_is_a_square_full_frame_mask` | 1723 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `degenerate_mask_sizes_stay_valid` | 1733 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `focus_ring_band_is_thin_and_traces_the_frame_curve` | 1776 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `focus_ring_inner_radius_follows_outer_minus_border` | 1886 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `focus_ring_vanishes_when_radius_reaches_the_border` | 1908 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `fullscreen_geometry_yields_square_masks_both_kinds` | 1926 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `float_past_the_edges_is_pulled_back` | 1949 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `float_larger_than_workarea_is_resized_and_stays_inside` | 1965 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `zero_sized_workarea_never_produces_a_zero_dimension` | 1982 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `float_zero_size_request_is_clamped_to_minimum` | 1997 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `float_partially_offscreen_is_pulled_back` | 2014 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `float_normal_resize_inside_is_honored` | 2028 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `float_huge_request_is_shrunk_to_workarea` | 2038 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `transient_chain_depth1_reaches_the_root` | 2072 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `transient_chain_depth2_reaches_the_root` | 2086 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `transient_chain_depth4_at_the_limit_reaches_the_root` | 2100 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `transient_chain_depth5_beyond_the_limit_is_fail_safe` | 2114 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `transient_chain_cycle_terminates` | 2136 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `transient_chain_stops_at_a_destroyed_parent` | 2158 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `clamp_is_idempotent_and_stable` | 2176 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `clamp_matches_layout_float_path` | 2198 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `covering_fullscreen_and_overlay_owner_are_distinct` | 2233 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `float_answer_is_toolkit_fixed_point` | 2478 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `clamp_only_pipeline_bounces_but_normalized_does_not` | 2499 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `fixed_size_dialog_request_is_pinned` | 2519 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `snap_is_idempotent` | 2540 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `normalize_is_idempotent_when_hints_fit_workarea` | 2557 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `normalize_never_degenerate_never_escapes` | 2575 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `snap_enforces_bounds_and_grid` | 2592 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `parse_wm_normal_hints_wire_format` | 2618 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `workarea_change_releases_the_client_authority_seal` | 2687 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `workarea_change_releases_the_seal_of_an_unmoved_float` | 2715 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `workarea_change_is_silent_for_an_unsealed_settled_float` | 2733 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `prop_rounded_mask_stays_inside_the_frame` | 2972 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE(R) |
| `prop_rounded_frame_mask_never_wraps_the_card16_boundary` | 3029 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE(R) |
| `the_masked_frame_never_reaches_past_the_signed_extent` | 3080 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `a_mask_request_can_never_exceed_the_servers_maximum_length` | 3115 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `an_unrepresentable_border_saturates_the_frame_instead_of_trapping` | 3152 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `the_frame_clamp_and_the_mask_clamp_are_both_reached` | 3186 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `a_forty_thousand_pixel_border_does_not_wrap_the_mask` | 3208 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `a_frame_wider_than_i32_max_does_not_panic_the_radius_clamp` | 3233 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `prop_rounded_mask_is_symmetric_about_both_sides` | 3248 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE(R) |
| `prop_client_clip_measures_exactly_the_inset_frame` | 3287 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE(R) |
| `prop_parked_rect_is_off_screen_and_stable` | 3341 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP(R) |
| `the_server_never_stores_a_parked_window_on_screen` | 3379 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `a_parked_window_wider_than_int16_cannot_come_back_on_screen` | 3403 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP |
| `prop_transient_ownership_matches_bounded_reachability` | 3434 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP(R) |
| `prop_reclaim_re_decides_exactly_when_it_must` | 3463 | X11 | float normalisation, transient ownership, client authority seal, Shape masks. | KEEP(R) |
| `prop_render_list_is_total_and_names_only_real_windows` | 3595 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE(R) |
| `prop_render_list_is_deterministic` | 3664 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE(R) |
| `prop_applied_geometry_is_what_the_next_cycle_wants` | 3707 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE(R) |
| `every_rounded_mask_row_stays_symmetric_at_the_radius_boundary` | 3790 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |
| `tangent_row_is_covered_when_the_radius_is_half_the_width` | 3819 | COMPOSITOR | float normalisation, transient ownership, client authority seal, Shape masks. | REMOVE |

#### `src/backend/x11/struts.rs` — 4 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `hostile_struts_never_escape_the_monitor` | 371 | X11 | _NET_WM_STRUT[_PARTIAL] -> workarea reservation. | KEEP |
| `every_nonzero_strut_edge_reaches_the_reservation` | 392 | X11 | _NET_WM_STRUT[_PARTIAL] -> workarea reservation. | KEEP |
| `a_docks_reservation_is_replaced_and_fully_released` | 444 | X11 | _NET_WM_STRUT[_PARTIAL] -> workarea reservation. | KEEP |
| `accumulating_docks_never_grow_the_workarea` | 519 | X11 | _NET_WM_STRUT[_PARTIAL] -> workarea reservation. | KEEP |

#### `src/backend/x11/teardown.rs` — 5 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `the_local_half_removes_the_record_and_the_socket` | 239 | X11 | shutdown halves: local record removal vs the X half. | KEEP |
| `a_session_with_no_id_still_completes` | 274 | X11 | shutdown halves: local record removal vs the X half. | KEEP |
| `the_x_half_runs_only_for_a_clean_exit_over_a_live_connection` | 290 | X11 | shutdown halves: local record removal vs the X half. | KEEP |
| `a_working_connection_can_be_borrowed` | 324 | X11 | shutdown halves: local record removal vs the X half. | KEEP |
| `a_reason_may_skip_the_x_half_but_never_the_local_one` | 344 | X11 | shutdown halves: local record removal vs the X half. | KEEP |

#### `src/backend/x11/tests.rs` — 27 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `resolver_uses_all_xkb_levels_and_groups` | 223 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `resolver_handles_level_five_without_clamping` | 253 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `a_level_missing_from_the_row_width_resolves_to_no_key` | 266 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `planner_dispatch_inverse_handles_multiple_configurable_selectors` | 287 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `planner_rejects_a_final_grab_state_that_falls_outside_the_xkb_rules` | 330 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `shift_and_capslock_keep_existing_letter_binding_semantics` | 347 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `level_three_planner_and_dispatch_share_the_same_resolution` | 404 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `keypad_numlock_selects_the_planned_and_dispatched_level` | 496 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `shift_binding_keeps_the_named_level_zero_keysym_compatibility` | 547 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `lock_variants_dispatch_even_when_the_type_does_not_select_lock` | 583 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `planner_and_dispatch_follow_the_active_group_for_dvorak` | 601 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `group_redirect_policy_is_applied_before_lookup` | 637 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `core_fallback_preserves_level_zero_and_shift` | 649 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `core_fallback_plans_every_keymap_row` | 694 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `num_and_scroll_lock_columns_use_full_protocol_keysyms` | 708 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `missing_keysym_is_reported_without_a_grab` | 729 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `action_keys_are_normalized_for_dispatch` | 750 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `clean_mask_strips_groups_and_configured_locks` | 770 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `core_dispatch_column_never_reads_past_level_one` | 785 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `bind_names_round_trip_into_config_syntax` | 800 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `keypress_state_separates_group_from_core_modifiers` | 810 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `an_idle_loop_with_no_deadline_blocks_indefinitely` | 851 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `a_pending_shutdown_budget_bounds_an_otherwise_unbounded_wait` | 860 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `a_pending_keyboard_refresh_bounds_an_otherwise_unbounded_wait` | 873 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `a_due_frame_is_not_postponed_by_a_later_deadline` | 882 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `an_elapsed_budget_yields_no_wait_at_all` | 905 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |
| `the_earlier_of_the_two_deadlines_wins` | 918 | X11 | xkb planner/dispatch resolution, or event-loop wait budget. | KEEP |

#### `src/backend/x11/trace.rs` — 11 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `records_bounded_and_counted_when_full` | 472 | LEGACY | bounded ring buffer of diagnostic trace records. | REMOVE |
| `frames_and_turns_are_monotonic_with_intervals` | 487 | LEGACY | bounded ring buffer of diagnostic trace records. | REMOVE |
| `payload_overflow_truncates_and_counts` | 513 | LEGACY | bounded ring buffer of diagnostic trace records. | REMOVE |
| `the_header_says_how_the_session_ended` | 563 | LEGACY | bounded ring buffer of diagnostic trace records. | REMOVE |
| `a_dump_that_cannot_write_reports_where_and_why` | 587 | LEGACY | bounded ring buffer of diagnostic trace records. | REMOVE |
| `a_dump_with_tracing_off_reports_nothing_and_no_error` | 606 | LEGACY | bounded ring buffer of diagnostic trace records. | REMOVE |
| `overflow_keeps_the_head_and_counts_everything_else` | 715 | LEGACY | bounded ring buffer of diagnostic trace records. | REMOVE |
| `an_over_long_payload_is_cut_at_a_character_boundary` | 741 | LEGACY | bounded ring buffer of diagnostic trace records. | REMOVE |
| `the_drop_and_truncation_counters_never_double_count` | 761 | LEGACY | bounded ring buffer of diagnostic trace records. | REMOVE |
| `counters_and_their_marker_records_cannot_desynchronise` | 792 | LEGACY | bounded ring buffer of diagnostic trace records. | REMOVE |
| `only_the_first_frame_reports_a_missing_interval` | 851 | LEGACY | bounded ring buffer of diagnostic trace records. | REMOVE |

#### `src/compositor_policy.rs` — 24 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `disabled_is_disabled_regardless_of_state` | 251 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `enabled_normal_is_compose` | 263 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `enabled_fullscreen_bypass_true_is_bypass` | 272 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `enabled_fullscreen_bypass_false_is_compose` | 283 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `fullscreen_with_floating_overlay_is_compose` | 296 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `fullscreen_with_visible_dialog_is_compose` | 314 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `maximized_non_fullscreen_is_compose` | 326 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `multiple_visible_windows_is_compose` | 346 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `transient_popup_forbids_bypass` | 356 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `hidden_fullscreen_is_not_a_candidate` | 370 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `per_monitor_independence` | 382 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `bypass_is_never_chosen_when_something_would_be_left_uncovered` | 726 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `a_disabled_compositor_is_never_composed` | 766 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `the_bypass_gate_is_never_overridden_by_a_scene` | 780 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `an_unknown_output_never_bypasses` | 794 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `a_second_compositable_client_forbids_bypass` | 807 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `a_skipped_neighbour_still_allows_bypass` | 830 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `a_covering_window_that_does_not_span_the_output_is_not_a_candidate` | 853 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `a_hidden_candidate_is_not_a_candidate` | 867 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `only_the_exact_compositor_demand_vetoes_bypass` | 882 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `a_maximized_window_never_bypasses` | 908 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `two_covering_fullscreen_windows_never_bypass` | 928 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `a_lone_true_policy_overlay_still_bypasses` | 942 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |
| `a_neighbouring_output_cannot_change_this_ones_mode` | 957 | COMPOSITOR | Compose-vs-Bypass decision per output and per scene. | REMOVE |

#### `src/config.rs` — 9 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `no_criteria_matches_anything` | 640 | CORE | window-rule criteria matching and compiled default keybindings. | KEEP |
| `class_match_is_substring_case_insensitive` | 646 | CORE | window-rule criteria matching and compiled default keybindings. | KEEP |
| `instance_match_uses_wm_class_instance_part` | 653 | CORE | window-rule criteria matching and compiled default keybindings. | KEEP |
| `window_type_match_is_exact_and_lowercase` | 660 | CORE | window-rule criteria matching and compiled default keybindings. | KEEP |
| `title_match_is_substring_case_insensitive` | 670 | CORE | window-rule criteria matching and compiled default keybindings. | KEEP |
| `all_criteria_must_hold_together` | 677 | CORE | window-rule criteria matching and compiled default keybindings. | KEEP |
| `compiled_config_normalizes_initial_state_for_all_clients` | 689 | CORE | window-rule criteria matching and compiled default keybindings. | KEEP |
| `compiled_config_binds_mod4_shift_q_to_quit` | 708 | CORE | window-rule criteria matching and compiled default keybindings. | KEEP |
| `compiled_config_binds_mod4_shift_r_to_restart` | 733 | CORE | window-rule criteria matching and compiled default keybindings. | KEEP |

#### `src/core/action.rs` — 13 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `every_canonical_name_parses` | 372 | CORE | Action parse / round-trip / alias agreement. | KEEP |
| `round_trip_arg_free_variants` | 396 | CORE | Action parse / round-trip / alias agreement. | KEEP |
| `window_ids_parse_in_every_spelling_a_client_prints` | 423 | CORE | Action parse / round-trip / alias agreement. | KEEP |
| `a_zero_window_id_is_refused` | 446 | CORE | Action parse / round-trip / alias agreement. | KEEP |
| `window_targeted_actions_round_trip` | 453 | CORE | Action parse / round-trip / alias agreement. | KEEP |
| `window_targeted_aliases_name_the_same_action` | 476 | CORE | Action parse / round-trip / alias agreement. | KEEP |
| `percentages_parse_with_or_without_their_decoration` | 489 | CORE | Action parse / round-trip / alias agreement. | KEEP |
| `move_window_needs_a_direction_and_an_id` | 512 | CORE | Action parse / round-trip / alias agreement. | KEEP |
| `window_targeted_verbs_are_not_keymap_actions` | 531 | CORE | Action parse / round-trip / alias agreement. | KEEP |
| `legacy_ipc_aliases_still_parse` | 550 | CORE | Action parse / round-trip / alias agreement. | KEEP |
| `toml_and_ipc_yeargent_same_results` | 579 | CORE | Action parse / round-trip / alias agreement. | KEEP |
| `rejects_unknown_and_malformed` | 591 | CORE | Action parse / round-trip / alias agreement. | KEEP |
| `wallpaper_subverbs_parse` | 601 | CORE | Action parse / round-trip / alias agreement. | KEEP |

#### `src/core/commands.rs` — 3 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `set_wallpaper_mutates_state_and_bumps_rev` | 2133 | CORE | SetWallpaper command mutates state and bumps the revision. | KEEP |
| `wallpaper_clear_and_mode` | 2152 | CORE | SetWallpaper command mutates state and bumps the revision. | KEEP |
| `wallpaper_noop_does_not_bump_rev` | 2170 | CORE | SetWallpaper command mutates state and bumps the revision. | KEEP |

#### `src/core/framebench.rs` — 3 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `the_counter_is_not_vacuously_zero` | 98 | COMPOSITOR | the per-frame allocation counter. *(impl-detail)* | REMOVE |
| `counting_is_off_by_default` | 108 | COMPOSITOR | the per-frame allocation counter. *(impl-detail)* | REMOVE |
| `an_animation_frame_allocates_nothing` | 202 | COMPOSITOR | the per-frame allocation counter. | REMOVE |

#### `src/core/invariants.rs` — 24 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `h_l_focus_keeps_settled_geometry` | 285 | CORE | settled-geometry invariant after one command. | KEEP |
| `mouse_focus_centers_focused_column` | 311 | CORE | settled-geometry invariant after one command. | KEEP |
| `fullscreen_then_neighbor_settled_geometry` | 326 | CORE | settled-geometry invariant after one command. | KEEP |
| `toggle_maximize_focused_only` | 382 | CORE | settled-geometry invariant after one command. | KEEP |
| `presented_maximize_tracks_focus_and_is_cleared_on_lifecycle` | 456 | CORE | settled-geometry invariant after one command. | KEEP |
| `move_window_keeps_invariant` | 522 | CORE | settled-geometry invariant after one command. | KEEP |
| `page_snap_does_not_break_invariant` | 536 | CORE | settled-geometry invariant after one command. | KEEP |
| `overview_returns_to_settled` | 555 | CORE | settled-geometry invariant after one command. | KEEP |
| `viewport_zoom_returns_to_settled` | 571 | CORE | settled-geometry invariant after one command. | KEEP |
| `workspace_switch_resettles` | 587 | CORE | settled-geometry invariant after one command. | KEEP |
| `dock_strut_retarget_respects_workarea` | 620 | CORE | settled-geometry invariant after one command. | KEEP |
| `settled_follows_target_at_rest` | 659 | CORE | settled-geometry invariant after one command. | KEEP |
| `live_differs_mid_animation_then_converges` | 692 | CORE | settled-geometry invariant after one command. | KEEP |
| `repeated_abc_navigation_idempotent` | 785 | CORE | settled-geometry invariant after one command. | KEEP |
| `focus_none_is_safe` | 807 | CORE | settled-geometry invariant after one command. | KEEP |
| `border_w_is_part_of_geom` | 851 | CORE | settled-geometry invariant after one command. | KEEP |
| `mouse_and_keyboard_focus_converge` | 898 | CORE | settled-geometry invariant after one command. | KEEP |
| `input_hittest_matches_settled_geom` | 960 | CORE | settled-geometry invariant after one command. | KEEP |
| `close_window_before_focus_realigns_pointer_and_geometry` | 1053 | CORE | settled-geometry invariant after one command. | KEEP |
| `close_focused_window_repoints_focus_to_neighbour` | 1098 | CORE | settled-geometry invariant after one command. | KEEP |
| `close_window_after_focus_keeps_focus_column` | 1139 | CORE | settled-geometry invariant after one command. | KEEP |
| `close_row_before_focus_shifts_focused_row` | 1166 | CORE | settled-geometry invariant after one command. | KEEP |
| `layout_switch_with_displaced_camera_recenters_focused_column` | 1201 | CORE | settled-geometry invariant after one command. | KEEP |
| `closing_any_window_keeps_focused_window_centered` | 1253 | CORE | settled-geometry invariant after one command. | KEEP |

#### `src/core/ipc.rs` — 9 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `the_inspect_document_reports_totals_layout_and_compositor` | 515 | COMPOSITOR | IPC document shape and action parsing. | REWRITE |
| `the_inspect_document_survives_an_empty_state` | 574 | CORE | IPC document shape and action parsing. | KEEP |
| `default_backend_facts_do_not_claim_a_compositor` | 592 | COMPOSITOR | IPC document shape and action parsing. | REWRITE |
| `the_state_snapshot_carries_each_monitors_screen_size` | 610 | CORE | IPC document shape and action parsing. | KEEP |
| `parses_directional_actions` | 628 | CORE | IPC document shape and action parsing. | KEEP |
| `parses_layout_and_ws` | 640 | CORE | IPC document shape and action parsing. | KEEP |
| `parses_grow_shrink` | 651 | CORE | IPC document shape and action parsing. | KEEP |
| `parses_spawn_with_args` | 663 | CORE | IPC document shape and action parsing. | KEEP |
| `rejects_unknown` | 673 | CORE | IPC document shape and action parsing. | KEEP |

#### `src/core/layout.rs` — 38 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `fullscreen_column_fills_screen_when_centered` | 1198 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `fullscreen_column_aligns_to_screen_edge_with_asymmetric_struts` | 1213 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `fullscreen_column_scrolls_away` | 1228 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `fullscreen_hides_column_siblings` | 1313 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `ribbon_invariants_hold_with_fullscreen` | 1358 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `giant_gaps_tiny_workarea_stay_valid` | 1476 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `many_rows_and_columns_with_extreme_gaps_never_produce_invalid_geometry` | 1490 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `one_by_one_workarea_two_clients_huge_gap_stay_valid` | 1577 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `hundred_square_workarea_hundred_clients_huge_gap_stay_valid` | 1591 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `full_hd_three_clients_huge_gap_stay_valid` | 1604 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `gap_sweep_zero_normal_ceiling_and_beyond_i32_stay_valid` | 1618 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `border_w_sweep_extreme_config_stays_valid` | 1645 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `realistic_border_w_is_passed_through_unchanged` | 1703 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `arrange_is_idempotent_over_a_reused_buffer` | 2060 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `a_stale_monitor_index_clears_the_buffer_instead_of_panicking` | 2101 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `each_phase_reads_only_its_own_camera_field` | 2134 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `no_arranged_rect_is_degenerate` | 2164 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `the_outer_gap_never_pushes_the_inset_off_the_workarea` | 2191 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `every_accepted_border_width_preserves_the_geometry_contract` | 2222 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `tiles_stay_within_the_gap_inset_workarea_on_the_vertical_axis` | 2276 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `smart_gaps_hand_a_lone_window_the_whole_gap_inset_workarea` | 2317 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `adding_a_column_leaves_the_existing_columns_untouched` | 2389 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `every_column_gets_a_bounded_positive_share_of_the_workarea` | 2446 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `arrange_never_places_an_unmanaged_window` | 2489 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `a_workarea_change_leaves_the_focused_column_on_the_screen` | 2532 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `the_camera_target_keeps_the_focused_column_visible` | 2632 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `the_hit_test_extents_agree_with_the_drawn_placement` | 2700 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `adopting_a_client_float_is_the_identity_for_representable_rects` | 2757 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `the_wm_float_normalization_stays_inside_the_workarea` | 2775 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `the_wm_float_normalization_is_a_fixed_point` | 2827 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `the_clamp_and_the_min_hint_agree_on_one_size` | 2858 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `the_protocol_floor_lands_on_the_client_grid` | 2901 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `a_sealed_float_is_projected_back_verbatim` | 2930 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `a_float_lands_in_its_own_monitor_workarea_not_a_neighbours` | 3049 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `a_float_inside_its_workarea_keeps_its_position` | 3082 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `a_neighbouring_monitor_cannot_move_another_monitors_float` | 3106 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `moving_a_float_between_monitors_resettles_the_record` | 3136 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |
| `every_float_lands_inside_its_own_monitor_workarea` | 3191 | CORE | arrange() geometry contract (no degenerate rect, gaps, borders, floats). | KEEP |

#### `src/core/present.rs` — 15 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `focused_fullscreen_covers_screen` | 148 | CORE | present_into() overlay rewrite for fullscreen/maximize. | KEEP |
| `fullscreen_persists_while_unfocused` | 182 | CORE | present_into() overlay rewrite for fullscreen/maximize. | KEEP |
| `focused_maximized_fills_workarea` | 220 | CORE | present_into() overlay rewrite for fullscreen/maximize. | KEEP |
| `unfocused_maximized_returns_to_tile_slot` | 260 | CORE | present_into() overlay rewrite for fullscreen/maximize. | KEEP |
| `fullscreen_beats_maximized` | 303 | CORE | present_into() overlay rewrite for fullscreen/maximize. | KEEP |
| `maximize_vertical_only_stretches_y` | 372 | CORE | present_into() overlay rewrite for fullscreen/maximize. | KEEP |
| `maximize_horizontal_only_stretches_x` | 389 | CORE | present_into() overlay rewrite for fullscreen/maximize. | KEEP |
| `maximize_both_axes_fills_the_workarea` | 404 | CORE | present_into() overlay rewrite for fullscreen/maximize. | KEEP |
| `no_fullscreen_is_noop` | 413 | CORE | present_into() overlay rewrite for fullscreen/maximize. | KEEP |
| `presenting_preserves_the_placement_set_and_orders_the_raise_list` | 743 | CORE | present_into() overlay rewrite for fullscreen/maximize. | KEEP |
| `presenting_an_already_presented_placement_changes_nothing` | 818 | CORE | present_into() overlay rewrite for fullscreen/maximize. | KEEP |
| `a_maximize_presentation_stretches_only_the_axis_it_owns` | 849 | CORE | present_into() overlay rewrite for fullscreen/maximize. | KEEP |
| `fullscreen_wins_over_maximized_and_covers_the_screen` | 921 | CORE | present_into() overlay rewrite for fullscreen/maximize. | KEEP |
| `only_the_presented_maximize_becomes_an_overlay` | 963 | CORE | present_into() overlay rewrite for fullscreen/maximize. | KEEP |
| `a_column_fullscreen_is_a_ribbon_participant_not_an_overlay` | 1001 | CORE | present_into() overlay rewrite for fullscreen/maximize. | KEEP |

#### `src/core/tests.rs` — 167 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `fullscreen_horizontal_navigation_releases_exclusive_overlay` | 65 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `fullscreen_horizontal_navigation_without_neighbour_keeps_overlay` | 171 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `config_compositor_spring_reaches_camera` | 188 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_cycle_layout_wraps_around` | 221 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_window_created_produces_layout_placement` | 235 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_workspace_cycle_layout_helper_wraps` | 273 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_move_right_single_window_swaps_not_merges` | 304 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_move_left_right_reversible` | 315 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_move_right_multi_window_extracts` | 327 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_move_right_boundary_is_noop` | 350 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `b1_viewport_then_overview_resets_viewport` | 376 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `b1_overview_then_viewport_resets_overview` | 401 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `b1_viewport_zoom_does_not_corrupt_live_zoom` | 419 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `b2_focus_next_syncs_column_focused_row` | 443 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_set_layout_command_emits_arrange` | 482 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_set_gaps_command_updates_cfg` | 505 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_noop_command_emits_no_publish` | 517 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `the_state_publish_is_the_last_effect_of_the_command_that_produced_it` | 537 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_quit_action_leads_with_shutdown_effect` | 616 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_event_bus_notifies_subscribers` | 644 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_execute_batch_publishes_state_once` | 668 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_query_reports_focus_workspace_layout` | 735 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `query_tree_includes_observability_fields` | 750 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `query_tree_reports_client_pid` | 778 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_query_visible_windows_and_info` | 800 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_best_focus_prefers_overlay_window` | 815 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_query_json_topics_return_wellformed_documents` | 864 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `query_json_unknown_topic_is_an_error` | 905 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_multi_column_overflow_prevention` | 913 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_new_column_single_window_keeps_full_width` | 964 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_fullscreen_unfocused_layering` | 991 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_focus_direction_allowed_in_fullscreen` | 1036 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `test_move_window_allowed_in_fullscreen` | 1116 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `camera_centers_focused_column` | 1249 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `ideal_scroll_matches_arrange_geometry` | 1308 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `column_screen_extents_agree_with_arrange` | 1373 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `overview_centers_whole_ribbon` | 1421 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `focus_direction_next_prev_syncs_column_and_camera` | 1476 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `drop_into_column_sets_focused_row` | 1562 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `reload_shrinking_tags_clamps_client_workspace` | 1600 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `ideal_scroll_uses_the_given_workspace` | 1647 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `collapse_column_absorbs_collapsed_weight` | 1701 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `focus_direction_horizontal_keeps_the_focused_row` | 1754 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `best_focus_ignores_unfocused_maximized` | 1820 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `view_workspace_round_trip_keeps_focused_window` | 1903 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `grow_column_does_not_panic_with_many_columns` | 1988 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `grow_column_second_tile_can_reach_fullscreen` | 2015 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `float_new_window_does_not_tremble_between_manage_and_arrange` | 2078 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `fullscreen_column_invariants_match_ribbon_functions` | 2131 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `float_fullscreen_moves_to_tiling_and_back` | 2219 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `toggle_fullscreen_targets_new_tiled_window_not_overlay` | 2313 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `toggle_fullscreen_explicit_focus_resolves_to_b` | 2372 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `toggle_fullscreen_stale_focus_targets_overlay_not_new` | 2409 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `ewmh_fullscreen_promotes_float_and_never_collapses_to_zero` | 2456 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `fullscreen_restore_exact_after_intervening_maximize` | 2566 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `fullscreen_a_then_b_normalize_exact` | 2619 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `fullscreen_toggle_promotes_policy_and_restores_it` | 2672 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `fullscreen_topology_is_idempotent` | 2721 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `fullscreen_policy_accessors` | 2773 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `fs_ctx_excludes_true_fullscreen` | 2795 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `maximized_axis_flags_are_independent` | 2840 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `viewport_zoom_enters_zoomed_mode_and_enlarges_ribbon` | 2862 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `viewport_zoom_out_returns_to_normal` | 2902 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `page_snap_scrolls_camera_by_one_page` | 2920 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `page_snap_does_not_jump_when_ribbon_fits` | 2946 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `property_invariants_hold_under_chaos` | 2977 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `fullscreen_column_normal_new_window_receives_focus` | 3359 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `fullscreen_true_keeps_overlay` | 3389 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `maximized_presented_keeps_overlay_unfocused_does_not` | 3409 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `fullscreen_a_create_b_destroy_b_focus_returns_to_a` | 3432 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `fullscreen_a_create_b_focus_b_does_not_hijack_a` | 3456 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `repeated_create_destroy_keeps_focus_stack_consistent` | 3490 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `workspace_switch_does_not_steal_focus_via_pending` | 3523 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `focus_fullscreen_create_destroy_never_leaves_invalid_focus` | 3557 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `property_random_window_ops_preserve_invariants` | 3594 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `pending_focus_consumed_on_fullscreen_keyboard_dismiss` | 3785 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `pending_focus_consumed_on_maximize_keyboard_dismiss` | 3814 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `pending_focus_invalidated_when_deferred_window_gone` | 3843 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `destroy_overlay_owner_consumes_pending` | 3870 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `orphan_defer_not_lost_when_overlay_destroyed_on_non_active_ws` | 3898 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `orphan_defer_not_lost_when_ws_switch_then_dismiss_on_other_ws` | 3930 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `orphan_defer_not_lost_when_overlay_destroyed_on_non_selected_monitor` | 3966 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `pending_focus_resolved_when_overlay_owner_moved_to_other_ws` | 3999 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `pending_focus_resolved_when_overlay_owner_moved_to_other_mon` | 4038 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `pending_focus_survives_when_overlay_hidden_by_workspace_switch` | 4077 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `pending_focus_survives_when_overlay_on_non_selected_monitor` | 4116 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `the_per_axis_ewmh_maximize_path_resolves_the_deferral_it_orphans` | 4153 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `a_no_op_ewmh_maximize_request_leaves_a_live_deferral_alone` | 4201 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `maximize_roundtrip_and_unmaximize` | 4248 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `destroy_background_window_keeps_active_monitor_focus` | 4290 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `overlay_desired_geometry_matches_layout` | 4367 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `fullscreen_desired_geometry` | 4393 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `maximize_desired_geometry` | 4424 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `float_desired_geometry` | 4447 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `only_an_exclusive_overlay_may_be_a_floating_fullscreen` | 4510 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `a_window_that_stops_floating_stops_being_sticky` | 4583 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `toggle_float_preserves_window_origin` | 4654 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `native_float_geometry_is_independent_from_tile_rect` | 4700 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `adopted_float_request_is_projected_verbatim` | 4777 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `float_gaining_new_context_is_settled_before_first_arrange` | 4837 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `self_resize_does_not_mutate_desired` | 4913 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `self_resize_tiled_causes_reapply` | 4972 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `self_resize_float_can_follow` | 5018 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `property_geometry_pipeline_consistency` | 5080 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `audit_p1_tiled_self_resize_reassert` | 5422 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p1_fullscreen_self_resize_reassert` | 5484 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p1_float_follow_and_adopt` | 5543 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p1_float_consecutive_requests_converge` | 5592 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p1_invalid_configure_request_model_clamped` | 5652 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p1_tiled_invalid_geometry_never_collapses_to_zero` | 5709 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p2_fullscreen_lifecycle_no_orphan_overlay` | 5772 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p2_fullscreen_grid_configure_notify_storm` | 5829 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p2_column_normal_fullscreen_is_ribbon_tile` | 5895 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p3_maximize_unmaximize_tracks_presented` | 5926 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p3_float_geometry_follows_model` | 5968 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p3_float_to_tiled_reasserts_authority` | 6007 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p4_fullscreen_dialog_steals_focus` | 6060 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p4_tiled_dialog_resize_close_consistent` | 6105 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p4_orphan_transient_parent` | 6158 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_r5_transient_chain_depth1_stays_coherent` | 6415 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_r5_transient_chain_depth2_stays_coherent` | 6420 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_r5_transient_chain_depth4_at_the_limit_stays_coherent` | 6425 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_r5_transient_chain_depth5_beyond_the_limit_stays_coherent` | 6430 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_r5_destroyed_parent_orphans_no_child` | 6438 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_r5_pending_transient_queue_never_dangles` | 6468 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p5_monitor_switch_keeps_other_overlay` | 6498 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p5_move_to_workspace_keeps_sel_mon` | 6528 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p5_move_to_monitor_moves_ownership` | 6563 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p5_fullscreen_owner_destroyed_no_orphan` | 6599 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p5_move_to_monitor_then_destroy_leaves_no_orphan` | 6627 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p5_multi_monitor_minifuzz` | 6686 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p9a_stale_applied_detected_and_converges` | 6918 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p9b_old_applied_reemits` | 6960 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p9c_destroy_eliminates_desired_applied_refs` | 6991 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p9d_move_workspace_removes_old_desired` | 7042 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p9e_fullscreen_owner_destroyed_resolves_pending` | 7075 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p9f_maximize_owner_destroyed_cleans_presented` | 7102 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `audit_p9g_float_configure_storm_no_backoff` | 7129 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `property_realistic_client_resistance` | 8047 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `configure_request_fullscreen_is_ignored_model_a` | 8117 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `net_active_window_respects_presented_overlay_policy` | 8215 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `view_workspace_fixes_focus_immediately` | 8365 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `view_workspace_invariant_after_effect_stage` | 8398 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `window_targeted_actions_ignore_the_focused_window` | 8427 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `a_targeted_window_arranges_its_own_monitor` | 8489 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `a_window_id_that_names_nothing_changes_nothing` | 8517 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `a_percentage_resize_scales_with_the_workarea` | 8566 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `a_percentage_resize_on_an_empty_workspace_is_a_no_op` | 8631 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `toggle_float_rejects_cross_monitor_focus` | 8640 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `extreme_gaps_do_not_produce_invalid_or_offscreen_geometry` | 8675 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `move_to_workspace_keeps_one_placement_per_client` | 9468 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `new_column_keeps_one_placement_for_a_cross_monitor_focus` | 9585 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `a_monitor_move_resolves_the_deferral_its_focus_request_orphans` | 9629 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `requested_focus_move_resolves_the_deferral_it_orphans` | 9697 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `maximize_presentation_is_re_derived_on_every_monitor_that_shows_it` | 9738 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `move_to_monitor_does_not_duplicate_the_focus_stack_entry` | 9790 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `toggle_float_ignores_a_focus_slot_that_names_a_dead_window` | 9832 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP |
| `prop_invariants_preserved_under_command_sequences` | 9886 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `prop_pending_focus_postcondition_holds_after_every_command` | 9946 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `prop_unmap_leaves_no_dangling_reference` | 9982 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `prop_absorbing_commands_reach_fixpoint` | 10206 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `prop_effects_are_well_formed` | 10286 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `prop_move_resize_sanitizes_and_agrees_with_effect` | 10390 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `prop_camera_spring_cfg_is_sanitized_and_pure` | 10513 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `prop_action_vocabulary_round_trips` | 10649 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `prop_action_parse_is_case_and_whitespace_insensitive` | 10711 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |
| `prop_ipc_json_is_well_formed` | 10817 | CORE | Engine command semantics, focus/overlay lifecycle, desired geometry. | KEEP(R) |

#### `src/main.rs` — 10 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `a_complete_install_reports_nothing` | 551 | CORE | signal-disposition install and the refusal report it prints. | KEEP |
| `a_missed_sigchld_names_reaping_and_not_stoppability` | 559 | CORE | signal-disposition install and the refusal report it prints. | KEEP |
| `a_missed_stop_signal_names_stoppability` | 570 | CORE | signal-disposition install and the refusal report it prints. | KEEP |
| `a_partial_install_reports_every_lost_guarantee` | 579 | CORE | signal-disposition install and the refusal report it prints. | KEEP |
| `a_terminal_hangup_shuts_the_window_manager_down` | 608 | CORE | signal-disposition install and the refusal report it prints. | KEEP |
| `the_stop_signals_are_distinct` | 620 | CORE | signal-disposition install and the refusal report it prints. | KEEP |
| `every_stop_signal_can_be_named_in_a_refusal_report` | 633 | CORE | signal-disposition install and the refusal report it prints. | KEEP |
| `an_unconfigured_signal_reports_no_consequence` | 654 | CORE | signal-disposition install and the refusal report it prints. | KEEP |
| `every_reported_signal_is_named` | 666 | CORE | signal-disposition install and the refusal report it prints. | KEEP |
| `every_installed_disposition_reports_its_consequence` | 683 | CORE | signal-disposition install and the refusal report it prints. | KEEP |

#### `src/userconfig.rs` — 27 tests, crate `maverick (src/)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `mod_alias_matches_super_for_legibility_chords` | 1464 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `missing_file_uses_entire_compiled_config` | 1472 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `broken_toml_uses_entire_compiled_config` | 1481 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `fallback_config_has_reachable_workspace_binds` | 1493 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `invalid_binding_is_dropped_without_losing_valid_file_values` | 1528 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `user_numeric_bind_keeps_other_auto_binds` | 1555 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `auto_workspace_binds_false_disables_generation` | 1593 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `n_tags_limits_generated_workspace_binds` | 1609 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `parses_supported_keys_and_actions` | 1631 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `list_sections_replace_compiled_lists` | 1648 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `rule_fullscreen_policy_parses_and_all_aliases_agree` | 1666 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `compositor_spring_aliases_agree_with_animations_table` | 1701 | CORE | user TOML config parsing, validation and fallback. | REWRITE |
| `compositor_spring_aliases_validate_like_animations_table` | 1746 | CORE | user TOML config parsing, validation and fallback. | REWRITE |
| `animations_table_wins_over_deprecated_compositor_aliases` | 1794 | CORE | user TOML config parsing, validation and fallback. | REWRITE |
| `compositor_table_enables_and_disables_bypass` | 1821 | COMPOSITOR | user TOML config parsing, validation and fallback. | REMOVE |
| `general_compositor_enabled_alias_still_works` | 1840 | COMPOSITOR | user TOML config parsing, validation and fallback. | REMOVE |
| `rule_ignore_initial_state_parses_and_all_aliases_agree` | 1849 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `theme_preset_fills_colors_but_explicit_colors_win` | 1866 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `unknown_theme_name_is_ignored` | 1895 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `gaps_legacy_alias_sets_both_inner_and_outer` | 1909 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `gaps_inner_outer_can_be_set_independently` | 1922 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `rule_opacity_and_border_width_are_parsed` | 1940 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `shipped_example_config_parses` | 1957 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `every_compiled_default_keysym_is_named` | 2012 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `classified_load_marks_a_broken_file_as_the_compiled_baseline` | 2026 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `classified_load_marks_a_missing_file_as_the_compiled_baseline` | 2045 | CORE | user TOML config parsing, validation and fallback. | KEEP |
| `classified_load_marks_a_partly_rejected_file_as_the_users_file` | 2059 | CORE | user TOML config parsing, validation and fallback. | KEEP |

#### `tests/child_lifecycle.rs` — 5 tests, crate `maverick (integration)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `a_child_of_this_process_cannot_be_waited_for` | 51 | INTEGRATION | SA_NOCLDWAIT contract observed from outside the process. | KEEP |
| `a_spawned_child_leaves_no_zombie_without_an_explicit_reap` | 79 | INTEGRATION | SA_NOCLDWAIT contract observed from outside the process. | KEEP |
| `the_external_image_fallback_does_not_depend_on_a_childs_exit_status` | 120 | INTEGRATION | SA_NOCLDWAIT contract observed from outside the process. | KEEP |
| `a_failed_external_decode_does_not_report_a_wait_failure` | 173 | INTEGRATION | SA_NOCLDWAIT contract observed from outside the process. | KEEP |
| `a_bounded_pipe_read_does_not_depend_on_the_childs_status` | 224 | INTEGRATION | SA_NOCLDWAIT contract observed from outside the process. | KEEP |

#### `tests/no_wait_in_wm.rs` — 2 tests, crate `maverick (integration)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `no_code_linked_into_the_window_manager_waits_on_a_child` | 148 | INTEGRATION | no crate linked into the WM may waitpid. *(impl-detail)* | REWRITE |
| `the_wait_detector_can_still_see_a_real_violation` | 224 | INTEGRATION | no crate linked into the WM may waitpid. *(impl-detail)* | REWRITE |

#### `tests/source_constraints.rs` — 2 tests, crate `maverick (integration)`

| test | line | class | asserts | verdict |
|---|---:|---|---|---|
| `no_production_code_writes_the_camera_target_field` | 119 | LEGACY | grep of production sources for a banned call shape. *(impl-detail)* | REMOVE |
| `the_monitor_change_handler_re_derives_the_camera_before_projecting` | 162 | LEGACY | grep of production sources for a banned call shape. *(impl-detail)* | REMOVE |

**Rust rows: 1094.**

### 2.2 Proptest regression seed files

| file | seeds | applies to | dies with the compositor? |
|---|---:|---|---|
| `proptest-regressions/core/layout.txt` | 6 | every proptest in `src/core/layout.rs` (18 closure-form bodies) | no |
| `proptest-regressions/core/present.txt` | 3 | every proptest in `src/core/present.rs` (6) | no |
| `proptest-regressions/core/tests.txt` | 21 | every proptest in `src/core/tests.rs` (10) | no |
| `proptest-regressions/backend/x11/render.txt` | 1 | the rounded-mask proptests in `src/backend/x11/render.rs` | **yes** — seed `(w,h)=(62,62), r=31` is the tangent-row case |

### 2.3 `[[test]]` targets

| target | manifest | tests | note |
|---|---|---:|---|
| `ctl_props` | `maverick-sys/Cargo.toml:46-48` | 5 | explicitly declared even though auto-discovery would also find `tests/ctl_props.rs`; the only `[[test]]` in the workspace |

### 2.4 C harnesses (`tests/*.c`, 13 sources + 5 gitignored compiled binaries)

| file | lines | role | compositor-only? |
|---|---:|---|---|
| `damager.c` | 236 | repaints a solid window; `--small-damage` accumulates bounded damage | yes |
| `pxsample.c` | 87 | reads a rect of the Composite overlay via XGetImage and asserts a colour | yes |
| `staticwin.c` | 65 | solid-colour window, backdrop / structural change | no |
| `winmove.c` | 22 | XMoveWindow/XResizeWindow on an override-redirect window | no |
| `mgdwin.c` | 87 | managed tiled client; cooperative / non-coop / lying WM_DELETE_WINDOW | no |
| `hostile.c` | 384 | stdin-driven EWMH/ICCCM abuse: degenerate + huge ConfigureRequest, synthetic ConfigureNotify, focus grabs | no |
| `stress.c` | 338 | 8-flow hostile loop against a live tiled session | no |
| `realwin.c` | 148 | real managed client, SIGUSR1 fullscreen toggle, state snapshots | no |
| `fsclient.c` | 161 | override-redirect fullscreen/stale-frame forensic client | partly |
| `keyboard-probe.c` | 156 | XTest-driven key/modifier probing for xkb planner checks | no |
| `dockstrut.c` | 54 | maps `_NET_WM_WINDOW_TYPE_DOCK` with `_NET_WM_STRUT_PARTIAL` | no |
| `setmon.c` | 107 | RANDR 1.5 `RRSetMonitor` splitter for the 2-output suite | no |
| `stacking-probe.c` | 95 | creates dock + peer windows and dumps XQueryTree order | no |

`tests/{damager,mgdwin,pxsample,staticwin,winmove}` also exist as compiled binaries but are
`.gitignore`d (`.gitignore:4-9`) and untracked — built on demand by the harnesses.

### 2.5 shell suites (`tests/*.sh`, 27)

| script | lines | asserts | compositor-only? |
|---|---:|---|---|
| `common.sh` | 133 | shared harness: build helpers, Xephyr lifecycle, pid tracking. Not a suite. | no |
| `compat-matrix.sh` | 331 | browser / Wine-like / game / focus-grab / transient-depth phases | partly |
| `session-isolation.sh` | 123 | two sessions get distinct ids; killing one leaves the other alive | no |
| `session-security.sh` | 249 | runtime dir, socket, record and cross-user boundaries | no |
| `session-suite.sh` | 1013 | a real session: create, exercise, remove | no |
| `xephyr-2mon.sh` | 366 | two RANDR outputs, real multi-monitor behaviour | no |
| `xephyr-bypass-fix.sh` | 72 | fullscreen-bypass direct-presentation regression | yes |
| `xephyr-client-death.sh` | 122 | SIGKILL a client mid-state; WM survives and still answers | no |
| `xephyr-compositor.sh` | 356 | scrolling, focus/raise, partial damage, animation, viewport culling — real GLX | yes |
| `xephyr-config-wallpaper.sh` | 99 | `[wallpaper] path` tilde expansion + reload re-seeds it | no |
| `xephyr-disconnect.sh` | 261 | X server SIGKILLed under a live WM leaves a truthful record | partly |
| `xephyr-ewmh-extents.sh` | 92 | `_NET_FRAME_EXTENTS` tracks the real border | no |
| `xephyr-ewmh-workarea.sh` | 89 | `_NET_WORKAREA` present at startup; dock shrinks and restores it | no |
| `xephyr-fs-pointer.sh` | 244 | fullscreen + pointer-loss: click reaches B, SYNC grab always released | no |
| `xephyr-fs-stale-deterministic.sh` | 165 | deterministic variant of the stale-frame harness | partly |
| `xephyr-fs-stale.sh` | 100 | forensic fullscreen/stale-frame reproduction with real pixels | partly |
| `xephyr-ipc-edge.sh` | 105 | long sid, missing/stale socket, restart loop, quit loop | no |
| `xephyr-partial.sh` | 239 | buffer-age + accumulated damage + scissor leave the framebuffer correct | yes |
| `xephyr-pointer-float.sh` | 252 | click-to-focus must not warp the pointer; torn-off tile re-insert | no |
| `xephyr-real-fullscreen.sh` | 164 | A/B forensic for real managed fullscreen | partly |
| `xephyr-restart-config.sh` | 74 | `restart` re-execs with the identical `--config` | no |
| `xephyr-restart.sh` | 102 | re-exec keeps every window drawn (texture rebind) | yes |
| `xephyr-rounded-clip.sh` | 177 | client content never shows outside the rounded visual region | yes |
| `xephyr-shutdown.sh` | 119 | quit within SHUTDOWN_BUDGET for coop / non-coop / lying clients | no |
| `xephyr-stress.sh` | 180 | hostile stress client — detection only, no source edits | no |
| `xephyr-suite.sh` | 419 | fullscreen / transient / viewport against xterm, firefox, mpv, a GL game | partly |
| `xephyr-wallpaper.sh` | 186 | shader/animation wallpaper end-to-end through the compositor | yes |

### 2.6 python suites (`tests/*.py`, 4)

| file | lines | asserts | run by CI? |
|---|---:|---|---|
| `install-smoke.py` | 653 | installer prefix boundary, exact installed binary set, broken-binary detection, repeat convergence | **yes** — `.github/workflows/ci.yml:44` |
| `xvfb-stacking.py` | 232 | XQueryTree ordering regression on an isolated Xvfb, `--no-default-features` build | **yes** — `.github/workflows/ci.yml:56` |
| `xvfb-keyboard.py` | 216 | xkb planner/dispatch against a real keymap | no |
| `tree_lines.py` | 38 | helper: renders `maverickctl query tree` JSON as lines (split out of `xephyr-2mon.sh`) | no (helper) |

**26 of 27 shell suites and 2 of 4 python suites are never executed by CI.** They are manual
harnesses. Of the ones that are, exactly one (`xvfb-stacking.py`) runs against the
`--no-default-features` build.

### 2.7 CI (`.github/workflows/ci.yml`)

| job | step | command | what it proves |
|---|---|---|---|
| `wm` | Clippy | ``cargo clippy --workspace --all-targets -- -D warnings`` | compiles every member *and* every test target — including `maverick-gl` and `maverick-vk` |
| `wm` | Test | ``cargo test --workspace`` | 1090 tests, **default features** (compositor ON) |
| `install-script` | bash -n | ``bash -n install.sh`` | the installer still parses |
| `install-script` | Installer smoke test | ``python3 tests/install-smoke.py`` | installer boundary contract |
| `x11-smoke` | Build supported profile | ``cargo build --release --no-default-features -p maverick -p maverick-sys`` | **build only** — no test runs here |
| `x11-smoke` | Xvfb stacking regression | ``python3 tests/xvfb-stacking.py`` | one live X11 ordering check |

## 3. Classification counts

| class | tests | survives a non-composited WM? |
|---|---:|---|
| `CORE` — pure WM logic (layout, focus, commands, state, config) | 387 | yes |
| `X11` — X11 backend | 127 | yes |
| `INTEGRATION` — cross-crate / cross-process (`maverick-sys` 199 + root integration 7) | 206 | yes (maverickctl and the control socket survive) |
| `PROPERTY` — proptest-driven | 124 | partly — `maverick-gl/tests/props.rs` (10) and `maverick-vk/tests/properties.rs` (14) do not |
| `COMPOSITOR` — GL/GLX/Damage/buffer-age/shader/wallpaper/render-property | 234 | no |
| `LEGACY` — diagnostic / low value | 16 | no |
| **total** | **1094** | |

Caveat on `INTEGRATION`: 199 of the 206 are in-crate unit tests of `maverick-sys`, which is the
OS/IPC boundary and the home of the auto-discovered `maverickctl` binary
(`maverick-sys/src/bin/maverickctl.rs`). They are not window-manager tests; they are "does the
shipped CLI and control protocol still work" tests. They survive because the crate survives,
not because they prove anything about tiling.

Overlay counts (tests additionally tagged):

| tag | tests |
|---|---:|
| `REGRESSION` — pinned by a proptest seed file or an `audit_*` name: 34 `audit_*` in `src/core/tests.rs` + 10 `prop_*` there + 23 closure-form proptests in `layout.rs`/`present.rs` + 10 `prop_*` in `render.rs` | 77 |
| implementation-detail (see §5) | 31 |
| `#[ignore]`d | 2 |

## 4. Deletion impact, crate by crate

Numbers are the number of `#[test]` attributes that disappear, verified against the built
binaries (`--list`), not estimated.

### 4.0 The `compositor-opengl` mechanism — verified

`maverick-gl` is an **optional** dependency reached only through
`default = ["compositor-opengl"]` -> `compositor-opengl = ["dep:maverick-gl", "x11rb/composite",
"x11rb/damage", "x11rb/xfixes"]` (`Cargo.toml:84`, `Cargo.toml:97`). It is the only optional dep in
the workspace.

Two consequences, both measured rather than assumed:

1. **`cargo test -p maverick --no-default-features` does not build `maverick-gl` at all.** Build log
   for that configuration compiles only `x11rb-protocol`, `x11rb`, `maverick-x11`, `maverick` — no
   `maverick-gl`, no `maverick-img` compile step. The root unit-test binary reports **518** tests
   against **588** with default features: **70 tests vanish**, and 4 `compositor::placeholder_substep_tests`
   appear in their place. The 70 are exactly `src/backend/x11/compositor_gl.rs`, which
   `src/backend/x11/compositor.rs:21-23` gates behind `#[cfg(feature = "compositor-opengl")] #[path = "compositor_gl.rs"]`.
2. **But `maverick-gl`'s own 62 tests still run in CI**, because `maverick-gl` is a *workspace member*
   and CI runs `cargo test --workspace` with default features (`ci.yml:27`). `--no-default-features`
   only disables features of the *selected* packages; `maverick-gl` declares none, so
   `cargo test --workspace --no-default-features` would still run its 62 tests. **[UNVERIFIED]** —
   I did not run that exact command; the reasoning is from cargo's documented feature-resolution
   semantics plus the observed per-package build graph.

### 4.1 `maverick-gl` — 62 tests, **all 62 die**

| file | tests | named tests |
|---|---:|---|
| `maverick-gl/src/renderer.rs` | 38 | `a_positive_count_with_a_list_is_usable`, `no_list_or_no_count_is_not_usable`, `argb32_never_binds_through_a_10bit_config`, `rgb24_binds_through_a_32bit_buffer_with_alpha_bits`, `depth_must_match_the_configs_own_visual`, `exact_visual_wins_over_same_depth`, `a_narrower_config_is_rejected_a_wider_one_is_allowed`, `dont_care_bind_targets_are_usable`, `bind_capability_follows_the_texture_format`, `colour_index_and_window_only_configs_are_skipped`, `software_rasterizers_classify_as_software`, `real_gpus_classify_as_gpu_regardless_of_vendor`, `empty_renderer_info_classifies_unknown`, `renderer_info_display_matches_startup_block`, `filter_maps_to_gl_constants`, `scissor_box_never_leaves_the_viewport`, `scissor_box_passes_an_interior_damage_rect_through_unchanged`, `scissor_box_never_shrinks_as_the_damage_grows`, `visual_colour_bits_are_the_wide_sum_of_its_channels`, `fbconfig_acceptance_is_exactly_what_the_visual_requires`, `accepted_fbconfigs_rank_by_how_well_they_match`, `a_pixmap_is_only_ever_bound_in_a_format_its_config_can_bind`, `only_y_inverted_false_needs_a_flip`, `reject_tally_reports_each_found_reason_with_its_count`, `premultiplied_upload_keeps_one_texel_per_pixel_and_the_source_alpha`, `premultiplied_channels_never_exceed_their_alpha`, `premultiplied_channels_are_rounded_to_nearest_and_monotone`, `a_cpu_texture_is_only_the_handle_it_was_given`, `glx_context_attribute_list_is_paired_and_zero_terminated`, `glx_context_is_new_enough_for_the_builtin_shaders`, `draw_shader_hands_the_window_program_back`, `a_shader_wallpaper_frame_still_reports_no_gl_error`, `the_window_quad_after_a_shader_wallpaper_raises_no_invalid_operation`, `a_window_quad_after_a_shader_wallpaper_writes_its_destination_rect`, `draw_raw_then_shader_then_draw_leaves_the_window_program_current`, `a_shader_wallpaper_drawn_last_leaves_a_clean_frame`, `a_leak_does_not_cross_a_frame_boundary`, `a_uniform_write_against_a_non_current_program_never_reaches_that_program` |
| `maverick-gl/tests/props.rs` | 10 | `extension_matching_reads_names_not_whitespace`, `an_extension_that_is_only_part_of_a_name_is_not_found`, `a_software_renderer_is_recognised_whichever_field_names_it`, `acceleration_classification_ignores_ascii_case`, `an_unknown_acceleration_needs_both_fields_empty`, `the_startup_report_has_one_line_per_field`, `a_visual_only_claims_colour_it_carries`, `extension_matching_answers_odd_driver_strings`, `filter_modes_map_to_distinct_gl_samplers`, `capability_probe_answers_the_same_every_time` |
| `maverick-gl/tests/loader.rs` | 8 | `every_loaded_entry_point_is_pointer_sized_and_aligned`, `a_name_argument_is_a_borrowed_c_string_that_is_already_terminated`, `an_optional_entry_point_is_an_option_over_a_c_abi_function_pointer`, `probing_for_a_driver_is_stable_and_never_fails`, `both_tables_load_completely_when_there_is_a_driver`, `symbol_resolution_works_without_a_context_and_names_what_is_missing`, `a_stubbed_symbol_still_needs_the_extension_token_to_be_called`, `the_re_exported_display_is_the_shared_bootstrap_type` |
| `maverick-gl/tests/shared_bootstrap.rs` | 3 | `the_gl_display_handle_is_the_shared_one`, `the_error_cell_is_the_installed_one`, `x_error_names_come_from_the_shared_table` |
| `maverick-gl/src/dl.rs` | 2 | `gl_candidates_are_absolute_paths_first_and_nul_free`, `a_symbol_name_with_an_embedded_nul_never_resolves` |
| `maverick-gl/src/lib.rs` | 1 | `extension_matching_is_token_exact` |

- **Survives untouched:** nothing. Every test names a GL, GLX, fbconfig, EGL-independent renderer
  or shader concept.
- **Must be rewritten:** nothing.
- **Note:** `tests/shared_bootstrap.rs` is the only file here asserting something a *non*-GL crate
  also asserts (`maverick-x11/tests/error_handler_scope.rs` and `maverick-x11/tests/x_error_signal.rs`
  already pin the shared `XDisplay` / error-cell / error-name-table contract from the other side).
  No rewrite is owed.

### 4.2 `maverick-render` — 3 tests, **all 3 die**

| file | tests | named tests |
|---|---:|---|
| `maverick-render/tests/contract_types.rs` | 3 | `acceleration_label_is_one_plain_line_and_unique`, `renderer_info_report_carries_every_field`, `default_draw_quad_draws_nothing` |

`maverick-render/src/lib.rs` has **zero** tests (measured: `maverick_render-… --list` -> `0 tests`).
The crate's only workspace consumer is `src/backend/renderer.rs:26`, under `#[allow(unused_imports)]`,
and `maverick-gl` never references it. All three tests are assertions about a display-string and a
default `DrawQuad` on a trait nobody implements.

### 4.3 `maverick-vk` — 36 tests, **all 36 die**; 2 of them were already dead

| file | tests | named tests |
|---|---:|---|
| `maverick-vk/tests/properties.rs` | 14 | `surface_format_is_unavailable_only_when_the_surface_lists_nothing`, `surface_format_follows_the_documented_preference_order`, `present_mode_never_names_a_mode_outside_mailbox_and_fifo`, `present_mode_prefers_mailbox_exactly_when_the_driver_offers_it`, `surface_owned_extent_overrides_any_request`, `a_sentinel_on_one_axis_only_is_not_the_you_choose_sentinel`, `extent_is_total_and_lands_in_the_advertised_window`, `extent_is_clamped_to_the_surface_limits`, `extent_clamp_is_monotone_in_the_request`, `image_count_stays_inside_what_the_driver_allows`, `selection_helpers_are_pure_functions_of_their_input`, `every_result_code_maps_to_an_error_that_names_it`, `each_error_variant_is_identifiable_from_its_message`, `device_report_shows_every_field_it_was_built_from` |
| `maverick-vk/tests/unit.rs` | 12 | `format_prefers_bgra8_srgb`, `format_undefined_single_allows_any`, `format_falls_back_to_first`, `format_empty_is_none_not_panic`, `present_mode_prefers_mailbox_then_fifo`, `extent_clamps_within_bounds`, `extent_uses_current_when_fixed`, `extent_uses_current_when_only_one_axis_is_real`, `extent_survives_an_inverted_window`, `image_count_plus_one_capped`, `image_count_does_not_overflow_at_the_top_of_the_range`, `vk_error_from_vk_result_is_descriptive` |
| `maverick-vk/src/pacing.rs` | 5 | `a_failed_frame_never_leaves_the_fence_unwaitable`, `the_record_matches_what_the_driver_did`, `a_fresh_fence_owes_nothing`, `a_submitted_frame_owes_exactly_one_completion`, `a_frame_that_fails_after_the_previous_submit_leaves_a_waitable_fence` |
| `maverick-vk/src/device.rs` | 3 | `device_type_score_is_total_over_the_whole_type_space`, `known_device_types_form_a_strict_order_with_discrete_first`, `unknown_device_type_never_outranks_a_discrete_gpu` |
| `maverick-vk/tests/smoke.rs` | 2 | `smoke_init_and_present`, `drop_after_present_is_validation_clean` — **both `#[ignore]`d** (`smoke.rs:37,101`), and additionally gated on `MAVERICK_VK_SMOKE=1`. These have never run in CI. |

**Dead-weight ratio: 3489 lines of source (2190 src + 1299 test, per `COMPOSITOR-AUDIT.md:724-725`)
carrying 36 tests — 34 that run and 2 that are permanently skipped — for a crate with zero reverse
dependencies.** `cargo tree -i maverick-vk` errors out (no reverse edge); the only reference in the
workspace is `tests/no_wait_in_wm.rs:63`, which lists `maverick-vk/src` in `WM_SOURCE_DIRS` *as if it
were linked into the WM* — a manifest of the audit's own confusion, called out in
`COMPOSITOR-AUDIT.md:847-848`.

### 4.4 `maverick-img` — 50 tests, **0 die**

Contrary to the campaign premise, `maverick-img` is a **mandatory** root dependency
(`Cargo.toml:49`, `maverick-img = { path = "maverick-img" }`) with a live non-compositor caller:
`src/backend/x11/rootwall.rs` — the "Root-pixmap wallpaper — the no-compositor, feh-style path" —
declares `mod rootwall` ungated at `src/backend/x11/mod.rs:106` and is invoked from five
unconditional sites (`events.rs:595`, `mod.rs:904`, `mod.rs:1320`, `actions.rs:436`,
`actions.rs:481`).

- `maverick-img/src/lib.rs` (10) — `ppm_roundtrip_trivial`, `png_rgba2x2`, `png_rgb3x1`,
  `png_grayscale_alpha`, `png_palette_with_trns`, `png_paeth_filter`, `png_sub_filter`, `qoi_inline`,
  `bmp_inline`, `farbfeld_inline`. **Survive untouched** (plus `tests/child_lifecycle.rs:120`, which
  exercises the external-converter fallback).
- `maverick-img/tests/properties/mod.rs` (38) — the strongest adversarial decoder suite in the repo
  (`arbitrary_bytes_never_panic_or_decode_to_a_malformed_image`, `truncation_never_panics`,
  `trailing_garbage_does_not_change_the_decoded_pixels`, `decoding_is_deterministic`,
  `every_truncation_of_a_valid_file_is_safe`, `one_bit_indices_share_one_byte`, …). **Survive
  untouched.** Worth keeping every one: a wallpaper decoder fed a user-chosen file is the most
  attacker-reachable parser in a non-composited WM.
- `maverick-img/tests/decode_dispatch.rs` (2). **Survive untouched.**

Note: `tests/properties/mod.rs` is picked up by cargo's integration-test auto-discovery despite the
non-canonical filename — confirmed empirically: `cargo test -p maverick-img -- --list` reports
`48 tests` for the lib target and `2 tests` for `decode_dispatch`.

### 4.5 `maverick-sys` — 199 tests, **0 die** (but 199 of them are not WM tests)

| file | tests | verdict |
|---|---:|---|
| `src/control.rs` | 16 | KEEP |
| `src/json.rs` | 26 | KEEP |
| `src/identity.rs` | 18 | KEEP |
| `src/session/{mod,xserver,proc,lifecycle}.rs` | 61 | KEEP |
| `src/ctl/{mod,session,windows}.rs` | 25 | KEEP |
| `src/hub.rs` | 7 | KEEP |
| `src/lib.rs` | 4 | KEEP |
| `tests/{signal_install,identity_props,hub_props,control_props,json_props,ctl_props,ctl_replies}.rs` | 42 | KEEP |

Nothing dies: `maverick-sys` is in both the default and the `--no-default-features` runtime graph,
and it is the home of the auto-discovered `maverickctl` binary
(`maverick-sys/src/bin/maverickctl.rs`), which `install.sh` ships. But **none of the 199 asserts
anything about tiling, layout, focus or geometry.** 86 of them (43 %) test the session manager
(`session/` 61) or the admin CLI (`ctl/` 25); the rest are instance identity, the control-socket
protocol and the JSON codec. For a "minimal test surface" this is the largest block of tests that
could be *moved* to a CLI crate rather than deleted.

### 4.6 Inside the root crate (`src/`, 592 tests)

| file | tests | verdict | reason |
|---|---:|---|---|
| `src/core/tests.rs` | 167 | **KEEP 167** | the Engine contract suite; this is the load-bearing WM surface |
| `src/core/layout.rs` | 38 | KEEP 38 | `arrange()` geometry contract |
| `src/core/invariants.rs` | 24 | KEEP 24 | settled-geometry invariants |
| `src/backend/x11/reconciler.rs` | 38 | KEEP 38 | ConfigureRequest authority + convergence |
| `src/core/present.rs` | 15 | KEEP 15 | fullscreen/maximize overlay rewrite |
| `src/core/action.rs` | 13 | KEEP 13 | Action vocabulary |
| `src/backend/x11/tests.rs` | 27 | KEEP 27 | xkb planner/dispatch + event-loop wait budget |
| `src/backend/x11/render.rs` | 57 | KEEP 33 / REMOVE 24 | see below |
| `src/userconfig.rs` | 27 | KEEP 22 / REWRITE 3 / REMOVE 2 | see below |
| `src/backend/x11/struts.rs` | 4 | KEEP 4 | dock reservation |
| `src/backend/x11/teardown.rs` | 5 | KEEP 5 | shutdown halves |
| `src/backend/x11/ewmh.rs` | 3 | KEEP 3 | stacking-list determinism |
| `src/config.rs` | 9 | KEEP 9 | window rules |
| `src/main.rs` | 10 | KEEP 10 | signal dispositions |
| `src/core/ipc.rs` | 9 | KEEP 7 / REWRITE 2 | compositor field in `inspect` |
| `src/core/commands.rs` | 3 | KEEP 3 | `SetWallpaper` (root-pixmap path) |
| `src/backend/x11/compositor_gl.rs` | 74 | REMOVE 74 | the compositor |
| `src/compositor_policy.rs` | 24 | REMOVE 24 | Compose-vs-Bypass; meaningless with no compositor |
| `src/backend/x11/framesched.rs` | 24 | REMOVE 24 | damage/vsync/wallpaper-shader frame pacing |
| `src/backend/x11/trace.rs` | 11 | REMOVE 11 | `--features window-trace` ring buffer, off by default, never run in CI |
| `src/backend/x11/compositor.rs` | 4 | REMOVE 4 | stub substep integrator, duplicated in `compositor_gl.rs` |
| `src/core/framebench.rs` | 3 | REMOVE 3 | per-frame allocation counter, "the headline compositor invariant" |
| `src/backend/x11/mod.rs` | 3 | REMOVE 3 | `describe_reasons` debug string |

**`src/backend/x11/render.rs` split (57):**

- *REMOVE 24 (COMPOSITOR)* — 21 rounded-corner / Shape-mask / focus-ring tests plus 3 render-list
  projections: `rounded_mask_spans_exactly_the_outer_frame`, `rounded_mask_is_horizontally_symmetric`,
  `rounded_mask_radius_is_bounded_by_half_the_smaller_side`, `zero_radius_is_a_square_full_frame_mask`,
  `degenerate_mask_sizes_stay_valid`, `focus_ring_band_is_thin_and_traces_the_frame_curve`,
  `focus_ring_inner_radius_follows_outer_minus_border`, `focus_ring_vanishes_when_radius_reaches_the_border`,
  `fullscreen_geometry_yields_square_masks_both_kinds`, `prop_rounded_mask_stays_inside_the_frame`,
  `prop_rounded_frame_mask_never_wraps_the_card16_boundary`, `the_masked_frame_never_reaches_past_the_signed_extent`,
  `a_mask_request_can_never_exceed_the_servers_maximum_length`,
  `an_unrepresentable_border_saturates_the_frame_instead_of_trapping`,
  `the_frame_clamp_and_the_mask_clamp_are_both_reached`, `a_forty_thousand_pixel_border_does_not_wrap_the_mask`,
  `a_frame_wider_than_i32_max_does_not_panic_the_radius_clamp`, `prop_rounded_mask_is_symmetric_about_both_sides`,
  `prop_client_clip_measures_exactly_the_inset_frame`, `every_rounded_mask_row_stays_symmetric_at_the_radius_boundary`,
  `tangent_row_is_covered_when_the_radius_is_half_the_width`, `prop_render_list_is_total_and_names_only_real_windows`,
  `prop_render_list_is_deterministic`, `prop_applied_geometry_is_what_the_next_cycle_wants`.
  These describe a *WM-owned decorated frame window* with Shape masks and a GL draw list. Without a
  compositor there is no such frame. **If the replacement WM still rounds corners with X Shape on a
  reparented frame, this block survives instead** — that is a product decision, not a test decision.
  `proptest-regressions/backend/x11/render.txt` (seed `(62,62,31)`, the tangent row) dies with it.
- *KEEP 33* — 18 float normalisation/snap/hint tests, 7 transient-chain tests, 3 parked-rect tests,
  4 client-authority-seal tests, 1 `covering_fullscreen_and_overlay_owner_are_distinct`. These are
  WM float policy and window-ownership rules; they survive unchanged.

**`src/userconfig.rs` split (27):**
- REMOVE 2 — `compositor_table_enables_and_disables_bypass`,
  `general_compositor_enabled_alias_still_works`: they parse `[compositor] enabled`, a key that
  stops existing.
- REWRITE 3 — `compositor_spring_aliases_agree_with_animations_table`,
  `compositor_spring_aliases_validate_like_animations_table`,
  `animations_table_wins_over_deprecated_compositor_aliases`. The *spring* configuration is core:
  it reaches `Camera::step` through `Engine::apply_camera_cfg` and is pinned behaviourally by
  `src/core/tests.rs:10513 prop_camera_spring_cfg_is_sanitized_and_pure`. Only the
  `[compositor.spring_*]` alias names go. Rewrite them against the surviving table name.
- KEEP 22.

**`src/core/ipc.rs` split (9):**
- REWRITE 2 — `the_inspect_document_reports_totals_layout_and_compositor` and
  `default_backend_facts_do_not_claim_a_compositor`. Both assert on the *document shape* of
  `maverickctl inspect`, which is genuinely load-bearing for the CLI; the second is the correct
  statement of "no compositor". Rewrite the first to drop the compositor field; the second becomes
  vacuously true and should be folded into the first.

---

## 5. Protection assessment: implementation-detail tests

31 tests assert an internal shape rather than a behaviour. They would break on a legitimate
refactor and are the rewrite list.

### 5.1 Source-grep guards (4) — the worst offenders, and the repo knows it

`tests/source_constraints.rs:1-12` says so in its own module doc: *"That is weaker than
exercising the code, and it is deliberately so — a renamed function or a moved call would need this
updated, which is a visible cost rather than a silent one."*

| test | what it actually asserts | rewrite as |
|---|---|---|
| `tests/source_constraints.rs:119 no_production_code_writes_the_camera_target_field` | that no line in 8 production files contains `.target =` | an engine-level test: close a window while a scroll spring is mid-flight; assert `camera.pos` converges on the target re-derived for the *shorter* ribbon (the exact case named at `source_constraints.rs:100-113`) |
| `tests/source_constraints.rs:162 the_monitor_change_handler_re_derives_the_camera_before_projecting` | that `retarget_cameras(` appears >= 2 times in the text after `fn handle_monitor_change`, before `self.arrange(i)` | a RANDR-change test through `Engine`: change the monitor screen, then assert every workspace's camera target is inside the new workarea |
| `tests/no_wait_in_wm.rs:148 no_code_linked_into_the_window_manager_waits_on_a_child` | that 7 source directories contain no `.wait()` / `.output()` / `libc::waitpid` token | keep the rule, fix the list: `WM_SOURCE_DIRS` includes `maverick-gl/src` and `maverick-vk/src`, **neither of which is linked into the WM**. A `wait` in `maverick-vk` would be reported as a WM violation |
| `tests/no_wait_in_wm.rs:224 the_wait_detector_can_still_see_a_real_violation` | that the needles still match `maverick-sys/src/ctl/session.rs` | keep — it is a meta-test that makes the guard non-tautological, which is genuinely good practice |

### 5.2 Diagnostic-string and log-format tests (13 of the 31)

(The 11 `src/backend/x11/trace.rs` tests are in the same family and are already on the
REMOVE list in §7.4; they are not double-counted in the 31.)

| test | asserts |
|---|---|
| `src/backend/x11/mod.rs:2289 reasons_read_in_scheduler_order` | the exact order/separator of a `FrameReason` phrase pasted into bug reports |
| `src/backend/x11/mod.rs:2298 a_single_reason_has_no_separator` | ditto |
| `src/backend/x11/mod.rs:2307 no_reasons_read_as_nothing` | ditto |
| `src/backend/x11/trace.rs` (all 11) | the contents of a `--features window-trace` ring buffer that is off by default and never exercised by CI |
| `maverick-gl/src/renderer.rs:3221 reject_tally_reports_each_found_reason_with_its_count` | a human-readable failure tally string |
| `maverick-gl/tests/props.rs:217 the_startup_report_has_one_line_per_field` | one-line-per-field layout of a startup banner |
| `maverick-gl/src/renderer.rs:2717 renderer_info_display_matches_startup_block` | banner text matches a block |
| `maverick-gl/src/renderer.rs:2734 filter_maps_to_gl_constants` | an enum maps to specific GL constants |
| `maverick-gl/src/renderer.rs:3396 glx_context_attribute_list_is_paired_and_zero_terminated` | the literal attribute array is paired and 0-terminated — a property of an array literal |
| `maverick-gl/tests/props.rs:314 filter_modes_map_to_distinct_gl_samplers` | distinct enum values -> distinct constants |
| `maverick-sys/src/session/proc.rs:727 human_bytes_rounds_to_the_unit_a_listing_expects` | a byte-count formatting function |
| `maverick-sys/src/session/proc.rs:736 display_name_never_returns_empty` | a display-name fallback |
| `src/core/framebench.rs:98 the_counter_is_not_vacuously_zero`, `:108 counting_is_off_by_default` | the test harness's own allocation counter |

### 5.3 ABI / type-layout assertions the compiler already guarantees (8)

| test | asserts |
|---|---|
| `maverick-gl/tests/loader.rs:122 every_loaded_entry_point_is_pointer_sized_and_aligned` | `size_of::<fn>() == size_of::<*mut c_void>()` |
| `maverick-gl/tests/loader.rs:140 a_name_argument_is_a_borrowed_c_string_that_is_already_terminated` | the type of a parameter |
| `maverick-gl/tests/loader.rs:168 an_optional_entry_point_is_an_option_over_a_c_abi_function_pointer` | the shape of `Option<fn>` |
| `maverick-gl/tests/shared_bootstrap.rs:19 the_gl_display_handle_is_the_shared_one` | that two `XDisplay`s are the *same newtype* |
| `maverick-gl/tests/shared_bootstrap.rs:31 the_error_cell_is_the_installed_one` | that there is exactly one error cell, by identity |
| `maverick-gl/tests/shared_bootstrap.rs:45 x_error_names_come_from_the_shared_table` | ditto for the name table |
| `maverick-x11/tests/x_error_signal.rs:261 the_display_handle_is_send`, `:275 every_alias_of_the_display_is_non_owning` | `Send` / `Drop` behaviour that a compile-fail test (`static_assertions`) would enforce permanently |

### 5.4 Duplicated-by-later-property unit tests (5)

`maverick-vk/tests/unit.rs` restates, at lower power, what `maverick-vk/tests/properties.rs` proves
exhaustively: `format_prefers_bgra8_srgb` vs `surface_format_follows_the_documented_preference_order`;
`present_mode_prefers_mailbox_then_fifo` vs `present_mode_prefers_mailbox_exactly_when_the_driver_offers_it`;
`extent_clamps_within_bounds` vs `extent_is_total_and_lands_in_the_advertised_window`;
`image_count_plus_one_capped` vs `image_count_stays_inside_what_the_driver_allows`;
`vk_error_from_vk_result_is_descriptive` vs `every_result_code_maps_to_an_error_that_names_it`.
Classic low-value redundancy — but the whole crate dies anyway.

### 5.5 One borderline case

`maverick-core/src/types.rs:3075 damping_extreme_is_bounded_relative_to_stiffness` asserts a
relationship between two **public** fields of `Camera`. Because the fields are public, a caller can
bypass `sanitize_spring` (the same file's `types.rs:3413
camera_step_terminates_and_converges_for_every_degenerate_direct_mutation` says so), so pinning the
post-sanitize domain is a real API contract, not an accident. **KEEP**, but note it will need
rewriting if `Camera`'s fields ever stop being public.

### 5.6 What is load-bearing and must not be touched


- **`src/core/tests.rs` — all 167.** Four families, all behavioural: `test_*` (33) command semantics,
  the fullscreen/maximize/pending-focus lifecycle cluster (≈ 90), the `audit_p1…p9g`/`audit_r5`
  cluster (34, each pinned to a named defect), and 10 proptests whose invariants are stated as
  contracts (`prop_invariants_preserved_under_command_sequences`, `prop_unmap_leaves_no_dangling_reference`,
  `prop_effects_are_well_formed`, `prop_absorbing_commands_reach_fixpoint`,
  `prop_pending_focus_postcondition_holds_after_every_command`, `prop_move_resize_sanitizes_and_agrees_with_effect`,
  `prop_action_vocabulary_round_trips`, `prop_ipc_json_is_well_formed`, …).
- **`src/backend/x11/reconciler.rs` — all 38.** This is the ConfigureWindow authority. Eight of
  them are proptests stating totality, purity, idempotence, convergence and totality of clamping
  (`reconcile_writes_exactly_the_pending_configures`, `reconcile_never_touches_the_logical_state`,
  `a_repeated_reconcile_emits_nothing`, `reconciliation_converges_on_the_latest_desired_state`,
  `a_clamped_float_is_always_x11_valid`, `the_configure_verdict_depends_only_on_geometry_equality`,
  `a_forgotten_window_is_re_emitted`, `only_a_dirty_client_is_re_poked`). Non-composited, this is
  *the* place where the WM meets the wire.
- **`src/core/layout.rs` (38) and `src/core/invariants.rs` (24)** — geometry contract and
  settled-state invariants.
- **`maverick-core/src/types.rs` (36)** — column weights, workarea derivation, spring/camera
  convergence.
- **`maverick-img/tests/properties/mod.rs` (38)** — the parser hardening for a user-supplied file.
- **`maverick-sys/tests/signal_install.rs` (8) + `tests/child_lifecycle.rs` (5)** — `SA_NOCLDWAIT`,
  the one OS contract that, if broken, hangs the event loop.

---

## 6. Coverage gaps

### 6.1 The coordinate/movement path is untested at its two ends — **the largest gap**

The chain is `input -> camera -> layout -> desired -> reconciler -> ConfigureWindow`
(`src/backend/x11/render.rs:5-19` documents it). Tested *in the middle*:

- `src/core/tests.rs:1308 ideal_scroll_matches_arrange_geometry`
- `src/core/tests.rs:1373 column_screen_extents_agree_with_arrange`
- `src/core/layout.rs:2700 the_hit_test_extents_agree_with_the_drawn_placement`
- `src/backend/x11/render.rs:3708 prop_applied_geometry_is_what_the_next_cycle_wants`
- `src/core/tests.rs:5080 property_geometry_pipeline_consistency` (a seeded LCG, not a proptest)

Untested at the **ends**:

1. **Input.** `src/backend/x11/pointer.rs:668 scroll_camera_with_wheel` and
   `pointer.rs:690 focus_column_at` are the wheel-scroll entry point into the camera. `pointer.rs`
   is **729 lines with zero `#[test]`**. There is **no property test** that a sequence of wheel
   notches over a ribbon of N columns lands the camera on a valid, in-range target — or, more
   importantly, that repeated scrolling does not accumulate error.
2. **Scroll accumulation / drift.** The only test in the repo whose *name* is about accumulation
   of a projection is `src/backend/x11/compositor_gl.rs:4944
   visual_x_projection_preserves_fraction_and_is_not_accumulated` — **which dies with the
   compositor.** The non-compositor analogue (`maverick-core/tests/animation_props.rs:604
   spring_smooth_converges_without_poisoning_or_overshooting`, `src/core/layout.rs:2060
   arrange_is_idempotent_over_a_reused_buffer`) covers the spring and the output buffer, but
   **nothing asserts that `camera.target -> ideal_scroll -> arrange` is exact over many steps.**
   If the projection rounds per frame in the non-composited path, no test in the surviving set
   catches it.
3. **`input.rs` (446 lines, 0 tests), `manage.rs` (1367, 0), `events.rs` (1097, 0),
   `actions.rs` (551, 0)** — roughly **3,461 lines** of the window manager's actual event surface
   with no unit tests. `manage.rs` is where the floats/transient/EWMH-state decisions named by
   60+ `src/core/tests.rs` cases are actually performed.

### 6.2 Reconciler convergence

Well covered — 8 of the 38 `reconciler.rs` tests are proptests and three of those
(`a_clamped_geometry_still_converges_in_one_request`, `a_duplicate_desired_entry_still_converges`,
`a_stale_echo_is_repaired_and_the_repair_is_bounded`) are explicitly convergence claims. **This is
the best-tested seam in the repository.** The only hole: convergence is tested against
`DesiredState` built from `State`, never against `DesiredState` mutated by a *live* `ConfigureNotify`
mid-cycle — `observe_records_real_geometry_without_emitting` (`:469`) covers one echo, not a
storm.

### 6.3 The `--no-default-features` build path is not tested by CI

`ci.yml:50-56` runs `cargo build --release --no-default-features -p maverick -p maverick-sys` and
then `python3 tests/xvfb-stacking.py`. It **builds** the non-compositor configuration and then runs
**one** Python script against it. No `cargo test --no-default-features` exists anywhere in the repo
or in CI. The 518 tests that configuration would run are never run by automation.
`--no-default-features` is also not applied to the `clippy` step (`ci.yml:24`), so lint coverage of
that configuration is absent too.

### 6.4 `maverick-vk` — dead weight, measured

| metric | value | evidence |
|---|---:|---|
| source lines | 3489 | `find maverick-vk -name '*.rs' | xargs wc -l` |
| tests carried | 36 | `maverick_vk-… --list` -> 8 unit + 14 + 12 + 2 |
| of which permanently skipped | 2 | `#[ignore]` at `tests/smoke.rs:37,101` |
| reverse dependencies | 0 | `cargo tree -i maverick-vk` errors; no crate depends on it |
| feature selecting it | 0 | `compositor-vulkan = []` selects nothing (`Cargo.toml:99`) |

The 34 that do run test Vulkan capability selection, extent clamping, image-count capping, present
-mode preference and fence pacing — none of which any shipped binary can reach.

### 6.5 Other gaps worth naming

- **Mutation testing shows the settle predicate is still under-covered.** `mutants.out/missed.txt`
  lists **22 uncaught mutants**, 11 in `Camera::needs_update` (`maverick-core/src/types.rs:537-541`)
  and 11 in `Workspace::cleanup_empty_columns` (`types.rs:892-908`) — replacing `||` with `&&`,
  `>` with `>=`/`<`, `-` with `+`/`/`. A prior run (`mutants.out.old/missed.txt`, 11 missed vs 16
  caught) targeted the same two functions. The git history shows this area being chased
  (`6a32ec1 test(core): close the four surviving mutants at the camera's settle predicate`,
  `3a5733d fix(core): make add/remove of a column symmetric for the focus pointer`), so the misses
  are either post-fix or from a run that predates them. `[UNVERIFIED]` — I did not re-run
  `cargo mutants`.
- **Zero coverage of `src/backend/renderer.rs`**, the declared-but-unspanned renderer seam.

---

## 7. Proposed minimal test surface for a non-composited WM

Target configuration: `maverick-core` + `maverick-x11` + `maverick-toml` + `maverick-sys`
(+ `maverick-img`, which the feh-style rootwall path needs) + the root binary.

### 7.1 Before / after

| | before | after | delta |
|---|---:|---:|---:|
| `#[test]` attributes in source | 1094 | **822** | −272 |
| executable test functions (distinct names) | 1090 | **818** | −272 |
| Rust test binaries (cargo targets) | 36 | 25 | −11 |
| crates | 8 (+ root) | 5 (+ root) | −3 |
| C harness sources | 13 | 13 | 0 |
| shell suites (excl. `common.sh`) | 26 | 19 | −7 (5 deleted, 2 archived) |
| python suites | 4 | 4 | 0 |
| proptest bodies | 234 | 234 | 0 |
| proptest regression seed files | 4 | 3 | −1 |
| `[ignore]`d tests | 2 | 0 | −2 |
| tests pinning an implementation detail | 31 | 5 (2 more REWRITE) | −24 |

`822 = 815 KEEP + 7 REWRITE`. The 272 removals break down as
`maverick-gl` 62 + `maverick-render` 3 + `maverick-vk` 36 = **101** (whole crates)
+ `compositor_gl.rs` 74 + `compositor_policy.rs` 24 + `framesched.rs` 24 + `framebench.rs` 3 +
`compositor.rs` 4 + `render.rs` (Shape 21 + render-list 3 = 24) = **153** (compositor inside the
root crate)
+ `trace.rs` 11 + `mod.rs` 3 = **14** (diagnostic)
+ `userconfig.rs` 2 + `source_constraints.rs` 2 = **4**.
101 + 153 + 14 + 4 = 272. ✔

Two caveats on the "after" column:

- The 24-test `render.rs` Shape/render-list block is **product-dependent** (§4.6): if the
  replacement WM still reparents a decorated frame and rounds corners with X Shape, those 21 Shape
  tests survive and only the 3 render-list tests go. **818 is therefore a floor and 839 a ceiling.**
- "Rust test binaries" counts cargo targets: 4 (root) + 5 (core) + 8 (sys) + 4 (x11) + 2 (toml) +
  3 (img) + 4 (gl) + 2 (render) + 4 (vk) = 36; after: 36 − 4 − 2 − 4 − 1 (`source_constraints`) = 25.

### 7.2 RETAIN — 815 tests, unchanged

(Row totals sum to 815; `maverick-img` is counted once, `maverick-sys` includes `maverickctl`.)

| block | tests | one-line reason |
|---|---:|---|
| `src/core/tests.rs` | 167 | the Engine command/lifecycle contract; the only place tiling semantics are pinned end to end |
| `src/backend/x11/reconciler.rs` | 38 | ConfigureWindow authority, purity, convergence — the WM's wire |
| `src/core/layout.rs` | 38 | `arrange()` never produces a degenerate or off-workarea rect |
| `maverick-core/src/types.rs` + `wallpaper.rs` | 39 | column weights, workarea derivation, camera/spring convergence |
| `src/backend/x11/render.rs` (float policy) | 33 | float clamp/snap/normalise is a fixed point, so a client never chases the WM |
| `src/userconfig.rs` (22 of 27; 3 REWRITE, 2 REMOVE) | 22 | config is validated against a bad file without losing good values |
| `src/core/invariants.rs` | 24 | settled geometry is stable under every single command |
| `src/backend/x11/tests.rs` | 27 | xkb planner/dispatch agree, and the event loop bounds its own wait |
| `src/core/present.rs` | 15 | fullscreen beats maximized, per-axis maximize, overlay ordering |
| `src/main.rs` | 10 | every installed signal disposition names its consequence |
| `src/core/action.rs` | 13 | one action vocabulary, two input channels, one meaning |
| `maverick-img` (lib 10 + properties 38 + dispatch 2) | 50 | no byte string panics or mis-decodes a user's wallpaper; one fixture per supported format |
| `maverick-toml` (lib + props) | 36 | the parser is total and fused after a fault |
| `maverick-core/tests/*_props.rs` | 51 | camera, rect, reservation and state-model properties |
| `maverick-x11` (lib + 3 integration) | 17 | the shared bootstrap reports errors and never invents one |
| `maverick-sys` (all) | 199 | `maverickctl` and the control socket still work; `SA_NOCLDWAIT` still holds |
| `src/backend/x11/{struts,teardown,ewmh}.rs` | 12 | docks cannot escape the screen; shutdown removes the record; stacking list is deterministic |
| `src/config.rs` 9 + `src/core/commands.rs` 3 + `src/core/ipc.rs` (7 of 9) | 19 | window rules, the wallpaper command, IPC document shape |
| `tests/child_lifecycle.rs` | 5 | `SA_NOCLDWAIT` observed from outside the process |

### 7.3 REWRITE — 7 tests

| test | assert this instead |
|---|---|
| `tests/no_wait_in_wm.rs:148` | keep the rule; derive `WM_SOURCE_DIRS` from the *actual* `cargo tree` link set, or drop `maverick-gl/src` and `maverick-vk/src`, which are not linked into the WM at all |
| `tests/no_wait_in_wm.rs:224` | unchanged, but re-point the allow-list at the post-deletion tree |
| `src/userconfig.rs:1701 compositor_spring_aliases_agree_with_animations_table` | the `[animations]` table wins over any deprecated alias and both reach `Engine::apply_camera_cfg` |
| `src/userconfig.rs:1746 compositor_spring_aliases_validate_like_animations_table` | a rejected alias leaves the compiled spring untouched |
| `src/userconfig.rs:1794 animations_table_wins_over_deprecated_compositor_aliases` | rename to the surviving table and assert precedence by effect, not by name |
| `src/core/ipc.rs:515 the_inspect_document_reports_totals_layout_and_compositor` | `maverickctl inspect` carries totals + layout and parses back; drop the compositor field |
| `src/core/ipc.rs:592 default_backend_facts_do_not_claim_a_compositor` | fold into the above — the document must not claim a capability the binary does not have |

Two further rewrites are *recommended but not counted above* because they replace tests already in
the REMOVE column:

| removed | replace with |
|---|---|
| `tests/source_constraints.rs:119 no_production_code_writes_the_camera_target_field` | an Engine test: `unmanage` a window while the scroll spring is mid-flight; assert the camera converges on the target re-derived for the shorter ribbon — the exact case at `source_constraints.rs:100-113` |
| `tests/source_constraints.rs:162 the_monitor_change_handler_re_derives_the_camera_before_projecting` | a RANDR-monitor-change test: every workspace's camera target ends up inside the new workarea |

### 7.4 REMOVE — 272 tests

| block | tests | reason |
|---|---:|---|
| `maverick-gl` (6 files) | 62 | GLX/fbconfig/scissor/premultiply/shader; no non-composited consumer |
| `maverick-render/tests/contract_types.rs` | 3 | a trait with zero implementors; the only assertions are on a banner string |
| `maverick-vk` (5 files) | 36 | zero reverse dependencies; 2 of the 36 were already `#[ignore]`d |
| `src/backend/x11/compositor_gl.rs` | 74 | the compositor |
| `src/compositor_policy.rs` | 24 | "compose or bypass" has no answer without a compositor |
| `src/backend/x11/framesched.rs` | 24 | damage/vsync/shader-wallpaper frame pacing; replace with a plain "is the world dirty" check and one behavioural test |
| `src/backend/x11/render.rs` Shape + render-list | 24 | rounded-corner masks over a decorated frame, and the GL draw-list projection (product-dependent — keep if the new WM reparents frames) |
| `src/backend/x11/trace.rs` | 11 | `--features window-trace` buffer, off by default, never run by CI |
| `src/backend/x11/mod.rs` | 3 | asserts the exact wording of a debug string |
| `src/backend/x11/compositor.rs` | 4 | stub substep integrator, duplicated verbatim in `compositor_gl.rs` |
| `src/core/framebench.rs` | 3 | per-frame allocation counter for the compositor |
| `src/userconfig.rs` | 2 | parses `[compositor] enabled`, a key that stops existing |
| `tests/source_constraints.rs` | 2 | grep of production source; the rule is real, the mechanism is not a test |

### 7.5 Non-Rust suites

- **Delete 5 shell suites** (all GLX/Composite-dependent): `xephyr-compositor.sh`,
  `xephyr-partial.sh`, `xephyr-rounded-clip.sh`, `xephyr-wallpaper.sh`, `xephyr-bypass-fix.sh`.
- **Rewrite 3 for the surviving behaviour** (they assert compositor *and* WM facts in one script —
  split the compositor half out, keep the WM half): `xephyr-fs-stale.sh`,
  `xephyr-fs-stale-deterministic.sh`, `xephyr-real-fullscreen.sh`.
- **Archive 2** (`xephyr-restart.sh` is entirely about GLX texture rebinding;
  `xephyr-disconnect.sh` case B is a compositor GLX teardown — split out case A, which is pure WM,
  and keep that half).
- **Keep the other 16 suites unchanged**, plus `common.sh` as the shared helper: `xephyr-suite.sh`,
  `xephyr-fs-pointer.sh`, `xephyr-client-death.sh`, `xephyr-shutdown.sh`,
  `xephyr-restart-config.sh`, `xephyr-stress.sh`, `xephyr-pointer-float.sh`,
  `xephyr-ewmh-workarea.sh`, `xephyr-ewmh-extents.sh`, `xephyr-ipc-edge.sh`, `xephyr-2mon.sh`,
  `compat-matrix.sh`, `session-suite.sh`, `session-isolation.sh`, `session-security.sh`.
- **C harnesses: keep all 13.** `pxsample.c` and `damager.c` are the only two that are strictly
  compositor-oriented, and `pxsample` is reusable against a plain root window.
- **Python: keep `install-smoke.py`, `xvfb-stacking.py`, `tree_lines.py`; keep `xvfb-keyboard.py`**
  (it drives the xkb planner, which survives).

### 7.6 What must be ADDED (the minimal surface is not a subset)

The retained set proves layout, focus, presentation, float policy and the reconciler. It does not
prove the front door. Before calling 822 "minimal", add:

1. **A property test for scroll accumulation.** Generate a ribbon of N columns and a random notch
   sequence; assert `ideal_scroll(ws)` stays inside `[0, ribbon_width - workarea_width]` and that
   the camera target after k notches equals the target derived from the *current* focus, not from
   the accumulated delta. This is the property `compositor_gl.rs:4944` currently covers and will
   lose.
2. **An engine-level replacement for each `tests/source_constraints.rs` guard** (§7.3).
3. **`--no-default-features` in CI**: `cargo test --workspace --no-default-features` as a fourth CI job,
   which runs the 518 root tests that configuration compiles today, and `--no-default-features` added to the clippy step.
4. **First unit tests for `src/backend/x11/pointer.rs`** — at minimum, `focus_column_at` and
   `scroll_camera_with_wheel`, extracted to pure functions taking `(State, Cfg, x, y, detail)`.
   These two functions are 60 lines of pointer-to-camera arithmetic with zero coverage in a suite
   whose sibling modules have 38 tests each.

---

## 8. CI coverage of `--no-default-features`

| question | answer | evidence |
|---|---|---|
| Does `--no-default-features` build? | **yes** | `ci.yml:50` — `cargo build --release --no-default-features -p maverick -p maverick-sys` |
| Does it get tested? | **no** | `ci.yml:50-56` — the only follow-up step is `python3 tests/xvfb-stacking.py` |
| Does it get linted? | **no** | `ci.yml:24` — `cargo clippy --workspace --all-targets -- -D warnings`, no `--no-default-features` |
| Does it get property-tested? | **no** | the only proptest run is inside `cargo test --workspace` (default features) |
| Are the 70 `compositor_gl.rs` tests covered by any CI job? | **no** | they are in the default build, so `cargo test --workspace` runs them — but they test a subsystem that is being removed |
| Does the non-compositor configuration's 518 tests ever run in automation? | **no** | measured `--list` count; no `cargo test --no-default-features` string exists in the repo |
| Does `--no-default-features` skip `maverick-gl`'s own tests? | **no** (CI) | `maverick-gl` is a workspace member with no features; CI's `--workspace` builds and tests it regardless |

---

## 9. Evidence appendix

### 9.1 Commands run (read-only; no build artefacts left behind beyond `target/`)

| command | result |
|---|---|
| `rg -c '^\s*#\[test\]\s*$' --glob '*.rs'` | 1094 |
| `rg -n 'proptest!\s*[{(]' --glob '*.rs'` | 234 bodies over 30 files (211 named + 23 closure-form) |
| `cargo test --offline --workspace -- --list` | 1090 `…: test` lines |
| per-binary `<binary> --list` | per-package counts in §1.2 |
| `cargo test --offline -p maverick --no-default-features --no-run` + `--list` | **518** vs **588** with default features; −70 = `compositor_gl.rs`, +4 = `placeholder_substep_tests` |
| `git log --oneline` | 451 commits; 30+ with `fix(x11)`/`fix(core)`/`fix(reconcile)`/`test(x11)`/`test(core)` subjects naming the defects the `audit_*` tests pin |
| `cat proptest-regressions/*/*.txt` | 4 files, 48 seeds |
| `cat mutants.out/missed.txt` | 22 uncaught mutants, all in `maverick-core/src/types.rs` |

### 9.2 Zero-test modules carrying WM behaviour

| module | lines | tests |
|---|---:|---:|
| `src/backend/x11/manage.rs` | 1367 | 0 |
| `src/backend/x11/events.rs` | 1097 | 0 |
| `src/backend/x11/pointer.rs` | 729 | 0 |
| `src/backend/x11/actions.rs` | 551 | 0 |
| `src/backend/x11/input.rs` | 446 | 0 |
| `src/core/engine.rs` | 316 | 0 |
| `src/core/capability.rs` | 141 | 0 |
| `src/core/event.rs` | 113 | 0 |
| `src/core/effect.rs` | 81 | 0 |
| `src/core/desired.rs` | 70 | 0 |
| `src/backend/renderer.rs` | — | 0 (declared seam, never spanned) |

Total: **4,911 lines of the window manager with no unit test in the file.**

### 9.3 `mutants.out/missed.txt` — uncaught mutants

```
maverick-core/src/types.rs:537:9:  replace Camera::needs_update -> bool with true
maverick-core/src/types.rs:537:12: delete ! in Camera::needs_update
maverick-core/src/types.rs:541:13: replace || with && in Camera::needs_update
maverick-core/src/types.rs:540:45: replace > with >= in Camera::needs_update
maverick-core/src/types.rs:892:27: replace - with + in Workspace::cleanup_empty_columns
maverick-core/src/types.rs:900:68: replace - with / in Workspace::cleanup_empty_columns
maverick-core/src/types.rs:908:20: replace > with == in Workspace::cleanup_empty_columns
```

The two functions are the camera settle predicate and the empty-column cleanup / focus-pointer fix —
exactly the two the git log shows being chased. See §6.5 for the `[UNVERIFIED]` caveat.

---

## 10. Out-of-scope bugs and unverified claims

### OUT OF SCOPE BUGS (found, not fixed)

1. **`tests/no_wait_in_wm.rs:57-64` lists two crates that are not linked into the window manager.**
   `WM_SOURCE_DIRS` contains `maverick-gl/src` and `maverick-vk/src`. `maverick-gl` is an optional
   dep compiled out by `--no-default-features`; `maverick-vk` has zero reverse dependencies. A
   legitimate `Command::output()` in either would be reported as "the window manager blocks on a
   child" — a false positive that would push a future author to remove working code. This is the
   same confusion `COMPOSITOR-AUDIT.md:847-848` flags.

2. **`tests/source_constraints.rs:126-137` lists `backend/x11/manage.rs` twice.** The array is
   `[manage, events, render, struts, pointer, manage, commands, layout]`. Harmless today (the
   scan collects per-file), but it reads as a typo and hides which file the author meant to add.

3. **`tests/source_constraints.rs:107` (`strip_test_module`) cuts the source at the *first*
   `#[cfg(test)]` in the file.** Any production code below an early `#[cfg(test)] mod …` becomes
   invisible to both guards — the same class of bug `no_wait_in_wm.rs:88-95` explicitly documents
   and fixes with depth tracking. `source_constraints.rs` does not do depth tracking. Today no
   `src/` file has a `#[cfg(test)]` block that precedes live production code, but the two guards now
   disagree about how to find the end of a test region.

4. **`maverick-img/tests/properties/mod.rs` is not named `main.rs`.** It is nevertheless picked up by
   cargo's integration-test auto-discovery (verified: `cargo test -p maverick-img -- --list` reports
   48 + 2). This is a cargo-version-dependent convention; if cargo's auto-discovery ever tightens to
   `tests/*/main.rs`, **38 of the strongest adversarial tests in the repo silently disappear with no
   build or test failure.** Add a `[[test]]` entry or rename to `main.rs`.

### `[UNVERIFIED]`

- `cargo test --workspace --no-default-features` was not executed. The claim that `maverick-gl`'s own
  62 tests still run under it is derived from cargo's feature-resolution semantics plus the observed
  per-package build graph, not from a run.
- The `mutants.out` results are from a previous, partial mutation session (27 mutants, one file). I
  did not re-run `cargo mutants`, so §6.5's conclusion ("the settle predicate is still
  under-covered") is a reading of a stale artefact.
- Whether the retained 815 tests actually *pass* was not verified — no full `cargo test` run was
  performed (only `--list`), to avoid a long build and `target/` lock contention with the other
  agents working in this tree.
- The §7 counts are arithmetic over §4.6's per-file verdicts; the `render.rs` Shape block (21 of the
  24) is product-dependent, so 818 is a floor and 839 the ceiling depending on whether the
  replacement WM reparents a decorated frame.
