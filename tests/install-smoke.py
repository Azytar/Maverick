#!/usr/bin/env python3
"""Installer integration tests: no real sudo, package installation or WM startup."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
BINS = ("maverick", "maverickctl")


def executable(path, text):
    path.write_text(text)
    path.chmod(0o755)


def scenario(name, args, build=False, missing=False, auth_fail=False, system=False, real=False, repair=False):
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as directory:
        base = Path(directory)
        repo, home, tools = base / "repo", base / "home", base / "tools"
        for path in (repo / "target/release", repo / "config", home, tools):
            path.mkdir(parents=True)
        shutil.copy2(ROOT / "install.sh", repo / "install.sh")
        (repo / "config/config.toml").write_text("[general]\nn_tags = 4\n")
        log = base / "calls"
        env = os.environ.copy()
        for key in list(env):
            if key.startswith(("CARGO_", "RUST", "SUDO_", "XDG_")) or key == "PREFIX":
                del env[key]
        prefix = base / "installed"
        env.update(HOME=str(home), PATH=f"{tools}:/usr/bin:/bin", NO_COLOR="1",
                   MAVERICK_NO_ANIM="1", TEST_LOG=str(log), TEST_PREFIX=str(prefix),
                   TEST_REPO=str(repo), TEST_AUTH_FAIL=str(int(auth_fail)))
        # Any elevated command is logged, then mapped to our private prefix.
        executable(tools / "sudo", '''#!/usr/bin/env python3
import os, subprocess, sys
with open(os.environ['TEST_LOG'], 'a') as f: f.write('sudo ' + repr(sys.argv[1:]) + '\\n')
a = sys.argv[1:]
if '-v' in a: sys.exit(int(os.environ['TEST_AUTH_FAIL']))
while a and a[0] in ('-n', '--'): a.pop(0)
assert a and a[0] in ('mkdir', 'install', 'mv', 'mktemp', 'rm', 'find'), a
if a[0] == 'find':
    assert '--no-dereference' in a and '--from=0' in a and '-P' in a and '-xdev' in a
    sys.exit(0)
p = os.environ['TEST_PREFIX']
a = [p + s[len('/usr/local'):] if s.startswith('/usr/local') else
     p + '/share/xsessions' + s[len('/usr/share/xsessions'):] if s.startswith('/usr/share/xsessions') else s for s in a]
sys.exit(subprocess.run(a).returncode)
''')
        executable(tools / "cargo", '''#!/bin/bash
set -eu
printf 'cargo uid=%s %s\\n' "$(id -u)" "$*" >> "$TEST_LOG"
case "$1" in
 --version) echo 'cargo 1.98.1' ;;
 tree) echo maverick ;;
 build)
   [[ $(id -u) != 0 ]]
   [[ "$CARGO_TARGET_DIR" == "$TEST_REPO/target" ]]
   for b in maverick maverickctl; do
     printf '#!/bin/sh\\necho "maverick test"\\n' > "$CARGO_TARGET_DIR/release/$b"
     chmod 755 "$CARGO_TARGET_DIR/release/$b"
   done
   echo 'Finished release' ;;
esac
''')
        if repair:
            executable(tools / 'find', '''#!/bin/sh
if [ ! -f "$TEST_REPO/find-seen" ]; then
    touch "$TEST_REPO/find-seen"
    echo "$TEST_REPO/target/root-owned-artifact"
fi
''')
        executable(tools / "rustc", "#!/bin/sh\necho 'rustc 1.98.1'\n")
        executable(tools / "cc", "#!/bin/sh\nexit 0\n")
        if not build:
            for binary in BINS:
                if missing and binary == BINS[-1]:
                    continue
                if real:
                    shutil.copy2(ROOT / 'target/release' / binary, repo / 'target/release' / binary)
                else:
                    executable(repo / "target/release" / binary,
                               '#!/bin/sh\necho "maverick test"\n')
        command = ["bash", str(repo / "install.sh"), "--yes", "--no-anim", "--lang", "en"]
        if not system:
            command += ["--prefix", str(prefix)]
        if not build:
            command += ["--no-build"]
        result = subprocess.run(command + args, env=env, capture_output=True, text=True, timeout=45)
        calls = log.read_text() if log.exists() else ""
        if missing or auth_fail:
            assert result.returncode != 0, (name, result.stdout, result.stderr)
            assert not (prefix / "bin/maverick").exists(), name + ': partially installed'
        else:
            assert result.returncode == 0, (name, result.stdout[-2000:], result.stderr)
            for binary in BINS:
                installed = prefix / "bin" / binary
                assert installed.read_bytes() == (repo / "target/release" / binary).read_bytes()
                assert installed.stat().st_mode & 0o777 == 0o755
            session = prefix / "share/xsessions/maverick.desktop"
            assert session.is_file()
            expected = '/usr/local/bin/maverick' if system else str(prefix / 'bin/maverick')
            assert f'Exec="{expected}"' in session.read_text()
            if shutil.which('desktop-file-validate'):
                validation = subprocess.run(['desktop-file-validate', str(session)], capture_output=True, text=True)
                assert validation.returncode == 0, (session.read_text(), validation.stdout, validation.stderr)
            assert (home / ".config/maverick/config.toml").is_file()
            assert (home / '.config/maverick/config.toml').stat().st_uid == os.getuid()
            for path in (repo / "target").rglob("*"):
                assert path.stat().st_uid == os.getuid(), path
            assert ("cargo uid=" in calls) == build or not build
        if system:
            assert calls.splitlines()[0] == "sudo ['-v']", calls
        else:
            assert 'sudo ' not in calls, calls
        if build:
            assert ' build ' in calls, calls
        print('PASS:', name)


def main():
    assert os.getuid() != 0, 'Run this test as a normal user'
    scenario('local prebuilt', [])
    scenario('local build', [], build=True)
    scenario('system auth and install', [], system=True)
    scenario('authentication failure', [], system=True, auth_fail=True)
    scenario('missing binary fails before copying', [], missing=True)
    for option in ('--prefix', '--lang'):
        result = subprocess.run(['bash', str(ROOT / 'install.sh'), option], capture_output=True)
        assert result.returncode == 2
    print('PASS: missing CLI values')


if __name__ == '__main__':
    main()
