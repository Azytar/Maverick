#!/usr/bin/env python3
"""Launcher contracts; physical-positive checks use simulated /proc, never a claim of hardware coverage."""
import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import select
import struct
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch, MagicMock

import launch
from privacy import inspect, verify
from audit import Reader
from validate import stop


class LauncherContracts(unittest.TestCase):
    def setUp(self):
        mock=patch('os.geteuid',return_value=1000);mock.start();self.addCleanup(mock.stop)

    def test_local_display_normalization(self):
        self.assertEqual(launch.display_key(':2'),launch.display_key('unix/:2.0'))
        for s in ('', 'localhost:2', 'host:0', ':0.bad'):
            with self.assertRaises(launch.CheckError):launch.display_key(s)

    def fake_peer(self,name='Xorg',drm=True,vt=True):
        tmp=tempfile.TemporaryDirectory();self.addCleanup(tmp.cleanup)
        proc=Path(tmp.name);p=proc/'321';p.mkdir();(p/'fd').mkdir()
        exe=proc/name;exe.write_text('fixture, never executed');(p/'exe').symlink_to(exe)
        (p/'cmdline').write_bytes(b'Xorg\0:0\0vt2\0')
        if drm:(p/'fd/3').symlink_to('/dev/dri/card0')
        if vt:(p/'fd/4').symlink_to('/dev/tty2')
        return launch.physical_from_peer(dict(local_unix=True,peer_pid=321),proc)

    def test_physical_positive_is_proc_simulation(self):
        self.assertTrue(self.fake_peer()['confirmed'])

    def test_virtual_servers_and_dummy_xorg_are_not_physical(self):
        for name in ('Xvfb','Xwayland','Xephyr'):
            self.assertFalse(self.fake_peer(name)['confirmed'])
        self.assertFalse(self.fake_peer(drm=False)['confirmed'])
        self.assertFalse(self.fake_peer(vt=False)['confirmed'])
        self.assertFalse(launch.physical_from_peer(dict(local_unix=False,peer_pid=0))['confirmed'])

    def test_tty_endpoint_must_be_fresh_and_match_vt(self):
        with tempfile.TemporaryDirectory() as tmp:
            proc=Path(tmp);p=proc/'42';p.mkdir()
            fields=['0']*20;fields[19]='100'
            (p/'stat').write_text('42 (Xorg) '+' '.join(fields))
            (p/'cmdline').write_bytes(b'Xorg\0:0\0vt2\0')
            peer=dict(local_unix=True,peer_pid=42)
            launch.fresh_tty_endpoint(peer,'2',100,proc)
            with self.assertRaises(launch.CheckError):launch.fresh_tty_endpoint(peer,'3',100,proc)
            with self.assertRaises(launch.CheckError):launch.fresh_tty_endpoint(peer,'2',102,proc)

    def test_results_cannot_go_in_repository(self):
        with self.assertRaises(launch.CheckError):launch.output_path(launch.REPO/'capture')

    def test_existing_output_is_not_overwritten(self):
        with tempfile.TemporaryDirectory() as p:
            with self.assertRaises(launch.CheckError):launch.output_path(p)

    def test_privacy_rejects_real_ids_titles_and_unknown_text(self):
        for record in (dict(id=4194304),dict(title='secret'),dict(reason='private application text')):
            with self.assertRaises(ValueError):inspect(record)
        inspect(dict(id='w000001',transient=None,category='tile'))

    def test_tty_refuses_active_graphical_environment(self):
        with patch.dict(os.environ,dict(DISPLAY=':1')):
            with self.assertRaises(launch.CheckError):launch.tty_plan(argparse.Namespace())

    def test_tty_refuses_non_terminal_input(self):
        with patch.dict(os.environ,{},clear=True),patch('os.isatty',return_value=False):
            with self.assertRaises(launch.CheckError):launch.tty_plan(argparse.Namespace())

    def test_session_failure_stops_only_its_wm(self):
        args=argparse.Namespace(wm='maverick',config=None,instance='owned')
        child=MagicMock();child.poll.return_value=None
        with patch.dict(os.environ,dict(STACK_AUDIT_TTY_CHILD='1',STACK_AUDIT_DISPLAY='1',STACK_AUDIT_VT='2',STACK_AUDIT_NOT_BEFORE='10',DISPLAY=':1')),patch.object(launch,'observer_binary',return_value=Path('/observer')),patch.object(launch,'invoke',return_value=b'{"local_unix":true,"peer_pid":42}'),patch.object(launch,'fresh_tty_endpoint'),patch.object(launch,'executable',return_value=Path('/maverick')),patch.object(launch.subprocess,'Popen',return_value=child),patch.object(launch,'check',return_value={}),patch.object(launch,'capture',side_effect=RuntimeError('controlled failure')):
            with self.assertRaisesRegex(RuntimeError,'controlled failure'):launch.session(args)
        child.terminate.assert_called_once();child.wait.assert_called_once_with(5);child.kill.assert_not_called()

    def test_tty_builds_explicit_xinit_command_and_refuses_lock(self):
        args=argparse.Namespace(native_only=False,display_number=1,wm='maverick',ctl='maverickctl',out=None,config=None,allow_unconfirmed=False,seconds=600,interval_ms=250,heartbeat=5)
        props='Type=tty\nActive=yes\nRemote=no\nSeat=seat0\nVTNr=2\nUser=1000'
        def logind(command):
            return '2 1000 user seat0 tty2' if command[0]=='list-sessions' else props
        def glob(path,pattern):
            return [Path('/dev/dri/card0')] if str(path)=='/dev/dri' else []
        with patch.dict(os.environ,dict(XDG_SESSION_ID='2'),clear=True),patch('os.getuid',return_value=1000),patch('os.isatty',return_value=True),patch('os.ttyname',return_value='/dev/tty2'),patch.object(Path,'read_text',return_value='tty2'),patch.object(Path,'glob',glob),patch('os.access',return_value=True),patch.object(launch,'logind',side_effect=logind),patch.object(launch,'executable',side_effect=lambda value:Path('/usr/bin')/value),patch.object(launch,'output_path',return_value=Path('/private/new-capture')),patch.object(Path,'exists',return_value=False):
            command=launch.tty_plan(args)
            self.assertEqual(command[-7:],['--','/usr/bin/Xorg',':1','vt2','-keeptty','-nolisten','tcp'])
            with patch.object(Path,'exists',return_value=True):
                with self.assertRaisesRegex(launch.CheckError,'ocupado'):launch.tty_plan(args)

    def test_endpoint_requires_matching_maverick_display(self):
        args=argparse.Namespace(ctl='maverickctl',instance='test',allow_unconfirmed=True)
        with patch.dict(os.environ,dict(DISPLAY=':1')),patch.object(launch,'executable',return_value=Path('/bin/true')),patch.object(launch,'observer_binary',return_value=Path('/bin/true')),patch.object(launch,'invoke',return_value=b'{"local_unix":false,"peer_pid":0}'),patch.object(launch,'ctl_json',return_value=dict(display=':2')):
            with self.assertRaisesRegex(launch.CheckError,'otro DISPLAY'):launch.check(args)


def integration(args):
    out=args.out.resolve()
    launch.require(out!=launch.REPO and launch.REPO not in out.parents,'La salida de pruebas debe quedar fuera del repositorio.')
    out.mkdir(parents=True,exist_ok=False)
    server=wm=collector=None;checks={}
    with tempfile.TemporaryDirectory(prefix='sal-') as tmp:
        env={k:v for k,v in os.environ.items() if k not in ('DISPLAY','XAUTHORITY','WAYLAND_DISPLAY','DBUS_SESSION_BUS_ADDRESS') and not k.startswith('MAVERICK_')}
        for key in ('HOME','XDG_RUNTIME_DIR','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_STATE_HOME'):
            p=Path(tmp)/('runtime' if key=='XDG_RUNTIME_DIR' else key);p.mkdir(mode=0o700);env[key]=str(p)
        cookie=os.urandom(16)
        def auth(path,value):
            fields=[b'',b'',b'MIT-MAGIC-COOKIE-1',value]
            path.write_bytes(struct.pack('!H',65535)+b''.join(struct.pack('!H',len(x))+x for x in fields));path.chmod(0o600)
        authority=Path(tmp)/'auth';auth(authority,cookie);env['XAUTHORITY']=str(authority)
        with ExitStack() as files:
            sl=files.enter_context((out/'server.log').open('wb'));wl=files.enter_context((out/'wm.log').open('wb'))
            try:
                server=subprocess.Popen(['Xvfb','-displayfd','1','-auth',str(authority),'-screen','0','1280x800x24','-nolisten','tcp'],env=env,stdout=subprocess.PIPE,stderr=sl,bufsize=0)
                assert select.select([server.stdout],[],[],5)[0];display=server.stdout.readline().decode().strip();assert display.isdecimal();env['DISPLAY']=':'+display
                config=Path(tmp)/'config.toml';config.write_text('[general]\nfocus_mouse=false\nwarp_cursor=false\n[autostart]\ncommands=[]\n')
                wm=subprocess.Popen([str(args.wm.resolve()),'--name','audit-test','--config',str(config)],env=env,stdout=wl,stderr=wl)
                command=[str(launch.HERE/'run'),'check','--ctl',str(args.ctl.resolve()),'--instance','audit-test']
                end=time.monotonic()+10
                while True:
                    r=subprocess.run(command+['--allow-unconfirmed'],env=env,capture_output=True,timeout=5)
                    if r.returncode==0:break
                    if time.monotonic()>end:raise RuntimeError('launcher readiness failed: '+r.stderr.decode())
                    time.sleep(.05)
                checks['xvfb_explicitly_unconfirmed']=json.loads(r.stdout)['source_kind']=='x11-unconfirmed'
                r=subprocess.run(command,env=env,capture_output=True,timeout=5)
                checks['virtual_not_confirmed_physical']=r.returncode==2 and b'Xorg' in r.stderr
                no_display=dict(env);no_display.pop('DISPLAY')
                r=subprocess.run(command,env=no_display,capture_output=True,timeout=5)
                checks['missing_display']=r.returncode==2 and b'DISPLAY' in r.stderr
                bad=Path(tmp)/'bad-auth';auth(bad,os.urandom(16))
                r=subprocess.run(command+['--allow-unconfirmed'],env=dict(env,XAUTHORITY=str(bad)),capture_output=True,timeout=5)
                checks['wrong_authentication']=r.returncode==2
                fake=Path(tmp)/'bad-ctl';fake.write_text('#!/bin/sh\nprintf "incompatible\\n"\n');fake.chmod(0o700)
                incompatible=list(command);incompatible[incompatible.index('--ctl')+1]=str(fake)
                r=subprocess.run(incompatible+['--allow-unconfirmed'],env=env,capture_output=True,timeout=5)
                checks['incompatible_ctl']=r.returncode==2
                capture=out/'capture'
                base=[str(launch.HERE/'run'),'capture','--allow-unconfirmed','--ctl',str(args.ctl.resolve()),'--instance','audit-test','--out',str(capture)]
                r=subprocess.run(base+['--preview'],env=env,capture_output=True,timeout=5)
                checks['preview_does_not_create_capture']=r.returncode==0 and not capture.exists()
                collector=subprocess.Popen(base+['--seconds','30'],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
                end=time.monotonic()+5
                while not (capture/'raw.jsonl').exists():
                    if collector.poll() is not None or time.monotonic()>end:raise RuntimeError('capture did not start')
                    time.sleep(.03)
                time.sleep(.3);collector.terminate();stdout,stderr=collector.communicate(timeout=8)
                (out/'launcher.stdout').write_bytes(stdout);(out/'launcher.stderr').write_bytes(stderr)
                require=collector.returncode==0
                checks['ordered_interrupt']=require and json.loads((capture/'manifest.json').read_text())['interrupted']
                checks['privacy']=verify(capture)['raw_schema_and_pseudonyms_verified']
                checks['physical_label_never_claimed']=json.loads((capture/'endpoint.json').read_text())['source_kind']=='x11-unconfirmed'
                native=out/'native-capture'
                r=subprocess.run([str(launch.HERE/'run'),'capture','--native-only','--allow-unconfirmed','--ctl',str(args.ctl.resolve()),'--instance','audit-test','--out',str(native),'--seconds','.6'],env=env,capture_output=True,timeout=8)
                native_rows=[json.loads(x) for x in (native/'raw.jsonl').read_text().splitlines()] if r.returncode==0 else []
                checks['native_only_has_no_logical_membership']=bool(native_rows) and all(not x['metrics'] and x['logical_query_error']=='disabled' for x in native_rows)
                checks['wm_survives_capture']=wm.poll() is None and server.poll() is None
                checks['private_permissions']=(capture.stat().st_mode&0o777)==0o700 and (capture/'raw.jsonl').stat().st_mode&0o777==0o600
                assert all(checks.values()),checks
            finally:
                stop(collector);stop(wm);stop(server)
                (out/'results.json').write_text(json.dumps(checks,indent=2)+'\n')
    print(json.dumps(checks,indent=2))


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--wm',type=Path);parser.add_argument('--ctl',type=Path);parser.add_argument('--out',type=Path)
    args=parser.parse_args()
    result=unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(LauncherContracts))
    if not result.wasSuccessful():raise SystemExit(1)
    if args.wm or args.ctl or args.out:
        if not all([args.wm,args.ctl,args.out]):parser.error('--wm, --ctl and --out must be supplied together')
        integration(args)
