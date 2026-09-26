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
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
BINS = ("maverick", "maverickctl")
# Obsolete artifacts that must never reappear in an install.
OBSOLETE = ("maverick-setup", "maverick-msg")

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
        shutil.copy2(ROOT / "install.sh", self.repo / "install.sh")
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
        command = ["bash", str(self.repo / "install.sh"), "--yes", "--no-anim", "--lang", "en"]
        command += list(args)
        result = subprocess.run(command, env=env or self.env(), capture_output=True,
                                text=True, timeout=timeout)
        self.calls = self.log.read_text() if self.log.exists() else ""
        return result

    def snapshot(self, *paths):
        return {p: (p.stat().st_mtime_ns, p.read_bytes() if p.is_file() else None)
                for p in paths if p.exists()}


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


def suite_prefix_is_a_boundary():
    """Nothing outside the chosen prefix may be created or modified."""
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
            cwd=ROOT, capture_output=True, text=True,
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
        result = box.install("--prefix", str(prefix), "--no-build")
        check_success(box, result, expect_prefix=prefix)
        for binary in BINS:
            assert (prefix / "bin" / binary).is_file()
        assert f'Exec="{prefix}/bin/maverick"' in \
            (prefix / "share/xsessions/maverick.desktop").read_text()
        print("PASS: a prefix containing spaces installs and verifies")


def suite_cli_contract():
    for option in ("--prefix", "--lang", "--xsessions-dir"):
        result = subprocess.run(["bash", str(ROOT / "install.sh"), option],
                                capture_output=True, text=True)
        assert result.returncode == 2, (option, result.returncode)
    unknown = subprocess.run(["bash", str(ROOT / "install.sh"), "--nope"],
                             capture_output=True, text=True)
    assert unknown.returncode == 2
    helped = subprocess.run(["bash", str(ROOT / "install.sh"), "--help"],
                            capture_output=True, text=True)
    assert helped.returncode == 0
    assert "$HOME/.local" in helped.stdout
    assert "--system" in helped.stdout
    print("PASS: CLI argument contract")


def main():
    assert os.getuid() != 0, "Run this test as a normal user"
    suites = [
        suite_local_install,
        suite_default_prefix_is_user_local,
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
