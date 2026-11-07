#!/usr/bin/env python3
"""Passive root stacking observer. Real Xorg captures must be explicitly labelled."""
import argparse
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import selectors
import signal
import resource
import subprocess
import time

ROOT = Path(__file__).resolve().parent
KINDS = ['map', 'unmap', 'configure', 'reparent', 'destroy', 'property', 'create']
CATEGORIES = ['tile', 'float', 'presentation', 'dock', 'transient', 'override_redirect',
              'external', 'unknown']


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def sanitized_tree(document):
    # Names, titles, class, process ids and status are deliberately never copied.
    worlds = []
    windows = {}
    for mon in document['monitors']:
        mi = mon['index']
        active = mon['active_ws']
        for ws in mon['workspaces']:
            key = (mi, ws['index'])
            if ws['index'] == active:
                worlds.append(dict(monitor=mi, workspace=active, camera=ws['scroll']))
            for ci, col in enumerate(ws['columns']):
                for w in col['windows']:
                    windows[w['id']] = logical_window(w, key, active, ci, 'column')
            for w in ws['floats']:
                windows[w['id']] = logical_window(w, key, active, None, 'float')
    return dict(worlds=worlds, windows=windows)


def logical_window(w, world, active, column, container):
    result = dict(world=list(world), active=world[1] == active, column=column,
                  container=container)
    for key in ['float', 'fullscreen', 'maximized', 'maximized_vert', 'maximized_horiz',
                'sticky', 'overlay', 'geom', 'desired', 'applied', 'focus']:
        result[key] = w.get(key)
    return result


class Aliases:
    def __init__(self, root):
        self.ids = {0: None, root: 'root'}
        self.serial = 0

    def get(self, xid):
        if xid not in self.ids:
            self.serial += 1
            self.ids[xid] = f'w{self.serial:06d}'
        return self.ids[xid]

    def event(self, e):
        xid = e['window']
        item = dict(kind=KINDS[e['kind']], window=self.get(xid), t_ns=e['t_ns'])
        # A resource identifier reused after DestroyNotify denotes a new window.
        if e['kind'] == 4:
            self.ids.pop(xid, None)
        return item


def classify(w, logical, coherent, managed_valid, listed):
    flags = w['flags']
    native = dict(fullscreen=bool(flags & 1), max_vert=bool(flags & 2),
                  max_horiz=bool(flags & 4), sticky=bool(flags & 8),
                  dock=bool(flags & 16), dialog=bool(flags & 32),
                  menu=bool(flags & (64 | 128 | 256)), tooltip=bool(flags & 512),
                  notification=bool(flags & 1024))
    tags = [k for k, v in native.items() if v]
    if w.get('override_redirect'):
        tags.append('override_redirect')
    if w['transient']:
        tags.append('transient')
    result = dict(tags=tags, category='unknown', confidence='unknown',
                  movement='unknown', world=None, column=None,
                  reason='insufficient_or_incoherent_metadata')
    if not w['attributes_valid'] or not w['metadata_complete']:
        return result
    if native['dock']:
        result.update(category='dock', confidence='confirmed', movement='independent',
                      reason='typed_window_type_dock')
    elif w.get('override_redirect'):
        result.update(category='override_redirect', confidence='confirmed', movement='independent',
                      reason='server_override_redirect')
    elif logical and coherent and all(isinstance(logical.get(k),bool) for k in ['float','fullscreen','maximized','sticky','overlay']):
        result.update(world=logical['world'], column=logical['column'])
        if not logical['active']:
            result.update(category='float' if logical['float'] else 'tile', confidence='confirmed', movement='independent',
                          reason='managed_inactive_world_parked')
        elif logical['fullscreen'] and logical['overlay'] and not logical['maximized']:
            result.update(category='presentation',confidence='confirmed',movement='independent',
                          reason='exclusive_fullscreen_overlay_owner_in_current_source')
        elif logical['overlay'] or logical['maximized']:
            # A non-presented maximized tile can still scroll. Until the IPC
            # exposes all presentation policy, it is an explicit exception.
            result.update(category='presentation', confidence='inferred', movement='unknown',
                          reason='presentation_requires_policy_not_exposed_by_tree')
        elif logical['fullscreen']:
            result.update(category='presentation', confidence='inferred', movement='unknown',
                          reason='fullscreen_ribbon_vs_exclusive_or_sibling_hiding')
        elif logical['container'] == 'float' and logical['float']:
            result.update(category='transient' if w['transient'] or native['dialog'] else 'float',
                          confidence='confirmed', movement='independent',
                          reason='active_workspace_float_projection_is_screen_anchored')
        elif logical['container'] == 'column' and logical['float'] is False:
            if logical['sticky'] or w['transient'] or native['dialog'] or native['menu']:
                result.update(category='transient' if w['transient'] or native['dialog'] else 'tile',
                              confidence='inferred', reason='additional_relationship_requires_review')
            else:
                result.update(category='tile', confidence='inferred', movement='tile',
                              reason='active_column_membership_policy_inferred_not_observed_translation')
    elif w['transient'] or native['dialog']:
        result.update(category='transient', confidence='confirmed', movement='unknown',
                      reason='transient_relation_known_movement_unknown')
    elif managed_valid and not listed:
        result.update(category='external', confidence='inferred', movement='unknown',
                      reason='unlisted_root_window_could_be_external_or_wm_auxiliary')
    return result


def segment_bounds(sequence, world, confirmed_only=False):
    states = {0: (0, 0)}
    for w in sequence:
        c = w['classification']
        if c['movement'] == 'unknown' or (confirmed_only and c['movement']=='tile' and c['world']==world and c['confidence']!='confirmed'):
            options = [0,1]
        else:
            options = [int(c['movement']=='tile' and c['world']==world)]
        next_states = {}
        for old,(low,high) in states.items():
            for new in options:
                inc = int(new and not old)
                lo,hi = next_states.get(new,(len(sequence)+1,-1))
                next_states[new]=(min(lo,low+inc),max(hi,high+inc))
        states = next_states
    return dict(minimum=min(x[0] for x in states.values()), maximum=max(x[1] for x in states.values()))


def blocks(sequence, world, confirmed_only=False):
    # Unknowns are barriers, never evidence of compatibility. The conditional
    # model assumes eligible tiles of THIS world share the same transform.
    runs = []
    separators = []
    unknown = []
    run = []
    waiting = []
    for w in sequence:
        c = w['classification']
        tile = (c['movement'] == 'tile' and c['world'] == world and
                (not confirmed_only or c['confidence'] == 'confirmed'))
        if tile:
            if not run and runs:
                separators.append(list(waiting))
            waiting = []
            run.append(w['id'])
        else:
            if run:
                runs.append(run)
                run = []
            waiting.append(dict(id=w['id'], category=c['category'], confidence=c['confidence'],
                                movement=c['movement']))
            if c['movement'] == 'unknown' or (confirmed_only and c['movement']=='tile' and c['world']==world and c['confidence']!='confirmed'):
                unknown.append(w['id'])
    if run:
        runs.append(run)
    lengths = [len(x) for x in runs]
    n = sum(lengths)
    main = max(lengths, default=0)
    grouped = sum(x for x in lengths if x > 1)
    return dict(tiles=n, non_tiles=len(sequence)-n, unknown=len(unknown), unknown_ids=unknown,
                segments=len(runs), unknown_assignment_bounds=segment_bounds(sequence,world,confirmed_only), interruptions=max(len(runs)-1, 0), blocks=runs,
                lengths=lengths, maximum_block=main,
                principal_fraction=main/n if n else None,
                groupable_tile_fraction=grouped/n if n else None,
                groupable_all_windows_fraction=grouped/len(sequence) if sequence else None,
                separators=separators,
                stacking_model_fully_classified=not unknown,
                actual_architecture_compatibility_proved=False,
                interpretation='conditional stacking-only model; geometry/layer compatibility not proved')


def transform(snapshot, logical, coherent, aliases):
    events = [aliases.event(e) for e in snapshot['events']]
    managed = set(snapshot['managed'])
    windows = []
    for w in snapshot['windows']:
        item = dict(w)
        xid = item['id']
        item['id'] = aliases.get(xid)
        item['transient'] = aliases.get(w['transient'])
        item['listed'] = xid in managed
        l = logical['windows'].get(xid) if logical else None
        item['classification'] = classify(w, l, coherent, snapshot['managed_valid'], xid in managed)
        # Persist only the minimum logical membership/presentation facts.
        if l is not None:
            item['logical'] = l
        windows.append(item)
    worlds = logical['worlds'] if logical else []
    metrics = []
    for world in worlds:
        key = [world['monitor'], world['workspace']]
        for scope in ['root_structural', 'mapped']:
            sequence = windows if scope == 'root_structural' else [w for w in windows if w.get('map_state') == 2]
            metrics.append(dict(world=key, scope=scope,
                                conditional=blocks(sequence, key),
                                confirmed_only=blocks(sequence, key, confirmed_only=True)))
    out = {k: v for k, v in snapshot.items() if k not in ['windows', 'events', 'managed', 'published_stack']}
    out.update(windows=windows, events=events, managed=[aliases.get(x) for x in snapshot['managed']],
               published_stack=[aliases.get(x) for x in snapshot['published_stack']],
               worlds=worlds, metrics=metrics, logical_bracket_stable=coherent)
    missing = managed - {w['id'] for w in snapshot['windows']}
    out['managed_not_root_children'] = [aliases.get(x) for x in sorted(missing)]
    out['root_attribute_unknown_count']=sum(not w['attributes_valid'] for w in windows)
    out['membership_coverage_complete']=coherent and not missing and not out['root_attribute_unknown_count']
    # EWMH is a subset publication. Root OR/auxiliary windows remain in windows.
    root_managed = [w['id'] for w in windows if w['listed']]
    managed_aliases=set(out['managed'])
    published_managed=[w for w in out['published_stack'] if w in managed_aliases]
    out['published_managed_order_agrees'] = root_managed == published_managed if snapshot['stacking_valid'] else None
    published_set=set(out['published_stack'])
    out['published_order_is_root_subsequence'] = [w['id'] for w in windows if w['id'] in published_set] == out['published_stack'] if snapshot['stacking_valid'] else None
    return out


def tree_query(args):
    if args.instance is None:
        return None, 'disabled'
    try:
        r = subprocess.run([str(args.ctl), 'query', 'tree', '--name', args.instance],
                           capture_output=True, timeout=1, check=False)
        if r.returncode or len(r.stdout) > 4*1024*1024:
            return None, 'ipc_failed'
        return sanitized_tree(json.loads(r.stdout)), None
    except (OSError, subprocess.TimeoutExpired, ValueError, KeyError, TypeError):
        return None, 'ipc_failed_or_schema_unknown'


class Reader:
    def __init__(self, pipe):
        self.pipe = pipe
        self.buf = bytearray()
        self.selector = selectors.DefaultSelector()
        self.selector.register(pipe, selectors.EVENT_READ)

    def line(self, timeout):
        end = time.monotonic() + timeout
        while b'\n' not in self.buf:
            if not self.selector.select(max(0, end-time.monotonic())):
                return None
            data = os.read(self.pipe.fileno(), 65536)
            if not data:
                raise RuntimeError('observer closed its output')
            self.buf.extend(data)
            if len(self.buf) > 16*1024*1024:
                raise RuntimeError('observer response exceeds limit')
        line, _, rest = self.buf.partition(b'\n')
        self.buf = bytearray(rest)
        return json.loads(line)

    def close(self):
        self.selector.close()


def collect(args):
    os.umask(0o077)
    args.out.mkdir(parents=True, exist_ok=False)
    manifest = dict(complete=False, source_kind=args.source_kind, requested_seconds=args.seconds,
                    coalescing_ms=args.interval_ms, heartbeat_seconds=args.heartbeat,
                    physical_backend_verification='source-kind is declared by operator, not proven by X11 vendor',
                    source_hashes={p.name: digest(p) for p in ROOT.glob('*') if p.is_file()},
                    observer_hash=digest(args.observer), ctl_hash=digest(args.ctl) if args.instance else None,
                    identity='pseudonyms local to this capture; XIDs/titles/classes/PIDs not retained',
                    intervals='event coalescing; snapshots are non-atomic observed brackets',
                    ipc_mode='explicit named instance read-only query tree twice per snapshot' if args.instance else 'no IPC',
                    started_utc=time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()))
    (args.out/'manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')
    stop_requested = False

    def stop_signal(_sig, _frame):
        nonlocal stop_requested
        stop_requested = True

    old = {s: signal.signal(s, stop_signal) for s in [signal.SIGTERM, signal.SIGINT]}
    observer = None
    reader = None
    try:
        with (args.out/'observer.stderr').open('wb') as error:
            observer = subprocess.Popen([str(args.observer), str(args.interval_ms)],
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=error, bufsize=0)
            reader = Reader(observer.stdout)
            ready = reader.line(5)
            if not ready or not ready.get('ready'):
                raise RuntimeError('observer did not start')
            aliases = Aliases(ready['root'])
            start = time.monotonic_ns()
            deadline = start + int(args.seconds*1e9)
            next_heartbeat = start
            pending = True
            rows = 0
            cpu0 = time.process_time_ns()
            children0=resource.getrusage(resource.RUSAGE_CHILDREN)
            with (args.out/'raw.jsonl').open('w') as raw:
                while not stop_requested and time.monotonic_ns() < deadline:
                    now = time.monotonic_ns()
                    if pending or now >= next_heartbeat:
                        before, failure0 = tree_query(args)
                        send_start = time.monotonic_ns()
                        refresh = now >= next_heartbeat
                        observer.stdin.write(b'refresh\n' if refresh else b'capture\n')
                        observer.stdin.flush()
                        while True:
                            snapshot = reader.line(5)
                            if snapshot is None:
                                raise RuntimeError('snapshot timeout')
                            if snapshot.get('snapshot'):
                                break
                        after, failure1 = tree_query(args)
                        coherent = (before is not None and before == after and snapshot['managed_valid']
                                    and set(before['windows']) == set(snapshot['managed'])
                                    and snapshot['root_stable'] and not snapshot['x_errors'] and not snapshot['overflow'])
                        row = transform(snapshot, after, coherent, aliases)
                        row.update(source_kind=args.source_kind, capture_index=rows,
                                   controller_start_ns=send_start-start,
                                   controller_end_ns=time.monotonic_ns()-start,
                                   logical_query_error=failure0 or failure1)
                        raw.write(json.dumps(row, separators=(',', ':'))+'\n')
                        raw.flush()
                        rows += 1
                        pending = False
                        if refresh:
                            next_heartbeat = time.monotonic_ns() + int(args.heartbeat*1e9)
                    else:
                        timeout = min((next_heartbeat-now)/1e9, (deadline-now)/1e9, .5)
                        notice = reader.line(max(0, timeout))
                        pending = bool(notice and notice.get('notice'))
            observer.stdin.write(b'quit\n');observer.stdin.flush()
            if observer.wait(3):
                raise RuntimeError('observer exit failure')
            children1=resource.getrusage(resource.RUSAGE_CHILDREN)
            manifest.update(complete=True, snapshots=rows,
                            child_cpu_seconds=(children1.ru_utime+children1.ru_stime-children0.ru_utime-children0.ru_stime),
                            duration_ns=time.monotonic_ns()-start,
                            controller_cpu_ns=time.process_time_ns()-cpu0,
                            interrupted=stop_requested, raw_sha256=digest(args.out/'raw.jsonl'))
    finally:
        if observer and observer.poll() is None:
            observer.terminate()
            try:
                observer.wait(3)
            except subprocess.TimeoutExpired:
                observer.kill();observer.wait(3)
        if reader:
            reader.close()
        for s, handler in old.items():
            signal.signal(s, handler)
        (args.out/'manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--observer', type=Path, required=True)
    p.add_argument('--ctl', type=Path, default=Path('maverickctl'))
    p.add_argument('--instance', help='explicit Maverick --name; without it managed movement remains unknown')
    p.add_argument('--source-kind', choices=['xorg-physical', 'xvfb-functional', 'xephyr-functional', 'x11-unconfirmed'], required=True)
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--seconds', type=float, default=600)
    p.add_argument('--interval-ms', type=int, default=250)
    p.add_argument('--heartbeat', type=float, default=5)
    a = p.parse_args()
    if not math.isfinite(a.seconds) or not math.isfinite(a.heartbeat) or a.seconds <= 0 or not 100 <= a.interval_ms <= 10000 or a.heartbeat < 2:
        p.error('positive duration, coalescing 100..10000 ms, heartbeat >=2 s required')
    a.observer = a.observer.resolve()
    if a.instance:
        import shutil
        found = shutil.which(str(a.ctl))
        if found is None:
            p.error('maverickctl unavailable')
        a.ctl = Path(found).resolve()
    collect(a)


if __name__ == '__main__':
    main()
