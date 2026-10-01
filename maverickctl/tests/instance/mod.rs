//! A fake Maverick instance a test can stand up, and take down again.
//!
//! `maverickctl` reaches an instance through two things in the runtime
//! directory: a `control.sock` it connects to, and an identity record that
//! discovery reads to decide the instance exists. A test that wants the tool to
//! talk to *something* has to publish at least one of them, and anything left
//! published is a Maverick that no test owns — the tool resolves it, connects to
//! it, and answers a question about a session nobody asked about.
//!
//! So the handle owns what it published. Dropping it signals the worker, joins
//! it, and unlinks what it created, in that order, so nothing running afterwards
//! can find an instance whose listener is still answering.
//!
//! What each test *answers* is not shared — one replies `ok` to everything, one
//! replies a fixed refusal, one routes by the request line, one closes without
//! replying at all — so only the ownership lives here.

use maverick_sys::identity::InstanceInfo;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixListener;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

/// What a fixture publishes, and therefore where reaching it fails.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Published {
    /// A socket and a record: an instance the tool can find and talk to.
    Instance,
    /// A socket and no record. Discovery has nothing to read, so the tool fails
    /// at resolution — a peer that is there and cannot be addressed as one.
    SocketOnly,
    /// A record and no socket. Discovery selects it and the connect is refused —
    /// an instance that is there and cannot be reached.
    RecordOnly,
}

/// A published instance and the worker serving it.
pub struct Instance {
    sid: String,
    published: Published,
    stop: Option<Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl Instance {
    /// Publish `sid` and serve it with `reply`, as `published` describes.
    ///
    /// `budget` caps how many connections are served before the worker stops
    /// answering; `None` serves for as long as the handle is held. The cap
    /// exists where several fixtures share one runtime directory: resolving any
    /// one of them makes the client ping *all* of them, so a worker that ran
    /// dry would have its own test fail as "connection refused" — passing for
    /// the wrong reason.
    pub fn serve(
        sid: &str,
        published: Published,
        budget: Option<usize>,
        reply: impl Fn(&str) -> Option<String> + Send + 'static,
    ) -> Self {
        let mut instance = Instance {
            sid: sid.to_string(),
            published,
            stop: None,
            worker: None,
        };

        if published != Published::RecordOnly {
            let path = maverick_sys::identity::sock_path(sid);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create fixture session dir");
            }
            // Only a socket of this session's own making is removed here: a
            // listener still holding the path is a live instance, and unlinking
            // it would leave a process serving a socket nobody can name.
            if let Ok(meta) = std::fs::symlink_metadata(&path) {
                if meta.file_type().is_socket() {
                    let _ = std::fs::remove_file(&path);
                }
            }
            let listener = UnixListener::bind(&path).expect("bind fixture socket");
            // Non-blocking accept, waiting on the stop channel in between. A
            // blocking `accept` has no timeout to interrupt it, so the only way
            // to stop a worker nobody connects to would be to connect to it —
            // which stops working the moment anything unlinks the socket.
            listener
                .set_nonblocking(true)
                .expect("make the fixture listener cancellable");
            let (stop, handle) = Self::work(listener, budget, reply);
            instance.stop = Some(stop);
            instance.worker = Some(handle);
        }

        if published != Published::SocketOnly {
            maverick_sys::identity::write_meta(&InstanceInfo {
                name: sid.to_string(),
                session_id: sid.to_string(),
                pid: std::process::id(),
                display: String::new(),
                tty_nr: 0,
                x_server_identity: String::new(),
                start_time: 0,
                exe: String::new(),
                started_at: 0,
                alive: true,
            })
            .expect("write fixture ficha");
        }

        instance
    }

    fn work(
        listener: UnixListener,
        budget: Option<usize>,
        reply: impl Fn(&str) -> Option<String> + Send + 'static,
    ) -> (Sender<()>, JoinHandle<()>) {
        let (stop, stopped) = mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            let mut served = 0usize;
            while budget.is_none_or(|cap| served < cap) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        served += 1;
                        let mut reader = BufReader::new(&stream);
                        let mut request = String::new();
                        if reader.read_line(&mut request).is_err() {
                            continue;
                        }
                        if let Some(reply) = reply(request.trim_end()) {
                            let _ = stream.write_all(reply.as_bytes());
                            let _ = stream.flush();
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // The wait is what makes teardown possible; its length
                        // bounds how long a drop takes, not what a test
                        // observes.
                        if stopped
                            .recv_timeout(std::time::Duration::from_millis(1))
                            .is_ok()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        (stop, handle)
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // Joining before the unlink is what makes teardown deterministic rather
        // than a race with a worker on its way out.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        match self.published {
            // Unlinks the record and the socket, each only if it still is the
            // type it was published as, under this session's directory alone.
            Published::Instance | Published::RecordOnly => {
                maverick_sys::identity::cleanup_meta(&self.sid);
            }
            Published::SocketOnly => {
                let sock = maverick_sys::identity::sock_path(&self.sid);
                if let Ok(meta) = std::fs::symlink_metadata(&sock) {
                    if meta.file_type().is_socket() {
                        let _ = std::fs::remove_file(&sock);
                    }
                }
            }
        }
    }
}
