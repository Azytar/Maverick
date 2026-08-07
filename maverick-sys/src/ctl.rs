// CLI control tools shared by the `maverickctl` and `maverick-msg` binaries.
//
// The two binaries are the "everything the WM shouldn't do itself" tools:
// discover running instances, query their state, run structured queries, send
// actions, stream events, and quit them — all over the per-instance Unix
// control socket exposed by `maverick-sys`. The WM stays minimal; the policy
// lives here.
//
// `maverickctl` is the general-purpose admin tool; `maverick-msg` is the
// dwm-style variant that takes *any* line (action, `query <topic>`, or raw
// protocol word) and forwards it verbatim — same engine underneath.
//
// Instance selection:
//   * `--name <id>` picks an instance explicitly.
//   * else `$MAVERICK_INSTANCE` (exported by the WM to its children).
//   * else, if exactly one instance is running, that one.
//   * else the tool lists candidates and refuses to guess.

use std::process::ExitCode;

use crate::{control, discover};
use crate::identity::InstanceInfo;

/// Entry point shared by both control binaries.
pub fn main_with_args(tool: &str, args: Vec<String>) -> ExitCode {
    if args.is_empty() {
        usage(tool);
        return ExitCode::FAILURE;
    }

    let cmd = args[0].as_str();
    let rest = &args[1..];

    match cmd {
        "-h" | "--help" | "help" => {
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
    state    [--name <id>]     Print the WM state snapshot (JSON)
    query <topic> [--name <id>]
                               Structured query: state, workspaces, tree, focused
                               (topic may also be a bare CLI action like
                               \"focus-left\" / \"view 3\", forwarded verbatim)
    msg <action> [--name <id>] Dispatch an action (e.g. \"focus-left\", \"view 3\")
    command <action>           Alias for msg (dispatch)
    subscribe   [--name <id>]  Stream WM events until interrupted
    quit     [--name <id>] [--confirm] [--yes]
                               Ask an instance to quit (confirmation optional)
    quit-all [--yes]           Quit every running instance
    restart  [--name <id>]     Restart an instance (re-exec)
    reload   [--name <id>]     Reload config (no-op for compiled config)
    prune                      Remove stale file far whose socket is dead

INSTANCE SELECTION:
    --name <id>   explicit; else $MAVERICK_INSTANCE; else the sole instance."
    );
}

// ── option parsing ────────────────────────────────────────────────────────

struct Opts {
    name: Option<String>,
    confirm: bool,
    yes: bool,
    positional: Vec<String>,
}

fn parse_opts(args: &[String], keep_flags: &[&str]) -> Opts {
    let mut o = Opts {
        name: None,
        confirm: false,
        yes: false,
        positional: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--name" | "-n" => o.name = it.next().cloned(),
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

/// Resolve the target instance name given `--name`/env/singleton rules.
/// On ambiguity, prints the candidates and returns None.
fn resolve_target(tool: &str, explicit: &Option<String>) -> Option<String> {
    if let Some(n) = explicit {
        return Some(n.clone());
    }
    if let Ok(env) = std::env::var("MAVERICK_INSTANCE") {
        if !env.is_empty() {
            return Some(env);
        }
    }
    let live: Vec<InstanceInfo> = discover::list_instances()
        .into_iter()
        .filter(|i| i.alive)
        .collect();
    match live.len() {
        1 => Some(live[0].name.clone()),
        0 => {
            eprintln!("{tool}: no running Maverick instance found");
            None
        }
        _ => {
            eprintln!("{tool}: multiple instances running — pick one with --name:");
            for i in &live {
                eprintln!("  {}", i.label());
            }
            None
        }
    }
}

// ── commands ──────────────────────────────────────────────────────────────

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
            "  {:<12} pid={:<7} display={:<6} tty={:#x} {}",
            i.name, i.pid, disp, i.tty_nr, status
        );
    }
    ExitCode::SUCCESS
}

fn print_json<E: std::error::Error + 'static>(
    tool: &str,
    res: Result<String, E>,
) -> ExitCode {
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
    let name = match resolve_target(tool, &o.name) {
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

fn cmd_msg(tool: &str, args: &[String]) -> ExitCode {
    let o = parse_opts_default(args);
    if o.positional.is_empty() {
        eprintln!("{tool}: msg requires an action, e.g. `{tool} msg focus-left`");
        return ExitCode::FAILURE;
    }
    let action = o.positional.join(" ");
    let name = match resolve_target(tool, &o.name) {
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

fn cmd_subscribe(tool: &str, args: &[String]) -> ExitCode {
    let o = parse_opts_default(args);
    let name = match resolve_target(tool, &o.name) {
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

fn cmd_quit(tool: &str, args: &[String]) -> ExitCode {
    let o = parse_opts_default(args);
    let name = match resolve_target(tool, &o.name) {
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

fn cmd_simple(tool: &str, args: &[String], verb: &str) -> ExitCode {
    let o = parse_opts_default(args);
    let name = match resolve_target(tool, &o.name) {
        Some(n) => n,
        None => return ExitCode::FAILURE,
    };
    let res = match verb {
        "restart" => control::restart(&name),
        "reload" => control::reload(&name),
        _ => unreachable!(),
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
    let name = match resolve_target(tool, &None) {
        Some(n) => n,
        None => return ExitCode::FAILURE,
    };
    use crate::identity::{DISPATCH_CMD, IDENTIFY_CMD, PING_CMD, QUERY_CMD};
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
        l if l.starts_with(QUERY_CMD) => control::query(
            &name,
            l.strip_prefix(QUERY_CMD).map(str::trim).unwrap_or(""),
        ),
        l if l.starts_with(DISPATCH_CMD) => {
            control::dispatch(&name, l.strip_prefix(DISPATCH_CMD).map(str::trim).unwrap_or(""))
        }
        l => control::dispatch(&name, l),
    };
    print_json(tool, res)
}

// ── confirmation ────────────────────────────────────────────────────────────

/// Ask the user to confirm `prompt`. Tries, in order:
///   1. `maverick-dialog` (our own X11 dialog binary, if installed)
///   2. `zenity` / `kdialog` graphical prompts
///   3. an interactive TTY prompt
fn confirm(tool: &str, prompt: &str) -> bool {
    if which("maverick-dialog") {
        if let Some(ok) = run_confirm("maverick-dialog", &["--question", prompt]) {
            return ok;
        }
    }
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

fn run_confirm(bin: &str, args: &[&str]) -> Option<bool> {
    std::process::Command::new(bin)
        .args(args)
        .status()
        .ok()
        .map(|s| s.success())
}

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

fn which(bin: &str) -> bool {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            let p = std::path::Path::new(dir).join(bin);
            if p.is_file() {
                return true;
            }
        }
    }
    false
}