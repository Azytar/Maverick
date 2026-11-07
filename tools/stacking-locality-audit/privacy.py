#!/usr/bin/env python3
"""Fail closed on raw capture schema or non-pseudonymous identifiers."""
import hashlib
import json
from pathlib import Path
import re

KEYS = set('''actual_architecture_compatibility_proved attributes_valid blocks camera
capture_index category classification column conditional confidence confirmed_only
controller_end_ns controller_start_ns event_counts events flags geom
groupable_all_windows_fraction groupable_tile_fraction id input_only interpretation
interruptions lengths listed logical logical_bracket_stable logical_query_error managed
managed_not_root_children managed_valid map_state maximum maximum_block
membership_coverage_complete metadata_complete metrics minimum monitor movement
non_tiles observer_cpu_ns observer_peak_rss_kib overflow override_redirect
principal_fraction published_managed_order_agrees published_order_is_root_subsequence
published_stack reason root_attribute_unknown_count root_stable scope segments separators
snapshot source_kind stacking_model_fully_classified stacking_valid t_end_ns t_start_ns
tags tiles transient unknown unknown_assignment_bounds unknown_ids windows workspace
world worlds x_errors xlib_query_and_subscription_calls float fullscreen maximized
maximized_vert maximized_horiz sticky overlay desired applied focus active container
kind window t_ns'''.split())
STRINGS = set('''tile float presentation dock transient override_redirect external unknown
confirmed inferred independent column root_structural mapped map unmap configure reparent
destroy property create fullscreen max_vert max_horiz sticky dialog menu tooltip
notification disabled ipc_failed ipc_failed_or_schema_unknown xorg-physical
xvfb-functional xephyr-functional x11-unconfirmed'''.split())
STRINGS.update([
    'insufficient_or_incoherent_metadata', 'typed_window_type_dock',
    'server_override_redirect', 'managed_inactive_world_parked',
    'exclusive_fullscreen_overlay_owner_in_current_source',
    'presentation_requires_policy_not_exposed_by_tree',
    'fullscreen_ribbon_vs_exclusive_or_sibling_hiding',
    'active_workspace_float_projection_is_screen_anchored',
    'additional_relationship_requires_review',
    'active_column_membership_policy_inferred_not_observed_translation',
    'transient_relation_known_movement_unknown',
    'unlisted_root_window_could_be_external_or_wm_auxiliary',
    'conditional stacking-only model; geometry/layer compatibility not proved',
])
IDENTIFIERS = {'id','window','transient','managed','published_stack',
               'managed_not_root_children','unknown_ids','blocks'}


def alias(value):
    if isinstance(value,list):
        for item in value:alias(item)
    elif value is not None and not (isinstance(value,str) and re.fullmatch(r'(?:root|w\d{6,})',value)):
        raise ValueError('Identificador sin seudonimizar.')


def inspect(value):
    if isinstance(value,dict):
        if set(value)-KEYS:raise ValueError('Campos desconocidos: no se certifica privacidad.')
        for key,item in value.items():
            if key in IDENTIFIERS:alias(item)
            inspect(item)
    elif isinstance(value,list):
        for item in value:inspect(item)
    elif isinstance(value,str) and value not in STRINGS and not re.fullmatch(r'(?:root|w\d{6,})',value):
        raise ValueError('Texto libre inesperado en los resultados.')


def verify(folder):
    folder=Path(folder)
    manifest=json.loads((folder/'manifest.json').read_text())
    raw=folder/'raw.jsonl'
    if manifest.get('complete') and hashlib.sha256(raw.read_bytes()).hexdigest()!=manifest['raw_sha256']:
        raise ValueError('El hash de las muestras no coincide.')
    rows=0
    with raw.open() as stream:
        for line in stream:
            inspect(json.loads(line));rows+=1
    return dict(raw_schema_and_pseudonyms_verified=True,snapshots=rows,
                complete=manifest.get('complete',False),
                scope='raw.jsonl only; review manifest, endpoint and any other files before sharing')
