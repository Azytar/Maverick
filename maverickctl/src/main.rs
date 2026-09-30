// maverickctl — command-line control for Maverick window-manager instances.
// All behavior lives in this crate's `ctl` engine; `maverick-sys` only
// provides the shared control-protocol surface underneath it.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    maverickctl::ctl::main_with_args("maverickctl", args)
}
