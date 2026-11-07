//! The checker must diagnose configuration without entering an X11 session.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "maverick-check-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).expect("private fixture directory");
        Self(dir)
    }

    fn write(&self, source: &str) -> PathBuf {
        let path = self.0.join("config.toml");
        std::fs::write(&path, source).unwrap();
        path
    }

    fn check(&self, path: &Path) -> Output {
        Command::new(env!("CARGO_BIN_EXE_maverick"))
            .args(["--log-level", "warn", "--check-config"])
            .arg(path)
            .env("DISPLAY", "invalid-display-for-checker")
            .env("XDG_CONFIG_HOME", &self.0)
            .env("XDG_RUNTIME_DIR", &self.0)
            .output()
            .expect("run checker")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn checker_exit_codes_and_diagnostics_follow_the_loader_policy() {
    let fixture = Fixture::new();
    // A missing explicit path is a clean fallback under the existing contract.
    assert!(fixture
        .check(&fixture.0.join("missing.toml"))
        .status
        .success());
    for (source, code, diagnostic) in [
        ("[general]\ncolumn_width = 0.1\n", 0, ""),
        ("[general]\ncolumn_width = 1.0\n", 0, ""),
        ("[general\ngaps_inner = 4\n", 1, "invalid TOML"),
        ("[general]\ngaps_inner = -0x10\n", 1, "invalid TOML"),
        ("[general]\ncolumn_width = 1.0000000001\n", 1, "line 2:"),
        ("[general]\nfocus_mouse = 1\n", 1, "expected a boolean"),
        ("[general]\ngaps_iner = 3\n", 1, "did you mean"),
        ("[rules]\nclass = \"x\"\n", 1, "[[rules]]"),
        (
            "[[keybindings]]\nkey = \"Super++a\"\naction = \"kill\"\n",
            1,
            "invalid key combination",
        ),
        (
            "[[keybindings]]\nkey = \"Super+a\"\naction = \"shrink-col -2147483648\"\n",
            1,
            "invalid action",
        ),
        (
            "[[keybindings]]\nkey = true\naction = \"kill\"\n",
            1,
            "line 2:",
        ),
        ("[future_feature]\nenabled = true\n", 0, ""),
    ] {
        let path = fixture.write(source);
        let output = fixture.check(&path);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(code), "{source}: {stderr}");
        assert!(stderr.contains(diagnostic), "{source}: {stderr}");
        if code == 1 {
            assert!(stderr.contains(path.to_str().unwrap()), "{stderr}");
        }
    }
}

#[test]
fn checker_does_not_run_autostart_or_spawn_bindings() {
    let fixture = Fixture::new();
    let autostart_marker = fixture.0.join("autostart-ran");
    let spawn_marker = fixture.0.join("spawn-ran");
    let path = fixture.write(&format!(
        "[autostart]\ncommands = [[\"touch\", \"{}\"]]\n\
         [[keybindings]]\nkey = \"Super+a\"\naction = \"spawn:touch {}\"\n",
        autostart_marker.display(),
        spawn_marker.display()
    ));
    let output = fixture.check(&path);
    assert!(output.status.success(), "{output:?}");
    assert!(!autostart_marker.exists());
    assert!(!spawn_marker.exists());
    assert!(String::from_utf8_lossy(&output.stdout).contains("config OK"));
}
