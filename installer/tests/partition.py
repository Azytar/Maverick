#!/usr/bin/env python3
"""The installer's test suite: installer/ against the contract it makes.

Three groups of tests:

1. The smoke suite, rerun unmodified against installer/install.sh.
   tests/install-smoke.py is imported as a module; it already copies
   installer/ into its sandbox, so every behavioural promise the installer
   makes is re-checked here without editing the original harness.

2. Properties only a partition can express: sourcing the libraries does
   nothing, the two message tables are in parity, every key the installer asks
   for exists, and a checkout missing its lib/ says so instead of dying with an
   unbound variable.

3. A checked-in golden of the plain output, so a wording change shows up as a
   reviewable diff instead of a surprise.

installer/install.sh is the installer, so its output is the reference.

Run:  python3 installer/tests/partition.py [name-substring]
      MAVERICK_UPDATE_GOLDEN=1 python3 installer/tests/partition.py golden
"""
import importlib.util
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent      # installer/
ROOT = HERE.parent                                 # repository root
ENTRY = HERE / "install.sh"
LIB = HERE / "lib"
GOLDEN = HERE / "golden"


def load_smoke():
    # Imported, not run: the original harness stays byte-for-byte untouched.
    # Bytecode stays in memory so the import leaves nothing behind in tests/.
    sys.dont_write_bytecode = True
    path = ROOT / "tests" / "install-smoke.py"
    spec = importlib.util.spec_from_file_location("install_smoke", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


smoke = load_smoke()


# ── output normalisation ─────────────────────────────────────────────────────
def normalize(text, box):
    """Strip everything that depends on where the sandbox happened to land."""
    pairs = {}
    for token, path in (("BASE", box.base), ("HOME", box.home), ("PREFIX", box.prefix),
                        ("TARGET", box.target), ("REPO", box.repo)):
        for form in (str(path), os.path.realpath(str(path))):
            pairs[form] = f"<{token}>"
    for form in sorted(pairs, key=len, reverse=True):
        text = text.replace(form, pairs[form])
    text = re.sub(r"/tmp/maverick[-._A-Za-z0-9]*", "<TMP>", text)
    # Step timings and the panel's elapsed time.
    text = re.sub(r"\d+\.\d+s", "<TIME>", text)
    # The distribution name is the host's, not the installer's.
    text = re.sub(r"(?m)^(.*\bdistro\b  ).+$", r"\1<DISTRO>", text)
    # wc is absent from very small images; the warning is host state, not code.
    text = "\n".join(line for line in text.split("\n")
                     if "alignment may be imprecise" not in line
                     and "alineación puede ser imprecisa" not in line)
    return text


def tree_of(prefix):
    """Relative path and mode of every file the installer put in the prefix."""
    rows = []
    for path in sorted(prefix.rglob("*")):
        rows.append(f"{path.relative_to(prefix)} {path.stat().st_mode & 0o777:o}")
    return rows


def run_install(*args, **env):
    """One clean install in its own sandbox, as this tree produces it."""
    with tempfile.TemporaryDirectory(prefix="maverick-partition-") as d:
        box = smoke.Sandbox(Path(d))
        box.seed_build()
        environment = box.env(SHELL="/bin/bash")
        environment.update(env)
        result = box.install(*args, env=environment)
        return (
            result.returncode,
            normalize(result.stdout, box),
            normalize(result.stderr, box),
            tree_of(box.prefix),
        )


def plain_snapshot(lang, **env):
    """The plain output of one --no-build install, in the shape golden/ records.

    Both the golden comparison and the locale comparison want the same thing,
    and a snapshot that is built twice from two places is a snapshot that will
    quietly stop being the same thing.
    """
    rc, out, err, _tree = run_install("--no-build", "--lang", lang, **env)
    assert rc == 0, (lang, env, out[-2000:], err[-2000:])
    return out if not err.strip() else out + "\n--- stderr ---\n" + err


def differing(expected, got):
    """The lines of a snapshot comparison, in the form the golden check uses."""
    return "\n".join(f"- {l}\n+ {r}" for l, r in
                     zip(expected.split("\n"), got.split("\n")) if l != r)


# ── 1. the smoke suite, unmodified ────────────────────────────────────────────
# The contract suite names the installer by path, so it is listed here by the
# name the smoke module exports; nothing else needs repointing.
ORIGINAL_SUITES = [
    smoke.suite_local_install,
    smoke.suite_default_prefix_is_user_local,
    smoke.suite_path_setup,
    smoke.suite_path_opt_out,
    smoke.suite_path_respects_existing_config,
    smoke.suite_path_follows_shell,
    smoke.suite_path_interactive,
    smoke.suite_prefix_is_a_boundary,
    smoke.suite_rejects_unwritable_prefix,
    smoke.suite_broken_binary_is_caught,
    smoke.suite_no_partial_install,
    smoke.suite_repeat_and_overwrite,
    smoke.suite_builds_from_source,
    # The live progress loop needs a tty and a real build, and answers no
    # question: --yes pre-answers the prompts the harness would otherwise have
    # to type. It is a gate suite, not a thing a human has to sit through.
    smoke.suite_interactive_build_progress,
    smoke.suite_prefix_with_spaces,
    smoke.suite_obsolete_artifacts_absent,
    smoke.suite_cli_contract,
]


# ── 2. partition properties ──────────────────────────────────────────────────
def bash_script(body, drop=(), **env):
    """Run a snippet under the same shell settings the installer runs with."""
    environment = {k: v for k, v in os.environ.items() if k not in drop}
    environment.update(env)
    return subprocess.run(["bash", "-c", body], capture_output=True, text=True,
                          env=environment)


def suite_libraries_are_inert():
    """Sourcing a library must define things, and do nothing else.

    This is the property that makes the rest of the partition safe: if reading
    lib/setup.sh never reads $HOME, never writes, and never asks, then the
    order in which install.sh composes its own state is a choice rather than a
    hazard. set -u is on throughout, so an unguarded variable is a failure and
    not a surprise later.
    """
    body = f"""
set -euo pipefail
. {LIB}/i18n.sh
. {LIB}/ui.sh
. {LIB}/setup.sh
printf 'defined\\n'
names=( t i18n_init i18n_audit ui_init ui_cursor_restore prompt ask is_yes die
        banner _panel_draw path_setup path_block ensure_rust check_disk_space
        check_x11_linkable os_detect check_unsupported_os )
for name in "${{names[@]}}"; do
    [[ "$(type -t "$name")" == function ]] || {{ echo "missing: $name" >&2; exit 1; }}
done
LANG_CHOICE=en
i18n_init
ui_init
os_detect
[[ "$LANG_ID" == en ]]
[[ -n "$OS_ID" && -n "$DISTRO_ID" ]]
printf 'initialised\\n'
"""
    with tempfile.TemporaryDirectory(prefix="maverick-libtest-") as d:
        home = Path(d) / "home"
        home.mkdir()
        result = bash_script(body, HOME=str(home))
        assert result.returncode == 0, (result.stdout, result.stderr)
        assert result.stdout == "defined\ninitialised\n", result.stdout
        assert result.stderr == "", result.stderr
        assert list(home.iterdir()) == [], list(home.iterdir())
    # The init block may assume HOME; sourcing may not. Reading the libraries
    # with HOME unset is the guard against a stray $HOME creeping back in.
    result = bash_script(body.replace("printf 'initialised\\n'", "true"), drop=("HOME",))
    assert result.returncode == 0, (result.stdout, result.stderr)
    assert result.stdout == "defined\n", result.stdout
    print("PASS: sourcing the libraries defines, and nothing else")


def suite_i18n_audit():
    """The two tables are in parity and cover every key the installer asks for."""
    result = subprocess.run(["bash", str(LIB / "i18n.sh")], capture_output=True, text=True)
    assert result.returncode == 0, (result.stdout, result.stderr)
    assert "parity" in result.stdout, result.stdout
    assert "never defined" not in result.stderr, result.stderr
    # And the same audit runs as a function, from inside the installer.
    body = f"""
set -euo pipefail
. {LIB}/i18n.sh
i18n_audit >/dev/null
LANG_CHOICE=es; i18n_init
[[ "$(t aborted)" == "cancelado." ]]
LANG_CHOICE=en; i18n_init
[[ "$(t aborted)" == "aborted." ]]
[[ "$(t no_such_key)" == "no_such_key" ]]
"""
    result = bash_script(body)
    assert result.returncode == 0, (result.stdout, result.stderr)
    print("PASS: i18n tables in parity, unknown keys visible, no key unused silently")


def suite_missing_library_is_reported():
    """A checkout without lib/ fails at the top, in one line, not halfway."""
    with tempfile.TemporaryDirectory(prefix="maverick-libtest-") as d:
        box = smoke.Sandbox(Path(d))
        shutil.rmtree(box.repo / "installer/lib")
        result = subprocess.run(["bash", str(box.repo / "installer/install.sh")],
                                env=box.env(), capture_output=True, text=True)
        assert result.returncode == 1, (result.returncode, result.stdout)
        assert "incomplete checkout" in result.stderr, result.stderr
        assert "i18n.sh" in result.stderr, result.stderr
        assert result.stdout == "", result.stdout
    print("PASS: an incomplete checkout is reported at the top, not discovered later")


def suite_syntax():
    """Every file parses, and shellcheck (where present) finds no errors."""
    files = [ENTRY] + sorted(LIB.glob("*.sh"))
    for path in files:
        result = subprocess.run(["bash", "-n", str(path)], capture_output=True, text=True)
        assert result.returncode == 0, (path, result.stderr)
    shellcheck = shutil.which("shellcheck")
    if shellcheck:
        result = subprocess.run([shellcheck, "-x", "-S", "error", *map(str, files)],
                                capture_output=True, text=True)
        assert result.returncode == 0, result.stdout + result.stderr
        print("PASS: bash -n and shellcheck -S error on entry + 3 libraries")
    else:
        print("PASS: bash -n on entry + 3 libraries (shellcheck not installed here)")


def suite_build_command_names_the_release_set():
    """The build must ask cargo for exactly the binaries the installer installs.

    `-p maverick` alone compiles the window manager and not maverickctl, so the
    install then fails at its own artifact check over a binary it just declined
    to build. The harness's cargo produces both files whatever -p says, so the
    only place the command line is visible is the log it records.

    The same assertion covers the feature plumbing: the maverick package has no
    default feature and no compositor, so a --features or --no-default-features
    on the command line is by definition a feature Maverick does not have.
    """
    with tempfile.TemporaryDirectory(prefix="maverick-buildcmd-") as d:
        box = smoke.Sandbox(Path(d))
        result = box.install("--prefix", str(box.prefix))
        assert result.returncode == 0, (result.stdout[-2000:], result.stderr[-2000:])
        builds = [line for line in box.calls.splitlines() if line.startswith("cargo build")]
        assert builds, f"no cargo build was recorded:\n{box.calls}"
        for line in builds:
            assert "-p maverick" in line, f"build does not name maverick: {line}"
            assert "-p maverickctl" in line, f"build does not name maverickctl: {line}"
            assert "--release" in line, f"build is not a release build: {line}"
            assert "--features" not in line, f"build names features: {line}"
            assert "--no-default-features" not in line, \
                f"build disables a default feature that does not exist: {line}"
    print("PASS: the build command names maverick and maverickctl, and no features")


def suite_golden_output():
    """A checked-in snapshot of the plain output, in both languages.

    With no monolith left to compare against, this snapshot is the only record
    of what the installer says: a wording change becomes a reviewable diff.
    Regenerate with MAVERICK_UPDATE_GOLDEN=1 and read the diff.
    """
    update = os.environ.get("MAVERICK_UPDATE_GOLDEN") == "1"
    GOLDEN.mkdir(exist_ok=True)
    for lang in ("en", "es"):
        snapshot = plain_snapshot(lang)
        path = GOLDEN / f"install.{lang}.txt"
        if update:
            path.write_text(snapshot)
            continue
        assert path.exists(), f"{path} missing — run with MAVERICK_UPDATE_GOLDEN=1"
        expected = path.read_text()
        if expected != snapshot:
            raise AssertionError(f"{path.name} changed:\n{differing(expected, snapshot)}\n"
                                 f"(MAVERICK_UPDATE_GOLDEN=1 to accept)")
    if update:
        print("WROTE: golden/install.en.txt, golden/install.es.txt")
    else:
        print("PASS: plain output matches the golden in en and es")


def suite_layout_is_locale_independent():
    """The panel has to come out the same width in every locale.

    Column padding is measured in characters, and both obvious ways of counting
    them quietly return bytes outside a UTF-8 locale: `wc -m` does, and so does
    ${#str}. The em dash in the summary title is then three columns wide instead
    of one, so every row of the panel is padded two columns short and the box
    no longer closes. Nothing on a UTF-8 machine can see that, and a build
    machine is routinely LC_ALL=C — so the locales are named here rather than
    inherited, or the property would only be tested by whoever happened to run
    the gate in the right locale.

    The golden is the recorded UTF-8 rendering, so each byte locale has to
    reproduce it exactly; C.UTF-8 is asserted against it too, which also makes
    a host with no UTF-8 locale at all report that instead of blaming the
    layout.
    """
    for lang in ("en", "es"):
        golden = (GOLDEN / f"install.{lang}.txt").read_text()
        reference = plain_snapshot(lang, LC_ALL="C.UTF-8")
        assert reference == golden, \
            f"LC_ALL=C.UTF-8 no longer reproduces install.{lang}.txt " \
            f"(no UTF-8 locale here?):\n{differing(golden, reference)}"
        for locale in ("C", "POSIX"):
            got = plain_snapshot(lang, LC_ALL=locale)
            if got != reference:
                raise AssertionError(
                    f"LC_ALL={locale} laid the {lang} output out differently "
                    f"from C.UTF-8:\n{differing(reference, got)}")
    print("PASS: panel layout is identical under LC_ALL=C, POSIX and C.UTF-8")


SUITES = [
    *ORIGINAL_SUITES,
    suite_libraries_are_inert,
    suite_i18n_audit,
    suite_missing_library_is_reported,
    suite_syntax,
    suite_build_command_names_the_release_set,
    suite_golden_output,
    suite_layout_is_locale_independent,
]


def main():
    assert os.getuid() != 0, "Run this test as a normal user"
    only = sys.argv[1] if len(sys.argv) > 1 else None
    for suite in SUITES:
        if only and only not in suite.__name__:
            continue
        suite()
    print("partition: all suites passed")


if __name__ == "__main__":
    main()
