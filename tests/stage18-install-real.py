#!/usr/bin/env python3
"""Install release artifacts into private prefixes without touching the desktop."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def main():
    with tempfile.TemporaryDirectory(prefix="maverick-real-install-") as scratch:
        root = Path(scratch)
        env = {k: v for k, v in os.environ.items()
               if not k.startswith(("MAVERICK_", "XDG_", "CARGO_"))}
        for key in ("HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR", "XDG_STATE_HOME"):
            path = root / key
            path.mkdir(mode=0o700)
            env[key] = str(path)
        config = Path(env["XDG_CONFIG_HOME"]) / "maverick/config.toml"
        config.parent.mkdir()
        config.write_text("# personal configuration\n[general]\nview_count = 5\n")
        original = config.read_bytes()
        prefix = root / "prefix"

        def install(profile):
            target = ROOT / ("target/minimal" if profile == "minimal" else "target")
            result = subprocess.run(
                ["bash", str(ROOT / "installer/install.sh"), "--yes", "--no-anim",
                 "--no-build", "--no-path", "--prefix", str(prefix), "--" +
                 ("minimal" if profile == "minimal" else "with-control")],
                env=dict(env, CARGO_TARGET_DIR=str(target.resolve())), text=True,
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=30)
            print(result.stdout, flush=True)
            assert result.returncode == 0, result.returncode
            assert config.read_bytes() == original
            assert subprocess.check_output([str(prefix / "bin/maverick"), "--build-profile"],
                                           text=True).strip() == profile

        install("minimal")
        client = prefix / "bin/maverickctl"
        assert not client.exists()
        print("PASS: real Minimal artifact installs alone and preserves personal config", flush=True)
        install("control")
        assert client.read_bytes() == (ROOT / "target/release/maverickctl").read_bytes()
        print("PASS: control installs the independently built pinned client", flush=True)
        install("minimal")
        assert not client.exists()
        print("PASS: migration removes an unchanged installer-owned client", flush=True)
        install("control")
        client.write_bytes(client.read_bytes() + b"\n# personal modification\n")
        before = hashlib.sha256(client.read_bytes()).digest()
        install("minimal")
        assert hashlib.sha256(client.read_bytes()).digest() == before
        print("PASS: modified client survives control-to-Minimal migration", flush=True)
        client.write_text("#!/bin/sh\n# foreign client\nexit 0\n")
        before = client.read_bytes()
        install("minimal")
        assert client.read_bytes() == before
        print("PASS: foreign client and personal config survive repeated Minimal installation", flush=True)


if __name__ == "__main__":
    main()
