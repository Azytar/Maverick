// maverickctl — command-line control for Maverick window-manager instances.
// All behavior lives in the shared `maverick-sys::ctl` engine.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    maverick_sys::ctl::main_with_args("maverickctl", args)
}
