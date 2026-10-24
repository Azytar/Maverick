#!/usr/bin/env python3
"""Checked capture and owned startx lifecycle. Never publish captured data."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import uuid

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
REFERENCE = 'e6c67fdaaddbe1093dcd0914bc536ecb86ecc41d'


class CheckError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise CheckError(message)


def executable(value):
    found = shutil.which(str(value))
    require(found is not None, f'No se encuentra el ejecutable: {value}')
    return Path(found).resolve()


def invoke(command, timeout=3, env=None):
    try:
        r = subprocess.run([str(x) for x in command], env=env, capture_output=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired) as e:
        raise CheckError('No se pudo ejecutar una comprobación dentro del plazo.') from e
    require(r.returncode == 0, 'Comprobación fallida: ' + Path(str(command[0])).name)
    return r.stdout


def private_dir(path):
    require(not path.is_symlink(), 'El directorio privado no puede ser un enlace.')
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    s = path.stat()
    require(s.st_uid == os.getuid() and not s.st_mode & 0o077,
            'El directorio debe pertenecer al usuario y tener permisos 0700.')
    return path


def observer_binary():
    cc = executable('cc')
    source = HERE/'observer.c'
    version = invoke([cc, '--version'])
    key = hashlib.sha256(source.read_bytes()+version).hexdigest()[:24]
    base = Path(os.environ.get('XDG_CACHE_HOME', str(Path.home()/'.cache')))
    cache=base/'maverick-stacking-audit'/key
    require(REPO not in cache.resolve().parents, 'La caché de compilación debe quedar fuera del repositorio.')
    folder = private_dir(cache)
    binary = folder/'observer'
    if not binary.exists():
        with tempfile.TemporaryDirectory(prefix='build-', dir=folder) as tmp:
            target = Path(tmp)/'observer'
            invoke([cc, '-std=c11', '-O2', '-Wall', '-Wextra', '-Werror', '-Wshadow',
                    '-Wformat=2', source, '-o', target, '-lX11'], timeout=30)
            target.chmod(0o700)
            os.replace(target, binary)
    require(not binary.is_symlink() and binary.stat().st_uid == os.getuid()
            and not binary.stat().st_mode & 0o077, 'Caché de sonda no segura.')
    return binary


def display_key(value):
    m = re.fullmatch(r'(?:unix/)?:(\d+)(?:\.(\d+))?', value or '')
    require(m is not None, 'Se requiere un DISPLAY Unix local explícito, por ejemplo :0.')
    return int(m[1]), int(m[2] or 0)


def physical_from_peer(peer, proc=Path('/proc')):
    # SO_PEERCRED binds this assessment to the authenticated X connection,
    # rather than an unrelated process named Xorg elsewhere on the machine.
    if not peer.get('local_unix') or not isinstance(peer.get('peer_pid'), int) or peer['peer_pid'] <= 0:
        return dict(confirmed=False, reason='no_local_peer_credentials')
    folder = proc/str(peer['peer_pid'])
    try:
        name = (folder/'exe').resolve(strict=True).name
        if name not in ('Xorg', 'Xorg.bin'):
            return dict(confirmed=False, reason='peer_is_not_Xorg')
        args = (folder/'cmdline').read_bytes().split(b'\0')
        vt = next((a.decode('ascii') for a in args if re.fullmatch(rb'vt[1-9]\d*', a)), None)
        targets = []
        for fd in (folder/'fd').iterdir():
            try:
                targets.append(os.readlink(fd))
            except OSError:
                pass
        drm = any(re.fullmatch(r'/dev/dri/card\d+', s) for s in targets)
        tty = vt is not None and '/dev/tty'+vt[2:] in targets
        if drm and tty:
            return dict(confirmed=True, reason='authenticated_Xorg_peer_with_DRM_card_and_matching_VT')
        return dict(confirmed=False, reason='physical_DRM_and_VT_not_verified')
    except (OSError, UnicodeError):
        return dict(confirmed=False, reason='server_process_not_inspectable')


def ctl_json(ctl, instance, words):
    data = invoke([ctl, *words, '--name', instance])
    require(len(data) <= 4*1024*1024, 'Respuesta de maverickctl demasiado grande.')
    try:
        return json.loads(data)
    except ValueError as e:
        raise CheckError('maverickctl no devuelve el JSON compatible esperado.') from e


def check(args):
    require(os.geteuid() != 0, 'Ejecuta la herramienta como usuario normal, sin sudo.')
    display_key(os.environ.get('DISPLAY'))
    ctl = executable(args.ctl)
    observer = observer_binary()
    try:
        peer = json.loads(invoke([observer, '--identity']))
    except ValueError as e:
        raise CheckError('Respuesta de identidad X11 inválida.') from e
    assessment = physical_from_peer(peer)
    require(assessment['confirmed'] or args.allow_unconfirmed,
            'No se puede verificar Xorg físico ('+assessment['reason']+'). '
            'Aborto; --allow-unconfirmed permite solo datos etiquetados x11-unconfirmed.')
    identity = ctl_json(ctl, args.instance, ['identify'])
    require(isinstance(identity, dict) and isinstance(identity.get('display'), str),
            'maverickctl identify incompatible.')
    require(display_key(identity['display']) == display_key(os.environ.get('DISPLAY')),
            'La instancia Maverick pertenece a otro DISPLAY.')
    if getattr(args,'native_only',False):
        return dict(assessment=assessment,source_kind='xorg-physical' if assessment['confirmed'] else 'x11-unconfirmed',observer=observer,ctl=ctl)
    tree = ctl_json(ctl, args.instance, ['query', 'tree'])
    from audit import sanitized_tree
    try:
        clean = sanitized_tree(tree)
        require(bool(clean['worlds']), 'El árbol no contiene un mundo activo.')
    except (KeyError, TypeError, ValueError) as e:
        raise CheckError('Esquema de query tree incompatible.') from e
    return dict(assessment=assessment,
                source_kind='xorg-physical' if assessment['confirmed'] else 'x11-unconfirmed',
                observer=observer, ctl=ctl)


def output_path(value=None):
    if value:
        path = Path(value).expanduser().absolute()
    else:
        base = Path(os.environ.get('XDG_STATE_HOME', str(Path.home()/'.local/state')))
        path = base/'maverick-stacking-audit'/('capture-'+time.strftime('%Y%m%dT%H%M%S')+'-'+uuid.uuid4().hex[:6])
    resolved = path.resolve()
    require(resolved != REPO and REPO not in resolved.parents,
            'Los resultados deben guardarse fuera del repositorio.')
    require(not path.exists() and not path.is_symlink(), 'La salida debe ser un directorio nuevo.')
    return path


def capture(args, wm=None):
    info = check(args)
    destination = output_path(args.out)
    command = [sys.executable, str(HERE/'audit.py'), '--observer', str(info['observer']),
               '--ctl', str(info['ctl']),
               '--source-kind', info['source_kind'], '--seconds', str(args.seconds),
               '--interval-ms', str(args.interval_ms), '--heartbeat', str(args.heartbeat),
               '--out', str(destination)]
    if not args.native_only:command += ['--instance',args.instance]
    if args.preview:
        print(json.dumps(dict(would_capture=True, source_kind=info['source_kind'],
                              seconds=args.seconds, output=str(destination)), ensure_ascii=False))
        return
    child = None
    interrupted = False
    old = {}

    def stop_capture(sig, _frame):
        nonlocal interrupted
        interrupted = True
        if child is not None and child.poll() is None:
            child.send_signal(sig)

    for sig in (signal.SIGINT, signal.SIGTERM):
        old[sig] = signal.signal(sig, stop_capture)
    try:
        child=subprocess.Popen(command)
        if interrupted:child.terminate()
        while child.poll() is None:
            if wm is not None and wm.poll() is not None:
                interrupted=True
                child.terminate()
            time.sleep(.1)
        code = child.returncode
    finally:
        if child is not None and child.poll() is None:
            child.terminate()
            try:
                child.wait(5)
            except subprocess.TimeoutExpired:
                child.kill();child.wait(3)
        for sig, handler in old.items():
            signal.signal(sig, handler)
    require(code == 0, 'Captura fallida; se conservan los datos parciales en '+str(destination))
    # No PID, DISPLAY, process path, instance label or inverse alias map is retained.
    attestation = dict(source_kind=info['source_kind'], physical=info['assessment'],
                       audited_policy_reference=REFERENCE, interrupted=interrupted,
                       limits='endpoint verification is not proof of policy equivalence or visual completion')
    (destination/'endpoint.json').write_text(json.dumps(attestation, indent=2)+'\n')
    from privacy import verify
    verify(destination)
    print('Resultados privados: '+str(destination))


def logind(command):
    return invoke([executable('loginctl'), *command]).decode().strip()


def tty_plan(args):
    require(os.geteuid() != 0, 'No uses sudo para iniciar la sesión.')
    require(not os.environ.get('DISPLAY') and not os.environ.get('WAYLAND_DISPLAY'),
            'Cierra la sesión gráfica anterior; cambiar de VT no equivale a cerrarla.')
    require(os.isatty(0), 'El modo tty requiere una TTY física de entrada.')
    tty = os.ttyname(0)
    m = re.fullmatch(r'/dev/tty([1-9]\d*)', tty)
    require(m is not None, 'No se acepta una pseudoterminal SSH/PTY.')
    require(Path('/sys/class/tty/tty0/active').read_text().strip() == 'tty'+m[1],
            'La TTY debe ser la VT activa.')
    sid = os.environ.get('XDG_SESSION_ID')
    require(bool(sid), 'No hay identidad logind de sesión; no se puede verificar el asiento.')
    props = dict(line.split('=', 1) for line in logind(['show-session', sid,
                  '-p', 'Type', '-p', 'Active', '-p', 'Remote', '-p', 'Seat', '-p', 'VTNr', '-p', 'User']).splitlines())
    require(props.get('Type') == 'tty' and props.get('Active') == 'yes' and props.get('Remote') == 'no'
            and props.get('VTNr') == m[1] and props.get('User') == str(os.getuid()) and props.get('Seat'),
            'La sesión logind no corresponde a esta TTY local activa.')
    for line in logind(['list-sessions', '--no-legend']).splitlines():
        other = line.split()[0]
        facts = dict(s.split('=', 1) for s in logind(['show-session', other, '-p', 'Type', '-p', 'Seat']).splitlines())
        require(not (facts.get('Seat') == props['Seat'] and facts.get('Type') in ('x11', 'wayland')),
                'Aún hay una sesión gráfica en este asiento. Ciérrala antes de ejecutar tty.')
    for entry in Path('/proc').glob('[0-9]*'):
        try:
            require((entry/'comm').read_text().strip() not in ('Xorg','Xwayland','XLibre'),
                    'Hay un servidor gráfico existente; no se iniciará otro automáticamente.')
        except OSError:
            continue
    require(any(os.access(p, os.R_OK | os.W_OK) for p in Path('/dev/dri').glob('card*')),
            'No se puede verificar acceso normal al dispositivo DRM de la sesión.')
    xorg = executable('Xorg')
    require(xorg.name in ('Xorg', 'Xorg.bin', 'Xorg.wrap'), 'El servidor debe ser el ejecutable Xorg.')
    startx = executable('startx')
    number = args.display_number
    require(not Path(f'/tmp/.X{number}-lock').exists() and not Path(f'/tmp/.X11-unix/X{number}').exists(),
            'El número de display está ocupado o tiene un lock; no se borrará.')
    # startx/xinit owns only the fresh server it launches. No .xinitrc is edited.
    destination = output_path(args.out)
    command = [str(startx), str(HERE/'run'), '_session', '--wm', str(executable(args.wm)),
               '--ctl', str(executable(args.ctl)), '--instance', 'stack-audit-'+uuid.uuid4().hex[:8],
               '--seconds', str(args.seconds), '--out', str(destination),
               '--interval-ms',str(args.interval_ms),'--heartbeat',str(args.heartbeat)]
    if args.config:
        require(Path(args.config).is_file(), 'No existe el archivo de configuración.')
        command += ['--config', str(Path(args.config).resolve())]
    if args.native_only:command += ['--native-only']
    if args.allow_unconfirmed:
        command += ['--allow-unconfirmed']
    command += ['--', str(xorg), ':'+str(number), 'vt'+m[1], '-keeptty', '-nolisten', 'tcp']
    return command


def fresh_tty_endpoint(peer, expected_vt, not_before, proc=Path('/proc')):
    require(peer.get('local_unix') and isinstance(peer.get('peer_pid'),int) and peer['peer_pid']>0,
            'No se puede vincular el cliente al nuevo servidor local.')
    folder=proc/str(peer['peer_pid'])
    fields=(folder/'stat').read_text().rsplit(') ',1)[1].split()
    start=int(fields[19])
    require(start>=not_before-1, 'El servidor ya existía antes de este arranque.')
    require(('vt'+expected_vt).encode() in (folder/'cmdline').read_bytes().split(b'\0'),
            'El servidor no corresponde a la VT solicitada.')


def session(args):
    require(os.environ.get('STACK_AUDIT_TTY_CHILD') == '1', '_session solo se inicia mediante tty.')
    require(display_key(os.environ.get('DISPLAY'))[0]==int(os.environ['STACK_AUDIT_DISPLAY']),
            'xinit entregó un display distinto del solicitado.')
    peer=json.loads(invoke([observer_binary(),'--identity']))
    fresh_tty_endpoint(peer,os.environ['STACK_AUDIT_VT'],int(os.environ['STACK_AUDIT_NOT_BEFORE']))
    wm = None
    old={}
    def interrupted(_sig,_frame):
        raise KeyboardInterrupt
    for sig in (signal.SIGINT,signal.SIGTERM):old[sig]=signal.signal(sig,interrupted)
    try:
        command = [str(executable(args.wm)), '--name', args.instance]
        if args.config:
            command += ['--config', args.config]
        wm = subprocess.Popen(command)
        deadline = time.monotonic()+20
        while True:
            require(wm.poll() is None, 'Maverick terminó antes de estar listo.')
            try:
                check(args)
                break
            except CheckError:
                require(time.monotonic() < deadline, 'Maverick/Xorg no superó las comprobaciones de arranque.')
                time.sleep(.2)
        capture(args, wm)
    finally:
        if wm and wm.poll() is None:
            wm.terminate()
            try:
                wm.wait(5)
            except subprocess.TimeoutExpired:
                wm.kill();wm.wait(3)
        for sig,handler in old.items():signal.signal(sig,handler)


def main():
    os.umask(0o077)
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest='mode', required=True)
    for mode in ('check', 'capture', 'tty', '_session'):
        q = sub.add_parser(mode)
        q.add_argument('--ctl', default='maverickctl')
        q.add_argument('--instance', default='default')
        q.add_argument('--allow-unconfirmed', action='store_true')
        q.add_argument('--native-only',action='store_true',help='No consultar query tree: pertenencia lógica desconocida.')
        q.add_argument('--wm', default='maverick')
        q.add_argument('--config')
        q.add_argument('--seconds', type=float, default=600)
        q.add_argument('--out')
        q.add_argument('--preview', action='store_true')
        q.add_argument('--interval-ms', type=int, default=250)
        q.add_argument('--heartbeat', type=float, default=5)
        q.add_argument('--display-number', type=int, default=0)
    q = sub.add_parser('privacy');q.add_argument('capture', type=Path)
    q = sub.add_parser('analyze');q.add_argument('captures', nargs='+', type=Path);q.add_argument('--out', type=Path, required=True)
    args = p.parse_args()
    try:
        if args.mode in ('check', 'capture', 'tty', '_session'):
            require(math.isfinite(args.seconds) and args.seconds > 0 and math.isfinite(args.heartbeat)
                    and args.heartbeat >= 2 and 100 <= args.interval_ms <= 10000
                    and 0 <= args.display_number <= 63, 'Duración o intervalos inválidos.')
        if args.mode == 'check':
            result = check(args)
            print(json.dumps(dict(ready=True, source_kind=result['source_kind'], physical=result['assessment'])))
        elif args.mode == 'capture':
            capture(args)
        elif args.mode == 'tty':
            observer_binary()
            command = tty_plan(args)
            if args.preview:
                print(json.dumps(dict(would_start=command), ensure_ascii=False))
            else:
                env = {k:v for k,v in os.environ.items() if not k.startswith('MAVERICK_')}
                env['STACK_AUDIT_TTY_CHILD'] = '1'
                env['STACK_AUDIT_DISPLAY']=str(args.display_number)
                env['STACK_AUDIT_VT']=os.ttyname(0).removeprefix('/dev/tty')
                env['STACK_AUDIT_NOT_BEFORE']=str(int(time.clock_gettime(time.CLOCK_BOOTTIME)*os.sysconf('SC_CLK_TCK')))
                raise SystemExit(subprocess.call(command, env=env))
        elif args.mode == '_session':
            session(args)
        elif args.mode == 'privacy':
            from privacy import verify
            print(json.dumps(verify(args.capture)))
        else:
            destination = output_path(str(args.out))
            raise SystemExit(subprocess.call([sys.executable, str(HERE/'analyze.py'),
                                              *map(str,args.captures), '--out', str(destination)]))
    except KeyboardInterrupt:
        raise SystemExit(130)
    except (CheckError, OSError, ValueError) as e:
        print('Error: '+str(e), file=sys.stderr)
        raise SystemExit(2)


if __name__ == '__main__':
    main()
