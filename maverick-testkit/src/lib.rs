//! Isolated X11 test processes. This crate is not part of the shipped WM.
//!
//! The private Xvfb display is obtained from `-displayfd` rather than a
//! guessed display number. Children inherit no desktop display, Xauthority
//! cookie, Maverick session variables, or real HOME/XDG directories.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tempfile::{Builder, TempDir};

const SERVER_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_DISPLAY_DIGITS: usize = 16;

/// One subprocess owned by this test. Never signals an arbitrary PID.
struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// An owned Xvfb and optional WM. Fields drop in WM, server, sandbox order.
pub struct XvfbSession {
    wm: Option<OwnedChild>,
    _server: OwnedChild,
    _sandbox: TempDir,
    display: String,
    config: PathBuf,
    root: PathBuf,
}

impl XvfbSession {
    /// Spawn one private X11 server, never selecting or probing a host display.
    pub fn start() -> io::Result<Self> {
        let sandbox = Builder::new().prefix("maverick-test-").tempdir()?;
        let root = sandbox.path().to_path_buf();
        for name in ["home", "runtime", "config", "state", "cache"] {
            fs::create_dir(root.join(name))?;
        }

        let config = root.join("maverick.toml");
        fs::write(
            &config,
            "[general]\nfocus_mouse = false\nwarp_cursor = false\n[autostart]\ncommands = []\n",
        )?;

        let mut command = isolated_command(&root, None, OsStr::new("Xvfb"));
        command.args([
            "-displayfd",
            "1",
            "-screen",
            "0",
            "800x600x24",
            "-nolisten",
            "tcp",
        ]);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(File::create(root.join("xvfb.log"))?));

        let mut server = OwnedChild(command.spawn()?);
        let output = server
            .0
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing Xvfb display pipe"))?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            let _ = sender.send(read_display_record(output));
        });

        let result = match receiver.recv_timeout(SERVER_STARTUP_TIMEOUT) {
            Ok(result) => result,
            Err(error) => {
                // Terminate only the child we spawned before joining its
                // possibly blocked stdout reader.
                drop(server);
                let _ = reader.join();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("Xvfb did not report an isolated display: {error}"),
                ));
            }
        };
        let _ = reader.join();
        let display = result?;

        Ok(Self {
            wm: None,
            _server: server,
            _sandbox: sandbox,
            display,
            config,
            root,
        })
    }

    /// The private display name, e.g. `:93`.
    pub fn display(&self) -> &str {
        &self.display
    }

    /// Start the requested WM binary without passing control-plane variables.
    pub fn launch_wm(&mut self, binary: &Path) -> io::Result<()> {
        if self.wm.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "test session already owns a window manager",
            ));
        }
        let mut command = isolated_command(&self.root, Some(&self.display), binary.as_os_str());
        command.arg("--config").arg(&self.config);
        command.arg("--session-id").arg("rust-testkit");
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(File::create(self.root.join("maverick.log"))?));
        self.wm = Some(OwnedChild(command.spawn()?));
        Ok(())
    }

    /// Return the observed WM exit, if any, without reaping unrelated children.
    pub fn wm_exit_status(&mut self) -> io::Result<Option<ExitStatus>> {
        match &mut self.wm {
            Some(wm) => wm.0.try_wait(),
            None => Ok(None),
        }
    }
}

fn isolated_command(root: &Path, display: Option<&str>, binary: &OsStr) -> Command {
    let mut command = Command::new(binary);
    for (key, _) in env::vars_os() {
        let key_text = key.to_string_lossy();
        if key_text.starts_with("MAVERICK_")
            || matches!(
                key_text.as_ref(),
                "DISPLAY"
                    | "XAUTHORITY"
                    | "WAYLAND_DISPLAY"
                    | "HOME"
                    | "XDG_RUNTIME_DIR"
                    | "XDG_CONFIG_HOME"
                    | "XDG_STATE_HOME"
                    | "XDG_CACHE_HOME"
            )
        {
            command.env_remove(key);
        }
    }
    for (key, folder) in [
        ("HOME", "home"),
        ("XDG_RUNTIME_DIR", "runtime"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
    ] {
        command.env(key, root.join(folder));
    }
    if let Some(display) = display {
        command.env("DISPLAY", display);
    }
    command
}

fn read_display_record(mut input: impl Read) -> io::Result<String> {
    let mut number = Vec::with_capacity(4);
    loop {
        let mut byte = [0];
        input.read_exact(&mut byte)?;
        match byte[0] {
            b'\n' if !number.is_empty() => {
                let number = String::from_utf8(number)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                return Ok(format!(":{number}"));
            }
            b'0'..=b'9' if number.len() < MAX_DISPLAY_DIGITS => number.push(byte[0]),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid or oversized Xvfb display record",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::read_display_record;
    use std::io::ErrorKind;

    #[test]
    fn display_record_accepts_only_a_bounded_decimal_line() {
        assert_eq!(read_display_record(&b"93\n"[..]).unwrap(), ":93");
        for bad in [
            &b"\n"[..],
            &b"a1\n"[..],
            &b"93x\n"[..],
            &b"12345678901234567\n"[..],
        ] {
            assert_eq!(
                read_display_record(bad).unwrap_err().kind(),
                ErrorKind::InvalidData
            );
        }
        assert_eq!(
            read_display_record(&b"21"[..]).unwrap_err().kind(),
            ErrorKind::UnexpectedEof
        );
    }
}
