// maverickctl — command-line control for Maverick window-manager instances.
//
// Thin wrapper over the shared CLI engine in `maverick-sys::ctl` (the logic
// itself lives in the library so `maverick-msg` reuses it verbatim).

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    maverick_sys::ctl::main_with_args("maverickctl", args)
}
