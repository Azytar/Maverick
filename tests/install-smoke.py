#!/usr/bin/env python3
"""Installer integration tests.

Everything happens inside a temporary directory: no real sudo, no package
installation, no WM startup, and no write outside the temporary prefix. The
suites below cover what the installer promises rather than how it looks while
doing it -- that the prefix is a boundary, that the set of installed binaries
is exactly the supported set, that a broken binary is caught rather than
reported as a successful install, and that a repeated install converges.
"""
import os
from pathlib import Path
import pty
import select
import shutil
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parent.parent
# The installer is `installer/install.sh`, and it refuses to run unless its
# `lib/` sits beside it, so the harness copies the whole directory into the
# sandbox rather than the entry point alone.
INSTALLER_DIR = REPO / "installer"
ENTRY = INSTALLER_DIR / "install.sh"
BINS = ("maverick", "maverickctl")
# Obsolete artifacts that must never reappear in an install.
OBSOLETE = ("maverick-setup", "maverick-msg")

# The marker the installer brackets its PATH block with. A startup file that
# gains one of these carries a line the user did not write, so the count is
# asserted, not just the presence.
PATH_MARKER = "# >>> maverick (install.sh) >>>"

# Every startup file the installer is allowed to touch. All of them live
# inside $HOME; a path outside it must never appear in this list.
def startup_files(box):
    return [box.home / name for name in
            (".profile", ".bash_profile", ".bash_login", ".bashrc",
             ".zshenv", ".zprofile", ".zshrc", ".config/fish/config.fish")]

# A stand-in for a real Maverick binary: it answers the three commands the
# installer verifies with, so a passing install means the installer really
# executed them.
STUB_OK = """#!/bin/sh
case "$1" in
  --version) echo "maverick 0.0.0-test"; exit 0 ;;
  session)   [ "${2:-}" = "--help" ] && echo "maverickctl sessions - test stub"; exit 0 ;;
  --help)    echo "usage: test stub"; exit 0 ;;
esac
exit 0
"""

# A stand-in for a binary that predates Sessions: it answers everything except
# the session command group, which it rejects the way the real pre-Sessions
# maverickctl did.
STUB_NO_SESSIONS = """#!/bin/sh
case "$1" in
  --version) echo "maverick 0.0.0-test"; exit 0 ;;
  session)   echo "maverickctl: unknown command 'session'" >&2; exit 1 ;;
  --help)    echo "usage: test stub"; exit 0 ;;
esac
exit 0
"""


def executable(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(0o755)


def fake_cargo(tools, log, stub_dir):
    """A cargo that records how it was called and produces the release set.

    The stub bodies are written to files rather than interpolated into the
    script, so shell quoting in the stub can never corrupt the harness.
    """
    executable(tools / "cargo", f"""#!/bin/bash
set -eu
printf 'cargo %s\\n' "$*" >> "{log}"
case "$1" in
  --version) echo 'cargo 1.98.1' ;;
  tree) echo maverick ;;
  build)
    [[ $(id -u) != 0 ]]
    # The installer must hand cargo the directory it was told to use, not one
    # of its own choosing.
    [[ -n "${{CARGO_TARGET_DIR:-}}" ]]
    printf 'cargo target %s\\n' "$CARGO_TARGET_DIR" >> "{log}"
    for b in maverick maverickctl; do
      cp "{stub_dir}/$b" "$CARGO_TARGET_DIR/release/$b"
      chmod 755 "$CARGO_TARGET_DIR/release/$b"
    done
    echo 'Finished release' ;;
esac
""")
    executable(tools / "rustc", "#!/bin/sh\necho 'rustc 1.98.1'\n")
    executable(tools / "cc", "#!/bin/sh\nexit 0\n")


def fake_sudo(tools, log):
    """A sudo that refuses, so any use of it fails the suite loudly."""
    executable(tools / "sudo", f"""#!/bin/sh
printf 'sudo %s\\n' "$*" >> "{log}"
echo 'sudo must not be used by the installer' >&2
exit 1
""")


class Sandbox:
    """One isolated installation environment."""

    def __init__(self, base):
        self.base = base
        self.repo = base / "repo"
        self.home = base / "home"
        self.tools = base / "tools"
        self.prefix = base / "installed"
        self.target = base / "cargo-target"
        self.log = base / "calls"
        for path in (self.repo / "config", self.home, self.tools,
                     self.repo / "target/release", self.target / "release"):
            path.mkdir(parents=True)
        shutil.copytree(INSTALLER_DIR, self.repo / "installer")
        (self.repo / "config/config.toml").write_text("[general]\nn_tags = 4\n")
        self.stubs = base / "stubs"
        self.stubs.mkdir()
        self.seed_build()
        fake_cargo(self.tools, self.log, self.stubs)
        fake_sudo(self.tools, self.log)
        self.calls = ""

    def seed_build(self, stub=STUB_OK):
        """(Re)write the artifacts cargo would produce, in the target dir."""
        for binary in BINS:
            executable(self.target / "release" / binary, stub)
            executable(self.stubs / binary, stub)

    def env(self, **extra):
        env = os.environ.copy()
        for key in list(env):
            # A caller's XDG/CARGO/SUDO state must not leak into the test.
            if key.startswith(("CARGO_", "RUST", "SUDO_", "XDG_")) or key in ("PREFIX", "HOME"):
                del env[key]
        env.update(HOME=str(self.home), PATH=f"{self.tools}:/usr/bin:/bin",
                   NO_COLOR="1", MAVERICK_NO_ANIM="1", CARGO_TARGET_DIR=str(self.target))
        env.update({k: str(v) for k, v in extra.items()})
        return env

    def install(self, *args, env=None, timeout=60):
        command = ["bash", str(self.repo / "installer/install.sh"), "--yes", "--no-anim", "--lang", "en"]
        command += list(args)
        result = subprocess.run(command, env=env or self.env(), capture_output=True,
                                text=True, timeout=timeout)
        self.calls = self.log.read_text() if self.log.exists() else ""
        return result

    def snapshot(self, *paths):
        return {p: (p.stat().st_mtime_ns, p.read_bytes() if p.is_file() else None)
                for p in paths if p.exists()}


def interactive_install(box, keys, args=()):
    """Run install.sh on a real pty, answering each prompt as it appears.

    The PATH offer is only made when stdin is a terminal, so without a pty
    there is no way to reach the branch that decides it. Answers are queued
    against the text of the question rather than a fixed order, so a prompt
    that is skipped (no existing config, say) cannot shift the rest.
    """
    master, slave = pty.openpty()
    env = box.env(SHELL="/bin/bash", TERM="xterm-256color")
    command = ["bash", str(box.repo / "installer/install.sh"), "--no-build", "--lang", "en"]
    command += list(args)
    proc = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave,
                            env=env, close_fds=True)
    os.close(slave)
    out = bytearray()
    pending = [dict(k) for k in keys]
    deadline = time.time() + 60
    while time.time() < deadline and proc.poll() is None:
        ready, _, _ = select.select([master], [], [], 0.2)
        if ready:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            out += chunk
        for index, key in enumerate(pending):
            if key["q"].encode() in out:
                pending.pop(index)
                os.write(master, key["a"].encode())
                break
    while True:
        ready, _, _ = select.select([master], [], [], 0.3)
        if not ready:
            break
        try:
            chunk = os.read(master, 65536)
        except OSError:
            break
        if not chunk:
            break
        out += chunk
    os.close(master)
    if proc.poll() is None:
        # Never leave a child stuck on a prompt nobody answered; the caller's
        # assertions on the exit status are what reports the hang.
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
    else:
        proc.wait(timeout=30)
    return proc.returncode, out.decode("utf-8", "replace")


def check_success(box, result, expect_prefix=None):
    """Assertions that must hold after any successful installation."""
    name = expect_prefix or box.prefix
    assert result.returncode == 0, (result.stdout[-2000:], result.stderr[-2000:])
    for binary in BINS:
        installed = name / "bin" / binary
        assert installed.is_file(), f"{binary} not installed"
        assert installed.read_bytes() == (box.target / "release" / binary).read_bytes()
        assert installed.stat().st_mode & 0o777 == 0o755, oct(installed.stat().st_mode)
    # The supported set is exactly these two: an obsolete artifact reappearing
    # is a packaging regression, not a harmless extra.
    for stale in OBSOLETE:
        assert not (name / "bin" / stale).exists(), f"{stale} must not be installed"
    session = name / "share/xsessions/maverick.desktop"
    assert session.is_file(), "session file missing"
    assert session.stat().st_mode & 0o777 == 0o644
    assert f'Exec="{name}/bin/maverick"' in session.read_text()
    if shutil.which("desktop-file-validate"):
        check = subprocess.run(["desktop-file-validate", str(session)],
                               capture_output=True, text=True)
        assert check.returncode == 0, (session.read_text(), check.stdout, check.stderr)
    assert (box.home / ".config/maverick/config.toml").is_file()
    assert (box.home / ".config/maverick/config.toml").stat().st_uid == os.getuid()
    # Nothing was built inside the checkout.
    assert not (box.repo / "target/release/maverick").exists(), "checkout used as build dir"
    assert "sudo " not in box.calls, f"installer invoked sudo: {box.calls}"


def suite_local_install():
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build()
        result = box.install("--prefix", str(box.prefix), "--no-build")
        check_success(box, result)
        # The artifacts came from the CARGO_TARGET_DIR the caller supplied. Had
        # the installer substituted a directory of its own, the pre-build
        # artifact check would have failed instead of installing this content.
        assert (box.prefix / "bin/maverick").read_bytes() == \
            (box.target / "release/maverick").read_bytes()
        assert "sudo " not in box.calls, box.calls
        print("PASS: user install honours CARGO_TARGET_DIR and needs no privileges")


def suite_default_prefix_is_user_local():
    """A bare ./install.sh must land in $HOME/.local, not /usr/local."""
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build()
        result = box.install("--no-build")
        assert result.returncode == 0, (result.stdout[-2000:], result.stderr[-2000:])
        expected = box.home / ".local/bin"
        assert (expected / "maverick").is_file(), "default prefix is not $HOME/.local/bin"
        assert (expected / "maverickctl").is_file()
        for stale in OBSOLETE:
            assert not (expected / stale).exists()
        assert (box.home / ".local/share/xsessions/maverick.desktop").is_file()
        assert "sudo " not in box.calls, box.calls
        print("PASS: bare ./install.sh defaults to $HOME/.local with no sudo")


def suite_path_setup():
    """A default install must leave the binaries reachable from the next shell.

    The binaries land in $HOME/.local/bin, which is not on PATH on every
    distribution, and telling a person who has just run an installer to export
    PATH by hand is the one step they are most likely to get wrong. The
    installer writes a marked block into the shell's own startup files
    instead. What matters is not that the block exists but that sourcing the
    file resolves maverick, and that a repeat install adds no second answer.
    """
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build()
        env = box.env(SHELL="/bin/bash")
        result = box.install("--no-build", env=env)
        assert result.returncode == 0, (result.stdout[-2000:], result.stderr[-2000:])
        touched = [p for p in startup_files(box) if p.exists()]
        assert touched, "the installer wrote no shell startup file"
        for path in touched:
            text = path.read_text()
            assert PATH_MARKER in text, f"{path} carries no PATH block"
            assert text.count(PATH_MARKER) == 1, f"{path} holds more than one PATH block"
            assert str(box.home / ".local/bin") in text, path
        # Execute the block: in a shell that starts without the directory on
        # PATH, sourcing the file must resolve maverick to what was installed.
        profile = box.home / ".profile"
        assert profile.is_file(), "no POSIX profile for a bash login shell"
        probe = subprocess.run(
            ["sh", "-c", f'. "{profile}"; command -v maverick'],
            capture_output=True, text=True,
            env={"HOME": str(box.home), "PATH": "/usr/bin:/bin"})
        assert probe.returncode == 0, (probe.stdout, probe.stderr)
        assert probe.stdout.strip() == str(box.home / ".local/bin/maverick")
        # A repeat install finds its own block and rewrites nothing.
        before = {p: p.read_bytes() for p in touched}
        repeat = box.install("--no-build", env=env)
        assert repeat.returncode == 0, (repeat.stdout[-2000:], repeat.stderr[-2000:])
        after = {p: p.read_bytes() for p in touched}
        assert before == after, "a repeat install rewrote the startup files"
        print("PASS: PATH block is written once, works, and is not duplicated")


def suite_path_opt_out():
    """Nothing is written when the caller opted out or PATH already has it."""
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build()
        env = box.env(SHELL="/bin/bash")
        result = box.install("--no-build", "--no-path", env=env)
        assert result.returncode == 0, (result.stdout[-2000:], result.stderr[-2000:])
        assert not any(p.exists() for p in startup_files(box)), \
            "--no-path wrote a shell startup file"
        assert "export PATH=" in result.stdout, "no command was offered instead"
        # Already reachable: no prompt, no file, nothing for the caller to do.
        bin_dir = str(box.home / ".local/bin")
        env = box.env(SHELL="/bin/bash", PATH=f"{bin_dir}:{box.tools}:/usr/bin:/bin")
        second = box.install("--no-build", env=env)
        assert second.returncode == 0, (second.stdout[-2000:], second.stderr[-2000:])
        assert not any(p.exists() for p in startup_files(box)), \
            "a directory already on PATH still earned a startup file"
        assert "already on PATH" in second.stdout
        print("PASS: --no-path and an already-present PATH entry write nothing")


def suite_path_respects_existing_config():
    """A startup file that already names the directory is reported, not doubled."""
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build()
        profile = box.home / ".profile"
        # Debian and Ubuntu ship exactly this line in ~/.profile; it puts the
        # directory on PATH for the whole graphical session.
        profile.write_text('if [ -d "$HOME/.local/bin" ] ; then\n'
                           '    PATH="$HOME/.local/bin:$PATH"\n'
                           'fi\n')
        result = box.install("--no-build", env=box.env(SHELL="/bin/bash"))
        assert result.returncode == 0, (result.stdout[-2000:], result.stderr[-2000:])
        text = profile.read_text()
        assert PATH_MARKER not in text, "the installer duplicated an existing PATH line"
        assert 'PATH="$HOME/.local/bin:$PATH"' in text, "the distro line was rewritten"
        assert not (box.home / ".bashrc").exists(), "a second answer was written"
        assert "already configured in" in result.stdout
        print("PASS: an existing PATH configuration is respected, not duplicated")


def suite_path_follows_shell():
    """The files edited are the files the caller's shell actually reads."""
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build()
        result = box.install("--no-build", env=box.env(SHELL="/usr/bin/zsh"))
        assert result.returncode == 0, (result.stdout[-2000:], result.stderr[-2000:])
        assert PATH_MARKER in (box.home / ".zshrc").read_text()
        # zsh reads no POSIX profile of its own: none is invented for it.
        assert not (box.home / ".profile").exists(), "a file zsh never reads was created"
        print("PASS: PATH block goes to the rc file of the shell in use")
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build()
        result = box.install("--no-build", env=box.env(SHELL="/usr/bin/fish"))
        assert result.returncode == 0, (result.stdout[-2000:], result.stderr[-2000:])
        text = (box.home / ".config/fish/config.fish").read_text()
        assert PATH_MARKER in text
        assert "set -gx PATH" in text, "fish was handed POSIX syntax"
        assert "export PATH" not in text, "fish does not read export"
        print("PASS: fish gets fish syntax")


def suite_path_interactive():
    """The PATH offer only exists on a terminal, so it is tested on one."""
    # Refusing must leave every startup file exactly as it was.
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build()
        rc, out = interactive_install(box, [
            {"q": "Install Maverick to", "a": "\r"},
            {"q": "Add to PATH in", "a": "n\r"}])
        assert rc == 0, (rc, out[-3000:])
        assert "Add to PATH in" in out, "the PATH prompt was never offered"
        assert "export PATH=" in out, "a refusal offered nothing to run instead"
        assert not any(p.exists() for p in startup_files(box)), \
            "a refused PATH edit wrote a startup file"
    # Accepting must write the block, name the files, and never ask twice.
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build()
        rc, out = interactive_install(box, [
            {"q": "Install Maverick to", "a": "\r"},
            {"q": "Add to PATH in", "a": "\r"}])
        assert rc == 0, (rc, out[-3000:])
        assert "PATH added to" in out, out[-3000:]
        assert "new terminal" in out, "the caller was not told how to pick it up"
        assert PATH_MARKER in (box.home / ".profile").read_text()
        assert out.count("Add to PATH in") == 1
        rc, out = interactive_install(box, [
            {"q": "Install Maverick to", "a": "\r"},
            {"q": "Overwrite?", "a": "\r"}])
        assert rc == 0, (rc, out[-3000:])
        assert "Add to PATH in" not in out, "a block already present was asked about"
        assert "already configured in" in out, out[-3000:]
        for path in startup_files(box):
            if path.is_file():
                assert path.read_text().count(PATH_MARKER) <= 1, path
    # --add-path is the same edit with the question removed.
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build()
        rc, out = interactive_install(box, [{"q": "Install Maverick to", "a": "\r"}],
                                      args=("--add-path",))
        assert rc == 0, (rc, out[-3000:])
        assert "Add to PATH in" not in out, "the question was asked anyway"
        assert PATH_MARKER in (box.home / ".profile").read_text()
    print("PASS: the interactive PATH prompt asks, accepts and refuses correctly")


def suite_prefix_is_a_boundary():
    """Nothing outside the chosen prefix may be created or modified.

    The two documented exceptions are both the caller's own files: the config
    under ~/.config, and the marked PATH block written under $HOME
    (covered by the suites above). Everything a system path holds is watched.
    """
    watched = [Path("/usr/local/bin"), Path("/usr/share/xsessions"),
               Path("/usr/local/share/xsessions")]
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build()
        before = box.snapshot(*watched)
        local_bin_before = sorted(p.name for p in (box.home / ".local/bin").glob("*") ) \
            if (box.home / ".local/bin").is_dir() else None
        result = box.install("--prefix", str(box.prefix), "--no-build")
        check_success(box, result)
        assert box.snapshot(*watched) == before, "installer modified a path outside its prefix"
        local_bin_after = sorted(p.name for p in (box.home / ".local/bin").glob("*") ) \
            if (box.home / ".local/bin").is_dir() else None
        assert local_bin_after == local_bin_before, "installer wrote into $HOME/.local"
        print("PASS: a custom prefix writes nothing outside it")


def suite_rejects_unwritable_prefix():
    """A prefix we cannot write is an error, never a silent escalation."""
    watched = [Path("/usr/local/bin/maverick"), Path("/usr/local/bin/maverickctl"),
               Path("/usr/share/xsessions")]
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build()
        before = box.snapshot(*watched)
        result = box.install("--system", "--no-build")
        assert result.returncode != 0, (result.stdout, result.stderr)
        combined = result.stdout + result.stderr
        assert "sudo" in combined or "cannot write" in combined, combined[-2000:]
        assert "sudo " not in box.calls, f"installer invoked sudo: {box.calls}"
        # A pre-existing installation is left exactly as it was: the installer
        # neither replaced nor removed anything on its way to failing.
        assert box.snapshot(*watched) == before, "system install touched a root-owned path"
        print("PASS: unwritable system prefix fails clearly without sudo")


def suite_broken_binary_is_caught():
    """A stub that cannot answer the session probe must fail the install."""
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build(STUB_NO_SESSIONS)
        result = box.install("--prefix", str(box.prefix), "--no-build")
        assert result.returncode != 0, (
            "installer reported success for a binary that lacks Sessions",
            result.stdout[-2000:], result.stderr[-2000:])
        assert "sudo " not in box.calls
        print("PASS: a binary without Sessions is rejected, not reported installed")


def suite_no_partial_install():
    """A destination that blocks one binary must leave the old set intact."""
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build()
        assert box.install("--prefix", str(box.prefix), "--no-build").returncode == 0
        first = {b: (box.prefix / "bin" / b).read_bytes() for b in BINS}
        # A new build, distinguishable from what is installed.
        box.seed_build(STUB_OK.replace("0.0.0-test", "9.9.9-new"))
        # Make the second binary's destination impossible to replace.
        blocker = box.prefix / "bin/maverickctl"
        blocker.unlink()
        blocker.mkdir()
        result = box.install("--prefix", str(box.prefix), "--no-build")
        assert result.returncode != 0, (result.stdout[-2000:], result.stderr[-2000:])
        # The critical property: maverick was NOT replaced, so the prefix is
        # not left holding a new binary beside an old one.
        assert (box.prefix / "bin/maverick").read_bytes() == first["maverick"], \
            "partial install: maverick was replaced before maverickctl failed"
        assert not any(p.name.startswith(".maverick-stage") for p in (box.prefix / "bin").iterdir()), \
            "staging directory left behind"
        print("PASS: a failed install leaves the previous set untouched")


def suite_repeat_and_overwrite():
    """Installing twice converges, and fixes permissions on the way."""
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build()
        assert box.install("--prefix", str(box.prefix), "--no-build").returncode == 0
        first = {b: (box.prefix / "bin" / b).read_bytes() for b in BINS}
        (box.home / ".local/share/xsessions").mkdir(parents=True, exist_ok=True)
        # Umask-hostile modes must not survive a reinstall.
        for binary in BINS:
            (box.prefix / "bin" / binary).chmod(0o600)
        (box.prefix / "share/xsessions/maverick.desktop").chmod(0o600)
        result = box.install("--prefix", str(box.prefix), "--no-build")
        check_success(box, result)
        for binary in BINS:
            installed = box.prefix / "bin" / binary
            assert installed.read_bytes() == first[binary], f"{binary} changed on reinstall"
            assert installed.stat().st_mode & 0o777 == 0o755
        assert (box.prefix / "share/xsessions/maverick.desktop").stat().st_mode & 0o777 == 0o644
        leftovers = [p.name for p in (box.prefix / "bin").iterdir() if p.name.startswith(".")]
        assert not leftovers, f"temporary files left in bin/: {leftovers}"
        print("PASS: repeat install is idempotent and repairs permissions")


def suite_builds_from_source():
    """The build path runs cargo against the requested target directory."""
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        result = box.install("--prefix", str(box.prefix))
        assert result.returncode == 0, (result.stdout[-2000:], result.stderr[-2000:])
        check_success(box, result)
        assert "cargo build" in box.calls, box.calls
        assert f"cargo target {box.target}" in box.calls, box.calls
        assert "cargo uid=0" not in box.calls, "cargo ran as root"
        print("PASS: build path produces and installs the release set")


def suite_obsolete_artifacts_absent():
    """The build must not produce an obsolete binary, and must not install one."""
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        result = box.install("--prefix", str(box.prefix))
        assert result.returncode == 0, (result.stdout[-2000:], result.stderr[-2000:])
        for stale in OBSOLETE:
            assert not (box.prefix / "bin" / stale).exists(), f"{stale} was installed"
        built = subprocess.run(
            ["cargo", "metadata", "--offline", "--no-deps", "--format-version", "1"],
            cwd=REPO, capture_output=True, text=True,
            env={**os.environ, "CARGO_TARGET_DIR": "/tmp/maverick-target"})
        assert built.returncode == 0, built.stderr
        for stale in OBSOLETE:
            assert stale not in built.stdout, f"{stale} is still a declared build target"
        print("PASS: obsolete binaries are neither built nor installed")


def suite_prefix_with_spaces():
    """A prefix containing spaces must work end to end.

    The installer's verification executes the installed binaries, so anything
    that word-splits a path silently breaks here. This suite is the guard for
    quoting in the install path.
    """
    with tempfile.TemporaryDirectory(prefix="maverick-install-test-") as d:
        box = Sandbox(Path(d))
        box.seed_build()
        prefix = box.base / "prefix with space"
        result = box.install("--prefix", str(prefix), "--no-build",
                             env=box.env(SHELL="/bin/bash"))
        check_success(box, result, expect_prefix=prefix)
        for binary in BINS:
            assert (prefix / "bin" / binary).is_file()
        assert f'Exec="{prefix}/bin/maverick"' in \
            (prefix / "share/xsessions/maverick.desktop").read_text()
        # The PATH block writes the directory into a file a shell parses, so a
        # directory with spaces has to survive that too — quoting here is the
        # difference between one word and three.
        profile = box.home / ".profile"
        assert profile.is_file()
        probe = subprocess.run(
            ["sh", "-c", f'. "{profile}"; command -v maverick'],
            capture_output=True, text=True,
            env={"HOME": str(box.home), "PATH": "/usr/bin:/bin"})
        assert probe.returncode == 0, (probe.stdout, probe.stderr)
        assert probe.stdout.strip() == str(prefix / "bin" / "maverick"), probe.stdout
        print("PASS: a prefix containing spaces installs and verifies")


def suite_cli_contract():
    for option in ("--prefix", "--lang", "--xsessions-dir"):
        result = subprocess.run(["bash", str(ENTRY), option],
                                capture_output=True, text=True)
        assert result.returncode == 2, (option, result.returncode)
    unknown = subprocess.run(["bash", str(ENTRY), "--nope"],
                             capture_output=True, text=True)
    assert unknown.returncode == 2
    helped = subprocess.run(["bash", str(ENTRY), "--help"],
                            capture_output=True, text=True)
    assert helped.returncode == 0
    assert "$HOME/.local" in helped.stdout
    assert "--system" in helped.stdout
    assert "--add-path" in helped.stdout
    assert "--no-path" in helped.stdout
    # An option that takes no value must reject one rather than swallow it.
    for option in ("--add-path", "--no-path"):
        result = subprocess.run(["bash", str(ENTRY), option, "--nope"],
                                capture_output=True, text=True)
        assert result.returncode == 2, (option, result.returncode)
    print("PASS: CLI argument contract")


def main():
    assert os.getuid() != 0, "Run this test as a normal user"
    suites = [
        suite_local_install,
        suite_default_prefix_is_user_local,
        suite_path_setup,
        suite_path_opt_out,
        suite_path_respects_existing_config,
        suite_path_follows_shell,
        suite_path_interactive,
        suite_prefix_is_a_boundary,
        suite_rejects_unwritable_prefix,
        suite_broken_binary_is_caught,
        suite_no_partial_install,
        suite_repeat_and_overwrite,
        suite_builds_from_source,
        suite_prefix_with_spaces,
        suite_obsolete_artifacts_absent,
        suite_cli_contract,
    ]
    only = sys.argv[1] if len(sys.argv) > 1 else None
    for suite in suites:
        if only and only not in suite.__name__:
            continue
        suite()
    print("install-smoke: all suites passed")


if __name__ == "__main__":
    main()
