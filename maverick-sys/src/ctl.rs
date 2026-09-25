//! CLI control tools shared by the `maverickctl` and `maverick-msg` binaries.
//!
//! The two binaries are the "everything the WM shouldn't do itself" tools:
//! discover running instances, query their state, run structured queries, send
//! actions, stream events, and quit them — all over the per-instance Unix
//! control socket exposed by `maverick-sys`. The WM stays minimal; the policy
//! lives here.
//!
//! `maverickctl` is the general-purpose admin tool; `maverick-msg` is the
//! dwm-style variant that takes *any* line (action, `query <topic>`, or raw
//! protocol word) and forwards it verbatim — same engine underneath.
//!
//! # Instance-selection precedence
//!
//! [`resolve_target`] resolves the target `session_id` in this order (first
//! match wins):
//!
//! 1. `--session <sid>` — explicit session id, validated via [`crate::identity::read_meta`].
//! 2. `--name <label>` — human label or sid, via [`crate::discover::find_by_name`].
//! 3. `$MAVERICK_INSTANCE` — session id the WM exported to its children.
//! 4. Caller context — `DISPLAY` + controlling TTY (`/proc/self/stat` field 7);
//!    if that yields a single live candidate, it is chosen.
//! 5. Singleton — if exactly one live instance exists globally, that one.
//! 6. Otherwise the tool lists candidates and returns `None` (refuses to guess).
//!
//! # Ownership
//!
//! Stateless CLI dispatch; no handles are retained across invocations. Confirmation
//! prompts try `zenity`/`kdialog` → TTY fallback.

use std::process::ExitCode;

use crate::identity::{current_display, current_tty_nr, InstanceInfo};
use crate::{control, discover};

/// Entry point shared by both control binaries.
pub fn main_with_args(tool: &str, args: Vec<String>) -> ExitCode {
    if args.is_empty() {
        usage(tool);
        return ExitCode::FAILURE;
    }

    let cmd = args[0].as_str();
    let rest = &args[1..];

    match cmd {
        "-h" | "--help" | "help" | "h" => {
            usage(tool);
            ExitCode::SUCCESS
        }
        "list" | "ls" => cmd_list(tool),
        "state" => cmd_state(tool, rest, true),
        "query" | "q" => cmd_state(tool, rest, false),
        "msg" | "dispatch" | "command" => cmd_msg(tool, rest),
        "subscribe" | "sub" => cmd_subscribe(tool, rest),
        "quit" => cmd_quit(tool, rest),
        "quit-all" => cmd_quit_all(tool, rest),
        "restart" => cmd_simple(tool, rest, "restart"),
        "reload" => cmd_simple(tool, rest, "reload"),
        "prune" => cmd_prune(tool),
        other => {
            if !tool.ends_with("msg") {
                eprintln!("{tool}: unknown command '{other}'\n");
                usage(tool);
                return ExitCode::FAILURE;
            }
            // `maverick-msg` with a non-admin word: forward the whole line
            // verbatim (dwm style). Structured queries become `query <topic>`,
            // everything else is dispatched as an action.
            let line = args.join(" ");
            cmd_forward(tool, &line)
        }
    }
}

fn usage(tool: &str) {
    println!(
        "\
{tool} — control Maverick window-manager instances

USAGE:
    {tool} <command> [options]

COMMANDS:
    list                       List running/known instances
    state    [--name <id>] [--session <sid>]   Print the WM state snapshot (JSON)
    query <topic> [--name <id>] [--session <sid>]
                                Structured query: state, workspaces, tree, focused
                                (topic may also be a bare CLI action like
                                \"focus-left\" / \"view 3\", forwarded verbatim)
    msg <action> [--name <id>] [--session <sid>] Dispatch an action; e.g.
                                \"focus-left\", \"view 3\", or wallpaper verbs:
                                \"wallpaper set /ruta\", \"wallpaper clear\",
                                \"wallpaper mode fill\"
    command <action>           Alias for msg (dispatch)
    subscribe   [--name <id>] [--session <sid>]  Stream WM events until interrupted
    quit     [--name <id>] [--session <sid>] [--confirm] [--yes]
                                Ask an instance to quit (confirmation optional)
    quit-all [--yes]           Quit every running instance
    restart  [--name <id>] [--session <sid>]     Restart an instance (re-exec)
    reload   [--name <id>] [--session <sid>]     Reload config (no-op for compiled config)
    prune                      Remove stale file far whose socket is dead

INSTANCE SELECTION:
    --session <sid>  explicit session id (from `list`)
    --name <id>      human label (or session id); else $MAVERICK_INSTANCE;
                     else the sole instance on this DISPLAY/TTY."
    );
}

/// Parsed CLI options shared by all `ctl` commands.
///
/// `name`/`session` feed [`resolve_target`]; `confirm`/`yes` gate
/// [`cmd_quit`]/[`cmd_quit_all`]; `positional` carries the remaining
/// action/topic words.
struct Opts {
    /// `--name` / `-n` human label or session id.
    name: Option<String>,
    /// `--session` / `-s` explicit session id.
    session: Option<String>,
    /// `--confirm` — require interactive confirmation.
    confirm: bool,
    /// `--yes` / `-y` — bypass confirmation.
    yes: bool,
    /// Non-flag positional arguments (action names, topics).
    positional: Vec<String>,
}

/// Parse `args` into [`Opts`]. Flags listed in `keep_flags` are consumed and
/// discarded (accepted for CLI compatibility) instead of becoming positional;
/// `--name`/`--session` take the next token as their value, while
/// `--confirm`/`--yes` are booleans.
fn parse_opts(args: &[String], keep_flags: &[&str]) -> Opts {
    let mut o = Opts {
        name: None,
        session: None,
        confirm: false,
        yes: false,
        positional: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--name" | "-n" => o.name = it.next().cloned(),
            "--session" | "-s" => o.session = it.next().cloned(),
            "--confirm" => o.confirm = true,
            "--yes" | "-y" => o.yes = true,
            other => {
                if !keep_flags.contains(&other) {
                    o.positional.push(other.to_string());
                }
            }
        }
    }
    o
}

fn parse_opts_default(args: &[String]) -> Opts {
    parse_opts(args, &[])
}

/// Resolve the target instance `session_id` using the module's documented
/// precedence. On no match or ambiguity prints the candidates to `stderr` and
/// returns `None` rather than guessing. The returned string is the filesystem
/// key (`session_id`), not the human label.
fn resolve_target(tool: &str, name: &Option<String>, session: &Option<String>) -> Option<String> {
    if let Some(s) = session {
        return if crate::identity::read_meta(s).is_some() {
            Some(s.clone())
        } else {
            eprintln!("{tool}: no instance with session id '{s}'");
            None
        };
    }
    if let Some(n) = name {
        return discover::find_by_name(n).map(|i| i.session_id);
    }
    // `$MAVERICK_INSTANCE` holds the session id the WM exported to its children
    // (the common case when a tool is launched from a Maverick keybind).
    if let Ok(env) = std::env::var("MAVERICK_INSTANCE") {
        if !env.is_empty() && crate::identity::read_meta(&env).is_some() {
            return Some(env);
        }
    }
    // Fall back to the caller's own context (DISPLAY + controlling tty), which
    // is what lets `maverickctl` launched from a bare TTY pick "the session on
    // my DISPLAY/TTY" rather than a globally ambiguous "default".
    let ctx_display = current_display();
    let ctx_tty = current_tty_nr();
    let live: Vec<InstanceInfo> = discover::list_instances()
        .into_iter()
        .filter(|i| i.alive)
        .collect();
    // An empty DISPLAY or a 0 tty means "unknown", not "match nothing": each
    // degrades to a wildcard so a tool launched without a terminal still
    // resolves.
    let by_context: Vec<InstanceInfo> = live
        .iter()
        .filter(|i| {
            (ctx_display.is_empty() || i.display == ctx_display)
                && (ctx_tty == 0 || i.tty_nr == ctx_tty)
        })
        .cloned()
        .collect();
    let candidates: &[InstanceInfo] = if ctx_display.is_empty() && ctx_tty == 0 {
        &live
    } else {
        &by_context
    };
    match candidates.len() {
        1 => Some(candidates[0].session_id.clone()),
        0 => {
            eprintln!("{tool}: no running Maverick instance found for this context");
            None
        }
        _ => {
            eprintln!("{tool}: multiple instances match — pick one with --session/--name:");
            for i in candidates {
                eprintln!("  {}", i.label());
            }
            None
        }
    }
}

/// List all known instances with `alive`/`STALE` status (`list`/`ls`).
fn cmd_list(_tool: &str) -> ExitCode {
    let instances = discover::list_instances();
    if instances.is_empty() {
        println!("no maverick instances found");
        return ExitCode::SUCCESS;
    }
    println!("maverick instances:");
    for i in &instances {
        let disp = if i.display.is_empty() {
            "?"
        } else {
            &i.display
        };
        let status = if i.alive { "alive" } else { "STALE" };
        println!(
            "  {:<22} {:<12} pid={:<7} display={:<6} tty={:#x} xserver={:<8} {}",
            i.session_id, i.name, i.pid, disp, i.tty_nr, i.x_server_identity, status
        );
    }
    ExitCode::SUCCESS
}

/// Print `res` as JSON on success or an error line on failure, returning the
/// appropriate [`ExitCode`]. Used by `state`/`query`/`msg` passthrough.
fn print_json<E: std::error::Error + 'static>(tool: &str, res: Result<String, E>) -> ExitCode {
    match res {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{tool}: query failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `state` → full snapshot; `query <topic>` → a single structured query (or a
/// bare action line passed through as a dispatcher).
fn cmd_state(tool: &str, args: &[String], full_snapshot: bool) -> ExitCode {
    let o = parse_opts(args, &["-j", "--json", "-b", "--bare"]);
    let name = match resolve_target(tool, &o.name, &o.session) {
        Some(n) => n,
        None => return ExitCode::FAILURE,
    };
    if full_snapshot {
        return print_json(tool, control::state(&name));
    }
    let line = o.positional.join(" ");
    if line.is_empty() {
        eprintln!("{tool}: query requires a topic (state|workspaces|tree|focused) or action");
        return ExitCode::FAILURE;
    }
    print_json(tool, control::query(&name, &line))
}

/// Dispatch an action string (`msg`/`dispatch`/`command`) to the resolved instance.
fn cmd_msg(tool: &str, args: &[String]) -> ExitCode {
    let o = parse_opts_default(args);
    if o.positional.is_empty() {
        eprintln!("{tool}: msg requires an action, e.g. `{tool} msg focus-left`");
        return ExitCode::FAILURE;
    }
    let action = o.positional.join(" ");
    let name = match resolve_target(tool, &o.name, &o.session) {
        Some(n) => n,
        None => return ExitCode::FAILURE,
    };
    match control::dispatch(&name, &action) {
        Ok(reply) => {
            if reply.starts_with("error") {
                eprintln!("{tool}: {reply}");
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("{tool}: dispatch failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Stream events from the resolved instance until the socket closes (`subscribe`/`sub`).
fn cmd_subscribe(tool: &str, args: &[String]) -> ExitCode {
    let o = parse_opts_default(args);
    let name = match resolve_target(tool, &o.name, &o.session) {
        Some(n) => n,
        None => return ExitCode::FAILURE,
    };
    let r = control::subscribe_stream(&name, |line| {
        println!("{line}");
        true
    });
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{tool}: subscribe failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Ask a single instance to quit, honouring `--confirm`/`--yes` (`quit`).
fn cmd_quit(tool: &str, args: &[String]) -> ExitCode {
    let o = parse_opts_default(args);
    let name = match resolve_target(tool, &o.name, &o.session) {
        Some(n) => n,
        None => return ExitCode::FAILURE,
    };

    if o.confirm && !o.yes {
        let prompt = format!("Quit Maverick instance '{name}'?");
        if !confirm(tool, &prompt) {
            eprintln!("{tool}: quit cancelled");
            return ExitCode::FAILURE;
        }
    }

    match discover::quit_by_name(&name) {
        Ok(_) => {
            println!("{tool}: '{name}' quit");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{tool}: quit failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Quit every live instance (`quit-all`), requiring `--yes` or confirmation.
fn cmd_quit_all(tool: &str, args: &[String]) -> ExitCode {
    let o = parse_opts_default(args);
    if !o.yes && !confirm(tool, "Quit ALL Maverick instances?") {
        eprintln!("{tool}: quit-all cancelled");
        return ExitCode::FAILURE;
    }
    let results = discover::quit_all();
    if results.is_empty() {
        println!("no running instances to quit");
        return ExitCode::SUCCESS;
    }
    let mut ok = true;
    for (name, res) in results {
        match res {
            Ok(_) => println!("  {name}: quit"),
            Err(e) => {
                eprintln!("  {name}: FAILED ({e})");
                ok = false;
            }
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Handle `restart`/`reload` — single verb dispatched via [`control::restart`]/[`control::reload`].
fn cmd_simple(tool: &str, args: &[String], verb: &str) -> ExitCode {
    let o = parse_opts_default(args);
    let name = match resolve_target(tool, &o.name, &o.session) {
        Some(n) => n,
        None => return ExitCode::FAILURE,
    };
    let res = match verb {
        "restart" => control::restart(&name),
        "reload" => control::reload(&name),
        other => {
            eprintln!("{tool}: unknown verb '{other}'");
            return ExitCode::FAILURE;
        }
    };
    match res {
        Ok(_) => {
            println!("{tool}: '{name}' {verb}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{tool}: {verb} failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Remove stale fichas whose socket no longer answers (`prune`).
fn cmd_prune(_tool: &str) -> ExitCode {
    let removed = discover::prune_stale();
    if removed.is_empty() {
        println!("no stale instances");
    } else {
        for name in removed {
            println!("pruned stale: {name}");
        }
    }
    ExitCode::SUCCESS
}

/// `maverick-msg <any line>` passthrough: a single line may be a structured
/// query ("query tree"), a raw protocol word ("state", "quit"), or an action
/// ("focus-right", "view 3"). Resolution order: raw command words first, then
/// `query <topic>` (structured), then fall back to dispatching.
fn cmd_forward(tool: &str, line: &str) -> ExitCode {
    let name = match resolve_target(tool, &None, &None) {
        Some(n) => n,
        None => return ExitCode::FAILURE,
    };
    use crate::identity::{DISPATCH_CMD, IDENTIFY_CMD, PING_CMD, QUERY_CMD};
    // Require a whitespace delimiter after `query`/`dispatch`, mirroring the
    // server (`control::dispatch_line`): `queryfoo` must fall through to
    // dispatch, not be parsed as topic `foo`.
    fn strip_cmd<'l>(line: &'l str, cmd: &str) -> Option<&'l str> {
        let rest = line.strip_prefix(cmd)?;
        if rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace()) {
            Some(rest.trim())
        } else {
            None
        }
    }
    let res: std::io::Result<String> = match line.trim() {
        "ping" => control::send_command(&name, PING_CMD),
        "identify" => control::send_command(&name, IDENTIFY_CMD),
        "state" => control::state(&name),
        "quit" => control::quit(&name),
        "restart" => control::restart(&name),
        "reload" => control::reload(&name),
        "subscribe" => control::subscribe_stream(&name, |l| {
            println!("{l}");
            true
        })
        .map(|_| "ok".to_string()),
        l => {
            if let Some(topic) = strip_cmd(l, QUERY_CMD) {
                control::query(&name, topic)
            } else if let Some(action) = strip_cmd(l, DISPATCH_CMD) {
                control::dispatch(&name, action)
            } else {
                control::dispatch(&name, l)
            }
        }
    };
    print_json(tool, res)
}

/// Ask the user to confirm `prompt`. Tries `zenity` then `kdialog` for a
/// graphical prompt and only then falls back to an interactive TTY.
fn confirm(tool: &str, prompt: &str) -> bool {
    if which("zenity") {
        if let Some(ok) = run_confirm("zenity", &["--question", "--text", prompt]) {
            return ok;
        }
    }
    if which("kdialog") {
        if let Some(ok) = run_confirm("kdialog", &["--yesno", prompt]) {
            return ok;
        }
    }
    tty_confirm(tool, prompt)
}

/// Run `bin` with `args` and map exit status to confirmation. `None` on spawn failure.
fn run_confirm(bin: &str, args: &[&str]) -> Option<bool> {
    std::process::Command::new(bin)
        .args(args)
        .status()
        .ok()
        .map(|s| s.success())
}

/// TTY fallback for [`confirm`] — reads `y/N` from stdin, returns `false` if not a terminal.
fn tty_confirm(tool: &str, prompt: &str) -> bool {
    use std::io::{IsTerminal, Write};
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        eprintln!("{tool}: no terminal for confirmation; re-run with --yes to force");
        return false;
    }
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if stdin.read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y" | "yes" | "YES")
}

/// Return `true` if `bin` exists in `$PATH` and is executable.
fn which(bin: &str) -> bool {
    // Reject path separators: only bare binary names are looked up in PATH.
    if bin.is_empty() || bin.contains('/') {
        return false;
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            if dir.is_empty() {
                continue;
            }
            let p = std::path::Path::new(dir).join(bin);
            // `is_file` alone accepts non-executable files.
            if !p.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(m) = std::fs::metadata(&p) {
                    if m.permissions().mode() & 0o111 != 0 {
                        return true;
                    }
                    continue;
                }
                continue;
            }
            #[cfg(not(unix))]
            {
                return true;
            }
        }
    }
    false
}

/// Properties of the shared argument parser.
///
/// The parsed `positional` words are what a tool forwards to the WM verbatim
/// and what `--name`/`--session` select an instance with, so the parser is the
/// boundary where a typo, a shell surprise or a hostile argument stops being
/// the caller's problem and starts being the WM's. It has to be total, it has
/// to be a pure function of argv, and it must not invent a token the caller
/// never typed.
#[cfg(test)]
mod opts_props {
    use super::*;
    use crate::prop_support::{config, text};
    use proptest::prelude::*;

    /// The compatibility flags `cmd_state` hands to [`parse_opts`].
    const KEEP: &[&str] = &["-j", "--json", "-b", "--bare"];

    /// A word that is never one of the recognised flags, so a parser that
    /// treated it as one would be caught. Near misses such as `--json` and a
    /// bare `-` are included on purpose: they are not flags, so they have to
    /// survive as positionals.
    fn loose_word() -> impl Strategy<Value = String> {
        prop_oneof![
            3 => "[A-Za-z0-9_.:=]{0,12}",
            1 => prop::sample::select(vec![
                "", "-", "--", "-x", "-j", "--json", "-b", "--bare", "focus-left", "view 3",
                "query state", "focus-left --session",
            ])
            .prop_map(String::from),
        ]
    }

    /// A token that follows a flag. A flag value is taken verbatim, so it may
    /// itself look exactly like another flag.
    fn flag_value() -> impl Strategy<Value = String> {
        prop_oneof![
            4 => "[A-Za-z0-9_.:-]{0,12}",
            1 => prop::sample::select(vec!["-y", "-n", "--name", "--yes", "--session", ""])
                .prop_map(String::from),
        ]
    }

    /// One argv entry, described together with what the parser owes for it.
    #[derive(Debug, Clone)]
    enum Item {
        Positional(String),
        /// `--name`/`-n` with the token that follows it, if any.
        Name(Option<String>),
        Session(Option<String>),
        Confirm,
        Yes,
        /// A compatibility flag: accepted, and not forwarded.
        Kept(String),
    }

    fn item() -> impl Strategy<Value = Item> {
        prop_oneof![
            4 => loose_word().prop_map(Item::Positional),
            2 => prop::option::of(flag_value()).prop_map(Item::Name),
            2 => prop::option::of(flag_value()).prop_map(Item::Session),
            1 => Just(Item::Confirm),
            1 => Just(Item::Yes),
            1 => loose_word().prop_map(Item::Kept),
        ]
    }

    // A well-formed invocation, described independently of the parser, so the
    // expectations below are an oracle rather than a restatement: every flag
    // is listed with the value it should take, and the last occurrence of a
    // value-taking flag is the one that counts.
    proptest! {
        #![proptest_config(config())]
        #[test]
        fn parse_opts_recovers_every_flag_and_positional(
            items in proptest::collection::vec(item(), 0..10),
        ) {
            let mut items = items;
            // A value-taking flag swallows the next token whatever it is, so a
            // flag with no value of its own is only possible at the end of argv.
            if let Some(cut) = items
                .iter()
                .position(|i| matches!(i, Item::Name(None) | Item::Session(None)))
            {
                items.truncate(cut + 1);
            }
            let mut args: Vec<String> = Vec::new();
            // Every word that is not a flag, in argv order; the `keep_flags`
            // parse is the same list with the compatibility flags taken out.
            let mut want_positional: Vec<String> = Vec::new();
            let mut want_name = None;
            let mut want_session = None;
            let mut want_confirm = false;
            let mut want_yes = false;
            for item in &items {
                match item {
                    Item::Positional(w) => {
                        args.push(w.clone());
                        want_positional.push(w.clone());
                    }
                    Item::Kept(w) => {
                        args.push(w.clone());
                        want_positional.push(w.clone());
                    }
                    Item::Name(v) => {
                        args.push("--name".to_string());
                        want_name = v.clone();
                        args.extend(v.clone());
                    }
                    Item::Session(v) => {
                        args.push("--session".to_string());
                        want_session = v.clone();
                        args.extend(v.clone());
                    }
                    Item::Confirm => {
                        args.push("--confirm".to_string());
                        want_confirm = true;
                    }
                    Item::Yes => {
                        args.push("--yes".to_string());
                        want_yes = true;
                    }
                }
            }

            let o = parse_opts(&args, &[]);
            prop_assert_eq!(&o.positional, &want_positional, "argv {:?}", args);
            prop_assert_eq!(&o.name, &want_name, "argv {:?}", args);
            prop_assert_eq!(&o.session, &want_session, "argv {:?}", args);
            prop_assert_eq!(o.confirm, want_confirm, "argv {:?}", args);
            prop_assert_eq!(o.yes, want_yes, "argv {:?}", args);

            // `keep_flags` decides what is dropped, and nothing else: the same
            // argv must yield the same instance selection either way.
            let filtered = parse_opts(&args, KEEP);
            let forwarded: Vec<String> = want_positional
                .iter()
                .filter(|w| !KEEP.contains(&w.as_str()))
                .cloned()
                .collect();
            prop_assert_eq!(&filtered.positional, &forwarded, "argv {:?}", args);
            prop_assert_eq!(&filtered.name, &o.name);
            prop_assert_eq!(&filtered.session, &o.session);
            prop_assert_eq!(filtered.confirm, o.confirm);
            prop_assert_eq!(filtered.yes, o.yes);
        }
    }

    // Whatever argv looks like — flagless, repeated, truncated, quoting a
    // control character, empty — nothing may come out of the parser that was
    // not in it, and the same argv must always parse the same way.
    proptest! {
        #![proptest_config(config())]
        #[test]
        fn parse_opts_invents_nothing_and_never_panics(
            args in proptest::collection::vec(text(), 0..12),
            keep in proptest::collection::vec(loose_word(), 0..3),
        ) {
            let keep: Vec<&str> = keep.iter().map(|s| s.as_str()).collect();
            let o = parse_opts(&args, &keep);
            let again = parse_opts(&args, &keep);
            prop_assert_eq!(&o.positional, &again.positional, "argv {:?}", args);
            prop_assert_eq!(&o.name, &again.name, "argv {:?}", args);
            prop_assert_eq!(&o.session, &again.session, "argv {:?}", args);
            prop_assert_eq!(o.confirm, again.confirm, "argv {:?}", args);
            prop_assert_eq!(o.yes, again.yes, "argv {:?}", args);

            for value in [&o.name, &o.session].into_iter().flatten() {
                prop_assert!(
                    args.contains(value),
                    "captured value {:?} is not a token of argv {:?}",
                    value,
                    args
                );
            }
            for word in &o.positional {
                prop_assert!(
                    args.contains(word),
                    "positional {:?} is not a token of argv {:?}",
                    word,
                    args
                );
                prop_assert!(
                    !keep.contains(&word.as_str()),
                    "dropped flag {:?} was forwarded to the WM: argv {:?}",
                    word,
                    args
                );
            }
        }
    }
}
