//! Properties of the `maverickctl` CLI entry point.
//!
//! Everything the tools do beyond argument parsing needs a live instance — a
//! socket, a ficha, a WM thread — so this suite covers the one part that is
//! decided by argv alone: which exit code a word maps to, and whether the tool
//! ever guesses at an instance it was not pointed at.

mod common;

use maverick_sys::ctl::main_with_args;
use proptest::prelude::*;
use std::process::ExitCode;

/// Admin words that would reach for an instance, a socket or the filesystem.
/// Excluded so the property can never touch a running window manager.
const INSTANCE_WORDING: [&str; 14] = [
    "list",
    "ls",
    "state",
    "query",
    "q",
    "msg",
    "dispatch",
    "command",
    "subscribe",
    "sub",
    "quit",
    "quit-all",
    "restart",
    "reload",
];

/// A word the admin tool can be given that decides its exit code without
/// leaving the process: the help forms, and anything it does not know.
fn local_word() -> impl Strategy<Value = String> {
    prop_oneof![
        2 => prop::sample::select(vec!["-h", "--help", "help", "h"]).prop_map(String::from),
        3 => common::text(),
    ]
}

// The entry point is total and stateless: any word is either a documented help
// form or an unknown command that fails loudly, and the same argv always
// decides the same way. An unknown word must never fall through to the
// verbatim-forwarding path, which would act on whatever instance the
// context happens to resolve to.
proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]
    #[test]
    fn the_admin_tool_answers_a_documented_exit_code(word in local_word()) {
        prop_assume!(
            !INSTANCE_WORDING.contains(&word.as_str()),
            "word {:?} would reach for an instance",
            word
        );
        let first = main_with_args("maverickctl", vec![word.clone()]);
        let second = main_with_args("maverickctl", vec![word.clone()]);
        prop_assert_eq!(first, second, "the same argv must decide identically");
        let wants_help = matches!(word.as_str(), "-h" | "--help" | "help" | "h");
        let want = if wants_help { ExitCode::SUCCESS } else { ExitCode::FAILURE };
        prop_assert_eq!(first, want, "word {:?} mapped to the wrong exit code", word);

        // No command at all is the documented failure, not a silent success.
        prop_assert_eq!(main_with_args("maverickctl", vec![]), ExitCode::FAILURE);
    }
}
