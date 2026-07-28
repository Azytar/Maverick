// maverickctl — command-line control for Maverick window-manager instances.
//
// This is the "everything the WM shouldn't do itself" tool: discover running
// instances, query their state, dispatch actions, stream events, and quit them
// (with confirmation) — all over the per-instance Unix control socket exposed
// by `maverick-sys`. The WM stays minimal; the policy lives here.
//
// Instance selection:
//   * `--name <id>` picks an instance explicitly.
//   * else `$MAVERICK_INSTANCE` (exported by the WM to its children).
//   * else, if exactly one instance is running, that one.
//   * else the tool lists candidates and refuses to guess.

use std::process::ExitCode;

use maverick_sys::control;
use maverick_sys::discover;
use maverick_sys::identity::InstanceInfo;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
        return ExitCode::FAILURE;
    }

    let cmd = args[0].as_str();
    let rest = &args[1..];

    match cmd {
        "-h" | "--help" | "help" => {
            usage();
            ExitCode::SUCCESS
        }
        "list" | "ls" => cmd_list(),
        "state" => cmd_state(rest),
        "msg" | "dispatch" => cmd_msg(rest),
        "subscribe" | "sub" => cmd_subscribe(rest),
        "quit" => cmd_quit(rest),
        "quit-all" => cmd_quit_all(rest),
        "restart" => cmd_simple(rest, "restart"),
        "reload" => cmd_simple(rest, "reload"),
        "prune" => cmd_prune(),
        other => {
            eprintln!("maverickctl: unknown command '{other}'\n");
            usage();
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    println!(
        "\
maverickctl — control Maverick window-manager instances

USAGE:
    maverickctl <command> [options]

COMMANDS:
    list                       List running/known instances
    state    [--name <id>]     Print the WM state snapshot (JSON)
    msg <action> [--name <id>] Dispatch an action (e.g. \"focus-left\", \"view 3\")
    subscribe   [--name <id>]  Stream WM events until interrupted
    quit     [--name <id>] [--confirm] [--yes]
                               Ask an instance to quit (confirmation optional)
    quit-all [--yes]           Quit every running instance
    restart  [--name <id>]     Restart an instance (re-exec)
    reload   [--name <id>]     Reload config (no-op for compiled config)
    prune                      Remove stale fichas whose socket is dead

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

fn parse_opts(args: &[String]) -> Opts {
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
            other => o.positional.push(other.to_string()),
        }
    }
    o
}

/// Resolve the target instance name given `--name`/env/singleton rules.
/// On ambiguity, prints the candidates and returns None.
fn resolve_target(explicit: &Option<String>) -> Option<String> {
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
            eprintln!("maverickctl: no running Maverick instance found");
            None
        }
        _ => {
            eprintln!("maverickctl: multiple instances running — pick one with --name:");
            for i in &live {
                eprintln!("  {}", i.label());
            }
            None
        }
    }
}

// ── commands ──────────────────────────────────────────────────────────────

fn cmd_list() -> ExitCode {
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

fn cmd_state(args: &[String]) -> ExitCode {
    let o = parse_opts(args);
    let name = match resolve_target(&o.name) {
        Some(n) => n,
        None => return ExitCode::FAILURE,
    };
    match control::state(&name) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("maverickctl: state failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_msg(args: &[String]) -> ExitCode {
    let o = parse_opts(args);
    if o.positional.is_empty() {
        eprintln!("maverickctl: msg requires an action, e.g. `maverickctl msg focus-left`");
        return ExitCode::FAILURE;
    }
    let action = o.positional.join(" ");
    let name = match resolve_target(&o.name) {
        Some(n) => n,
        None => return ExitCode::FAILURE,
    };
    match control::dispatch(&name, &action) {
        Ok(reply) => {
            if reply.starts_with("error") {
                eprintln!("maverickctl: {reply}");
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("maverickctl: dispatch failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_subscribe(args: &[String]) -> ExitCode {
    let o = parse_opts(args);
    let name = match resolve_target(&o.name) {
        Some(n) => n,
        None => return ExitCode::FAILURE,
    };
    let r = control::subscribe_stream(&name, |line| {
        println!("{line}");
        // Keep streaming until the socket closes or the process is killed.
        true
    });
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("maverickctl: subscribe failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_quit(args: &[String]) -> ExitCode {
    let o = parse_opts(args);
    let name = match resolve_target(&o.name) {
        Some(n) => n,
        None => return ExitCode::FAILURE,
    };

    if o.confirm && !o.yes {
        let prompt = format!("Quit Maverick instance '{name}'?");
        if !confirm(&prompt) {
            eprintln!("maverickctl: quit cancelled");
            return ExitCode::FAILURE;
        }
    }

    match discover::quit_by_name(&name) {
        Ok(_) => {
            println!("maverickctl: '{name}' quit");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("maverickctl: quit failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_quit_all(args: &[String]) -> ExitCode {
    let o = parse_opts(args);
    if !o.yes && !confirm("Quit ALL Maverick instances?") {
        eprintln!("maverickctl: quit-all cancelled");
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

fn cmd_simple(args: &[String], verb: &str) -> ExitCode {
    let o = parse_opts(args);
    let name = match resolve_target(&o.name) {
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
            println!("maverickctl: '{name}' {verb}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("maverickctl: {verb} failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_prune() -> ExitCode {
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

// ── confirmation ────────────────────────────────────────────────────────────

/// Ask the user to confirm `prompt`. Tries, in order:
///   1. `maverick-dialog` (our own X11 dialog binary, if installed)
///   2. `zenity` / `kdialog` graphical prompts
///   3. an interactive TTY prompt
///
/// Returns true only on an explicit "yes".
fn confirm(prompt: &str) -> bool {
    // 1. Our dedicated X11 dialog (separate crate, optional).
    if which("maverick-dialog") {
        if let Some(ok) = run_confirm("maverick-dialog", &["--question", prompt]) {
            return ok;
        }
    }
    // 2. Common desktop dialog tools.
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
    // 3. Fall back to a TTY prompt.
    tty_confirm(prompt)
}

/// Run a dialog command that returns exit code 0 for "yes", non-zero for "no".
/// Returns None if the command couldn't be executed at all.
fn run_confirm(bin: &str, args: &[&str]) -> Option<bool> {
    std::process::Command::new(bin)
        .args(args)
        .status()
        .ok()
        .map(|s| s.success())
}

/// Interactive y/N prompt on the controlling terminal. Returns false if there
/// is no TTY (can't safely assume "yes").
fn tty_confirm(prompt: &str) -> bool {
    use std::io::{IsTerminal, Write};
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        eprintln!("maverickctl: no terminal for confirmation; re-run with --yes to force");
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

/// True if `bin` is found on `$PATH`.
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
