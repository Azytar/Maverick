#!/usr/bin/env python3
"""Owned Xvfb controls; these are functional tests, never real-session statistics."""
import argparse
import ctypes as C
import json
import random
import os
import resource
from pathlib import Path
import subprocess
import tempfile
import time
from audit import Reader, digest

ROOT=Path(__file__).resolve().parent


def stop(p):
    if p and p.poll() is None:
        p.terminate()
        try:p.wait(3)
        except subprocess.TimeoutExpired:p.kill();p.wait(3)


def reply(p, reader, text, field=None):
    p.stdin.write((text+'\n').encode());p.stdin.flush()
    while True:
        r=reader.line(5)
        if r is None:raise RuntimeError('child timeout')
        if field is None or field in r:return r


def build(out):
    common=['cc','-std=c11','-O2','-Wall','-Wextra','-Werror','-Wshadow','-Wformat=2']
    commands=[common+[str(ROOT/'observer.c'),'-o',str(out/'observer'),'-lX11'],
              common+['-DAUDIT_REVERSE_ROOT',str(ROOT/'observer.c'),'-o',str(out/'observer-reversed'),'-lX11'],
              common+[str(ROOT/'fixture.c'),'-o',str(out/'fixture'),'-lX11'],
              common+[str(ROOT/'guard-negative.c'),'-o',str(out/'guard-negative'),'-lX11'],
              common+['-Wno-unused-parameter','-fPIC','-shared',str(ROOT/'guard.c'),'-o',str(out/'guard.so'),'-ldl']]
    for i,c in enumerate(commands):
        r=subprocess.run(c,capture_output=True,text=True);(out/f'build-{i}.log').write_text(r.stdout+r.stderr)
        if r.returncode:raise RuntimeError('build failure')


def root_contract(args, env, out, fixture, fr, observer_name, expected_order):
    p=None;reader=None;result={}
    with (out/f'{observer_name}.stderr').open('wb') as err:
        try:
            guarded=dict(env,LD_PRELOAD=str(out/'guard.so'))
            p=subprocess.Popen([str(out/observer_name),'100'],env=guarded,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=err,bufsize=0)
            reader=Reader(p.stdout);assert reader.line(5)['ready']
            s=reply(p,reader,'refresh','snapshot')
            ids=[w['id'] for w in s['windows'] if w['id'] in expected_order]
            result['root_order_bottom_to_top']=ids==expected_order
            if observer_name.endswith('reversed'):
                if result['root_order_bottom_to_top']:raise AssertionError('reversed negative control survived')
                result['expected_named_failure']='root_order_bottom_to_top'
                return result
            assert result['root_order_bottom_to_top']
            reply(fixture,fr,'or');s=reply(p,reader,'refresh','snapshot')
            result['map_events']=s['event_counts'][0]>0
            external=next(w for w in s['windows'] if w.get('override_redirect'))['id']
            reply(fixture,fr,'between');s=reply(p,reader,'capture','snapshot')
            order=[w['id'] for w in s['windows']]
            result['configure_event']=s['event_counts'][2]>0
            result['external_interleaving']=order.index(expected_order[0])<order.index(external)<order.index(expected_order[1])
            reply(fixture,fr,'unmap');s=reply(p,reader,'capture','snapshot')
            result['unmap_event']=s['event_counts'][1]>0
            reply(fixture,fr,'remap');reply(p,reader,'capture','snapshot')
            reply(fixture,fr,'bad-state');s=reply(p,reader,'refresh','snapshot')
            result['malformed_not_valid_empty']=not next(w for w in s['windows'] if w['id']==expected_order[0])['metadata_complete']
            reply(fixture,fr,'good-state');reply(p,reader,'refresh','snapshot')
            reply(fixture,fr,'reparent');s=reply(p,reader,'capture','snapshot')
            result['reparent_event']=s['event_counts'][3]>0
            result['reparented_missing_from_root']=external not in [w['id'] for w in s['windows']]
            reply(fixture,fr,'root-again');reply(p,reader,'capture','snapshot')
            reply(fixture,fr,'destroy');s=reply(p,reader,'capture','snapshot')
            result['destroy_event']=s['event_counts'][4]>0
            result['destroyed_missing']=expected_order[4] not in [w['id'] for w in s['windows']]
            assert all(result.values()),result
            p.stdin.write(b'quit\n');p.stdin.flush();assert p.wait(3)==0
            result['passivity_guard_clean']=True
            return result
        finally:
            stop(p)
            if reader:reader.close()


def native_contract(args,out):
    server=fixture=None
    with tempfile.TemporaryDirectory(prefix='mav-locality-native-') as tmp:
        env={k:v for k,v in os.environ.items() if k not in ('DISPLAY','XAUTHORITY','WAYLAND_DISPLAY') and not k.startswith('MAVERICK_')}
        env['XDG_RUNTIME_DIR']=tmp
        with (out/'native-server.log').open('wb') as sl,(out/'native-fixture.stderr').open('wb') as fl:
            sr=fr=None
            try:
                server=subprocess.Popen(['Xvfb','-displayfd','1','-screen','0','1280x800x24','-nolisten','tcp'],env=env,stdout=subprocess.PIPE,stderr=sl,bufsize=0)
                import select
                assert select.select([server.stdout],[],[],5)[0]
                display=server.stdout.readline().decode().strip();assert display.isdecimal();env['DISPLAY']=':'+display
                fixture=subprocess.Popen([str(out/'fixture')],env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=fl,bufsize=0)
                fr=Reader(fixture.stdout);assert fr.line(5)['fixture_ready']
                ids=reply(fixture,fr,'tiles')['tiles'];reply(fixture,fr,'order')
                r=subprocess.run([str(out/'guard-negative')],env=dict(env,LD_PRELOAD=str(out/'guard.so')),capture_output=True)
                assert r.returncode==-6 and b'PASSIVITY GUARD REJECTED' in r.stderr
                results={'guard_negative':{'runtime_rejection':True,'expected_signal':'SIGABRT'}}
                # Run the reversed control before lifecycle destroys a fixture window.
                results['reversed_control']=root_contract(args,env,out,fixture,fr,'observer-reversed',ids)
                results['native']=root_contract(args,env,out,fixture,fr,'observer',ids)
                (out/'native-contract.json').write_text(json.dumps(results,indent=2)+'\n')
            finally:
                stop(fixture);stop(server)
                if fr:fr.close()


LIBC=C.CDLL(None,use_errno=True)
LIBC.clock_getcpuclockid.argtypes=[C.c_int,C.POINTER(C.c_int)]
LIBC.clock_gettime.argtypes=[C.c_int,C.c_void_p]
class Timespec(C.Structure):
    _fields_=[('sec',C.c_long),('ns',C.c_long)]
def remote_clock(pid):
    clock=C.c_int()
    if LIBC.clock_getcpuclockid(pid,C.byref(clock)):raise RuntimeError('CPU clock unavailable')
    return clock.value
def remote_cpu(clock):
    t=Timespec()
    if LIBC.clock_gettime(clock,C.byref(t)):raise RuntimeError('CPU read failed')
    return t.sec*10**9+t.ns
def resident(pid):
    return int(Path(f'/proc/{pid}/statm').read_text().split()[1])*os.sysconf('SC_PAGE_SIZE')


def ctl(args, env, action, instance=None):
    command=[str(args.ctl),'msg',action]
    if instance:command+=['--name',instance]
    r=subprocess.run(command,env=env,capture_output=True,text=True,timeout=3)
    if r.returncode:raise RuntimeError('fixture WM action failed')


def session(args,out,index,observe=True):
    processes=[];fixture=None;fr=None
    directory=out/f'session-{index:02d}'
    directory.mkdir()
    stages=[]
    with tempfile.TemporaryDirectory(prefix='mav-locality-wm-') as tmp:
        env={k:v for k,v in os.environ.items() if k not in ('DISPLAY','XAUTHORITY','WAYLAND_DISPLAY','DBUS_SESSION_BUS_ADDRESS') and not k.startswith('MAVERICK_')}
        for k in ['HOME','XDG_RUNTIME_DIR','XDG_CONFIG_HOME','XDG_STATE_HOME','XDG_CACHE_HOME']:
            p=Path(tmp)/k;p.mkdir(mode=0o700);env[k]=str(p)
        env['DBUS_SESSION_BUS_ADDRESS']='unix:path=/nonexistent/locality-audit'
        cfg=directory/'config.toml';cfg.write_text('[general]\nfocus_mouse=false\nwarp_cursor=false\n[autostart]\ncommands=[]\n[[rules]]\nclass="LocalityFloat"\nfloat=true\n[[rules]]\nclass="LocalityExclusive"\ntrue_fullscreen=true\n')
        logs=[]
        try:
            for name in ['server','wm','fixture','collector']:
                logs.append((directory/f'{name}.log').open('wb'))
            server=subprocess.Popen(['Xvfb','-displayfd','1','-screen','0','1280x800x24','-nolisten','tcp'],env=env,stdout=subprocess.PIPE,stderr=logs[0],bufsize=0);processes.append(server)
            import select
            assert select.select([server.stdout],[],[],5)[0]
            display=server.stdout.readline().decode().strip();assert display.isdecimal();env['DISPLAY']=':'+display
            wm=subprocess.Popen([str(args.wm),'--config',str(cfg)],env=env,stdout=logs[1],stderr=logs[1]);processes.append(wm)
            # Resolve only the instance created inside this private runtime/display.
            instance=None;end=time.monotonic()+5
            while time.monotonic()<end:
                r=subprocess.run([str(args.ctl),'query','tree'],env=env,capture_output=True,text=True,timeout=2)
                if not r.returncode:
                    doc=json.loads(r.stdout)
                    metas=list(Path(env['XDG_RUNTIME_DIR']).rglob('*.json'))
                    for path in metas:
                        try:
                            meta=json.loads(path.read_text())
                            for key in ['session_id','id','name']:
                                if isinstance(meta.get(key),str):instance=meta[key];break
                        except (ValueError,OSError):pass
                        if instance:break
                    break
                time.sleep(.03)
            if r.returncode:raise RuntimeError('private WM unavailable')
            # The identity layout is documented; using its generated socket basename
            # avoids auto-selecting another instance even in the private runtime.
            if instance is None:
                socks=list(Path(env['XDG_RUNTIME_DIR']).rglob('control.sock'))
                if len(socks)==1:instance=socks[0].parent.name
            if instance is None:raise RuntimeError('private WM identity not found')
            fixture=subprocess.Popen([str(out/'fixture')],env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=logs[2],bufsize=0);processes.append(fixture)
            fr=Reader(fixture.stdout);assert fr.line(5)['fixture_ready']
            collector=None
            if observe:
                guardenv=dict(env,LD_PRELOAD=str(out/'guard.so'))
                collector=subprocess.Popen(['python3',str(ROOT/'audit.py'),'--observer',str(out/'observer'),'--ctl',str(args.ctl),'--instance',instance,'--source-kind','xvfb-functional','--out',str(directory/'capture'),'--seconds','60','--interval-ms','250','--heartbeat','2'],env=guardenv,stdout=logs[3],stderr=logs[3]);processes.append(collector)
                end=time.monotonic()+5
                while not (directory/'capture/raw.jsonl').exists():
                    if collector.poll() is not None or time.monotonic()>end:raise RuntimeError('collector startup failed')
                    time.sleep(.02)
            clocks=[remote_clock(wm.pid),remote_clock(server.pid)]
            cpu_before=[remote_cpu(c) for c in clocks]
            begin=time.monotonic_ns()
            actions=[('tiles',None),('float',None),('dialog',None),('or',None),('between',None),('or-top',None),
                     (None,'focus:left'),(None,'focus:right'),('activate',None),('fullscreen',None),(None,'focus:right'),('restore',None),
                     ('maximize',None),('unmaximize',None),('exclusive',None),('exclusive-restore',None),('dock',None),(None,'view:2'),(None,'view:1'),
                     ('unmap',None),('remap',None),('destroy',None)]
            for fixture_action,wm_action in actions:
                stamp=time.monotonic_ns()-begin
                if fixture_action:reply(fixture,fr,fixture_action)
                else:ctl(args,env,wm_action,instance)
                stages.append(dict(label=fixture_action or wm_action,start_ns=stamp))
                time.sleep(.6)
            time.sleep(2)
            cpu_after=[remote_cpu(c) for c in clocks]
            telemetry=dict(observed=observe,wm_cpu_ns=cpu_after[0]-cpu_before[0],xvfb_cpu_ns=cpu_after[1]-cpu_before[1],wall_ns=time.monotonic_ns()-begin,wm_rss=resident(wm.pid),xvfb_rss=resident(server.pid))
            (directory/'telemetry.json').write_text(json.dumps(telemetry,indent=2)+'\n')
            if collector:
                collector.terminate();assert collector.wait(5)==0
                m=json.loads((directory/'capture/manifest.json').read_text());assert m['complete']
                rows=[json.loads(s) for s in (directory/'capture/raw.jsonl').read_text().splitlines()]
                coherent=sum(r['logical_bracket_stable'] for r in rows)
                assert coherent>0,'no IPC/root coherent samples'
                assert any(w['classification']['movement']=='tile' for r in rows for w in r['windows']),'no classified active tiles'
                assert any(w['classification']['category']=='float' for r in rows for w in r['windows']),'float not classified'
                assert any(w['classification']['category']=='dock' for r in rows for w in r['windows']),'dock not observed'
                assert any(w['classification']['category']=='transient' for r in rows for w in r['windows']),'transient not observed'
                assert any(w.get('logical',{}).get('fullscreen') and w.get('logical',{}).get('overlay') for r in rows for w in r['windows']),'exclusive fullscreen not observed'
                assert any(w.get('logical',{}).get('fullscreen') and not w.get('logical',{}).get('overlay') for r in rows for w in r['windows']),'ribbon fullscreen not observed'
                assert any(w.get('logical',{}).get('maximized') for r in rows for w in r['windows']),'maximize not observed'
                assert any(x['conditional']['segments']>=2 for r in rows for x in r['metrics'] if x['scope']=='mapped'),'interleaving not observed'
                encoded=(directory/'capture/raw.jsonl').read_text()
                assert 'LOCALITY_PRIVATE' not in encoded and 'Locality' not in encoded
            (directory/'stages.json').write_text(json.dumps(stages,indent=2)+'\n')
        finally:
            for p in reversed(processes):stop(p)
            if fr:fr.close()
            for f in logs:f.close()


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--out',type=Path,required=True)
    p.add_argument('--wm',type=Path,required=True)
    p.add_argument('--ctl',type=Path,required=True)
    p.add_argument('--sessions',type=int,default=3)
    a=p.parse_args();resource.setrlimit(resource.RLIMIT_CORE,(0,0));a.out=a.out.resolve();a.wm=a.wm.resolve();a.ctl=a.ctl.resolve()
    repo=ROOT.parents[1]
    if a.out==repo or repo in a.out.parents:p.error('test output must be outside the repository')
    if a.sessions<1 or a.sessions>10:p.error('sessions must be 1..10')
    a.out.mkdir(parents=True,exist_ok=False)
    manifest=dict(complete=False,environment='Xvfb functional fixtures only',wm_sha256=digest(a.wm),ctl_sha256=digest(a.ctl))
    (a.out/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    build(a.out);native_contract(a,a.out)
    schedule=[];rng=random.Random(20261027)
    for block in range(a.sessions):
        variants=[False,True];rng.shuffle(variants)
        for observed in variants:
            schedule.append(dict(block=block,observed=observed,index=len(schedule)))
    manifest['schedule']=schedule
    (a.out/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    for item in schedule:session(a,a.out,item['index'],item['observed'])
    manifest.update(complete=True,sessions=a.sessions)
    (a.out/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')


if __name__=='__main__':main()
