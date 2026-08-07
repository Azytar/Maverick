// maverick-msg — send a message to a Maverick window-manager instance, dwm
// style: any unrecognized argument line is forwarded verbatim over the
// control socket (actions like "focus-right", structured queries like "query
// tree", or raw protocol words like "state"). Known admin subcommands
// (list/state/quit/…) behave like `maverickctl`.
//
// Thin wrapper over the shared CLI engine in `maverick-sys::ctl`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    maverick_sys::ctl::main_with_args("maverick-msg", args)
}
