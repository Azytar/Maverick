//! Signal dispositions are process-wide, so every scenario gets its own process.
use std::os::fd::AsRawFd;
use std::time::Duration;

#[test]
fn signals_wake_the_wm_when_delivered_to_another_thread() {
    const CASE: &str = "MAVERICK_SIGNAL_WAKE_CASE";
    if let Ok(case) = std::env::var(CASE) {
        assert!(maverick_sys::Signal::new()
            .on_sigterm(libc::SIGUSR1)
            .on_sigcont(libc::SIGUSR2)
            .install()
            .is_empty());
        let idle = rustix::event::eventfd(0, rustix::event::EventfdFlags::CLOEXEC).unwrap();
        let signal = if case.starts_with("quit") {
            libc::SIGUSR1
        } else {
            libc::SIGUSR2
        };
        let during_wait = case.ends_with("during");
        let worker = std::thread::spawn(move || {
            if during_wait {
                std::thread::sleep(Duration::from_millis(50));
            }
            // SAFETY: raise targets this worker, with the handler installed
            // above. No default termination disposition is involved.
            assert_eq!(unsafe { libc::raise(signal) }, 0);
        });
        if !during_wait {
            worker.join().unwrap();
            assert!(
                !maverick_sys::wait_readable_fds(&[idle.as_raw_fd()], Some(Duration::ZERO)),
                "ordinary waiters must not consume the signal notification"
            );
            assert!(
                maverick_sys::wait_readable_fds_with_signals(
                    &[idle.as_raw_fd()],
                    Some(Duration::from_secs(2))
                ),
                "notification before the wait was lost"
            );
        } else {
            let woke = maverick_sys::wait_readable_fds_with_signals(
                &[idle.as_raw_fd()],
                Some(Duration::from_secs(2)),
            );
            worker.join().unwrap();
            assert!(woke, "notification on the worker did not wake the WM wait");
        }
        assert_eq!(maverick_sys::quit_requested(), signal == libc::SIGUSR1);
        assert_eq!(maverick_sys::need_regrab(), signal == libc::SIGUSR2);
        maverick_sys::clear_quit();
        maverick_sys::clear_regrab();
        assert!(
            !maverick_sys::wait_readable_fds_with_signals(
                &[idle.as_raw_fd()],
                Some(Duration::ZERO)
            ),
            "a consumed signal must not leave the event loop spinning"
        );
        return;
    }
    for case in [
        "quit-before",
        "quit-during",
        "regrab-before",
        "regrab-during",
    ] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "signals_wake_the_wm_when_delivered_to_another_thread",
                "--nocapture",
            ])
            .env(CASE, case)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{case}: {}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
