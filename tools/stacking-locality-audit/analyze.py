#!/usr/bin/env python3
"""Time-weighted diagnostic summaries. Never pool Xvfb fixtures with physical Xorg."""
import argparse
from collections import Counter, defaultdict
import csv
import json
from pathlib import Path
from audit import digest


def intervals(rows, end_ns):
    # Zero-order hold is an estimator between observations, not proof that no
    # short-lived stack existed during the event coalescing interval.
    for i, row in enumerate(rows):
        start=row['controller_end_ns']
        end=rows[i+1]['controller_end_ns'] if i+1<len(rows) else end_ns
        if end<start:
            raise ValueError('non-monotonic capture timestamps')
        yield row,end-start


def summarize(captures):
    accum=defaultdict(lambda:dict(observed_ns=0,usable_ns=0,unknown_policy_ns=0,
                                unclassified_ns=0,no_known_tiles_ns=0,segment_ns=Counter(),nonempty_segment_ns=Counter(),unknown_bounds_ns=Counter(),
                                block_length_ns=Counter(),principal_ns=Counter(),
                                separator_window_ns=Counter(),separator_episodes=Counter(),
                                snapshots=0,configuration_transitions=0,
                                unique_sequences=set(),sessions=set(),last=None))
    sessions=[]
    source_kinds=set()
    for path in captures:
        m=json.loads((path/'manifest.json').read_text())
        if not m['complete'] or digest(path/'raw.jsonl')!=m['raw_sha256']:
            raise ValueError('capture incomplete or checksum mismatch')
        source_kinds.add(m['source_kind'])
        rows=[json.loads(s) for s in (path/'raw.jsonl').read_text().splitlines()]
        if not rows:
            raise ValueError('capture has no snapshots')
        root_changes=0;oldroot=None
        for r in rows:
            signature=tuple(w['id'] for w in r['windows'])
            root_changes+=oldroot is not None and signature!=oldroot
            oldroot=signature
        sessions.append(dict(capture=f'session-{len(sessions)+1:03d}',capture_manifest_sha256=digest(path/'manifest.json'),source_kind=m['source_kind'],duration_ns=m['duration_ns'],
                             snapshots=len(rows),root_sequence_transitions=root_changes,
                             unobserved_initial_ns=rows[0]['controller_end_ns'],
                             coherent_snapshots=sum(r['logical_bracket_stable'] for r in rows),
                             event_counts=[sum(r['event_counts'][i] for r in rows) for i in range(7)],
                             xlib_query_and_subscription_calls=sum(r['xlib_query_and_subscription_calls'] for r in rows),
                             x_errors=sum(r['x_errors'] for r in rows),
                             overflow=sum(r['overflow'] for r in rows),
                             observer_cpu_ns=rows[-1].get('observer_cpu_ns'),
                             observer_peak_rss_kib=max(r.get('observer_peak_rss_kib',0) for r in rows),
                             controller_cpu_ns=m['controller_cpu_ns'],
                             collector_child_cpu_seconds=m.get('child_cpu_seconds')))
        local_last={}
        for row,dt in intervals(rows,m['duration_ns']):
            for metric in row['metrics']:
                key=(m['source_kind'],metric['scope'],tuple(metric['world']))
                a=accum[key];a['observed_ns']+=dt;a['snapshots']+=1;a['sessions'].add(str(path))
                model=metric['conditional']
                signature=tuple(tuple(b) for b in model['blocks'])
                separators=tuple(tuple((x['id'],x['category'],x['movement']) for x in block) for block in model['separators'])
                config=(signature,separators,model['unknown'])
                new=config!=local_last.get(key)
                a['configuration_transitions']+=int(key in local_last and new)
                local_last[key]=config
                # Session-qualified pseudonyms avoid treating different sessions'
                # w000001 as one shared real window.
                a['unique_sequences'].add((str(path),config))
                if not row['membership_coverage_complete'] or not row['root_stable'] or row['x_errors'] or row['overflow']:
                    a['unclassified_ns']+=dt;continue
                a['usable_ns']+=dt
                if model['unknown']:
                    a['unknown_policy_ns']+=dt
                bounds=model['unknown_assignment_bounds']
                a['unknown_bounds_ns'][(bounds['minimum'],bounds['maximum'])]+=dt
                a['segment_ns'][model['segments']]+=dt
                if model['tiles']:
                    a['nonempty_segment_ns'][model['segments']]+=dt
                else:
                    a['no_known_tiles_ns']+=dt
                for length in model['lengths']:
                    a['block_length_ns'][length]+=dt
                if model['principal_fraction'] is not None:
                    a['principal_ns'][round(model['principal_fraction'],3)]+=dt
                for block in model['separators']:
                    for w in block:
                        category=w['category'] if w['movement']!='unknown' else 'unknown-policy:'+w['category']
                        a['separator_window_ns'][category]+=dt
                        if new:a['separator_episodes'][category]+=1
    if len(source_kinds)!=1:
        raise ValueError('refusing to pool different environment kinds')
    results=[]
    for key,a in sorted(accum.items()):
        results.append(dict(source_kind=key[0],scope=key[1],world=list(key[2]),
                            sessions=len(a['sessions']),snapshots=a['snapshots'],
                            observed_ns=a['observed_ns'],usable_ns=a['usable_ns'],
                            unknown_policy_ns=a['unknown_policy_ns'],unclassified_ns=a['unclassified_ns'],
                            configuration_transitions=a['configuration_transitions'],
                            no_known_tiles_ns=a['no_known_tiles_ns'],
                            conditional_unknown_bounds_seconds={f'{k[0]}..{k[1]}':v/1e9 for k,v in sorted(a['unknown_bounds_ns'].items())},
                            nonempty_conditional_segment_seconds={str(k):v/1e9 for k,v in sorted(a['nonempty_segment_ns'].items())},
                            unique_session_configurations=len(a['unique_sequences']),
                            conditional_segment_seconds={str(k):v/1e9 for k,v in sorted(a['segment_ns'].items())},
                            conditional_block_length_block_seconds={str(k):v/1e9 for k,v in sorted(a['block_length_ns'].items())},
                            principal_fraction_seconds={str(k):v/1e9 for k,v in sorted(a['principal_ns'].items())},
                            separator_window_seconds={k:v/1e9 for k,v in a['separator_window_ns'].items()},
                            separator_configuration_occurrences=dict(a['separator_episodes']),
                            meaning='conditional on inferred normal-tile policy; unknown cases not compatibility evidence'))
    return dict(source_kind=next(iter(source_kinds)),sessions=sessions,worlds=results,
                time_model='zero-order hold until next snapshot; transition counts are sampled lower bounds')


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('captures',type=Path,nargs='+')
    p.add_argument('--out',type=Path,required=True)
    a=p.parse_args();result=summarize(a.captures);a.out.mkdir(parents=True,exist_ok=False)
    (a.out/'summary.json').write_text(json.dumps(result,indent=2)+'\n')
    with (a.out/'segments.csv').open('w') as f:
        w=csv.writer(f);w.writerow(['source_kind','scope','monitor','workspace','conditional_S','seconds','fraction_of_usable_time','usable_seconds','unknown_policy_seconds','unclassified_seconds'])
        for r in result['worlds']:
            for k,v in r['conditional_segment_seconds'].items():
                w.writerow([r['source_kind'],r['scope'],*r['world'],k,v,v/(r['usable_ns']/1e9) if r['usable_ns'] else None,r['usable_ns']/1e9,r['unknown_policy_ns']/1e9,r['unclassified_ns']/1e9])
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    fig,axes=plt.subplots(1,3,figsize=(14,4))
    kind=result['source_kind'];label='Xvfb: validación sintética' if kind=='xvfb-functional' else kind
    selected=[r for r in result['worlds'] if r['scope']=='mapped' and r['world']==[0,0]]
    if selected:
        r=selected[0];items=r['conditional_segment_seconds'];total=r['usable_ns']/1e9
        axes[0].bar(list(items),[v/total*100 for v in items.values()],color='#3976fa')
        axes[0].set_title('Bloques inferidos, mundo [0,0]');axes[0].set_xlabel('S condicional');axes[0].set_ylabel('% tiempo con bracket utilizable')
        lengths=r['conditional_block_length_block_seconds']
        axes[1].bar(list(lengths),list(lengths.values()),color='#3976fa');axes[1].set_title('Longitud de bloques');axes[1].set_xlabel('Clientes por bloque');axes[1].set_ylabel('Bloque·segundos')
        sep=r['separator_window_seconds'];axes[2].barh(list(sep),list(sep.values()),color='#9ea8bb');axes[2].set_xlabel('Ventana separadora·segundos');axes[2].set_title('Separadores observados')
    if not selected:
        for ax in axes:
            ax.text(.5,.5,'Sin pertenencia lógica: S indeterminado',ha='center',va='center',transform=ax.transAxes);ax.set_axis_off()
    fig.suptitle(f'{label} — no describe la distribución de una sesión Xorg física')
    fig.tight_layout()
    for ext in ['png','svg','pdf']:fig.savefig(a.out/f'distribution.{ext}',dpi=170)


if __name__=='__main__':main()
