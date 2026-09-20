use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TERMINALS: &[&str] = &[
    "alacritty",
    "kitty",
    "ghostty",
    "wezterm",
    "xterm",
    "urxvt",
    "st",
    "gnome-terminal",
    "konsole",
    "xfce4-terminal",
    "lxterminal",
];

const LAUNCHERS: &[Launcher] = &[
    Launcher {
        bin: "rofi",
        drun: &["rofi", "-show", "drun"],
        run: &["rofi", "-show", "run"],
    },
    Launcher {
        bin: "dmenu_run",
        drun: &["dmenu_run"],
        run: &["dmenu_run"],
    },
    Launcher {
        bin: "bemenu-run",
        drun: &["bemenu-run"],
        run: &["bemenu-run"],
    },
    Launcher {
        bin: "fuzzel",
        drun: &["fuzzel"],
        run: &["fuzzel"],
    },
    Launcher {
        bin: "wofi",
        drun: &["wofi", "--show", "drun"],
        run: &["wofi", "--show", "run"],
    },
];

const BARS: &[Bar] = &[
    Bar {
        bin: "waybar",
        command: &["waybar"],
    },
    Bar {
        bin: "polybar",
        command: &["polybar", "main"],
    },
    Bar {
        bin: "xfce4-panel",
        command: &["xfce4-panel"],
    },
    Bar {
        bin: "lxpanel",
        command: &["lxpanel"],
    },
];

#[derive(Clone, Copy)]
struct Launcher {
    bin: &'static str,
    drun: &'static [&'static str],
    run: &'static [&'static str],
}

#[derive(Clone, Copy)]
struct Bar {
    bin: &'static str,
    command: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Profile {
    Minimal,
    Daily,
    Laptop,
    Gaming,
    Custom,
}

impl Profile {
    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "minimal" => Some(Self::Minimal),
            "daily" => Some(Self::Daily),
            "laptop" => Some(Self::Laptop),
            "gaming" => Some(Self::Gaming),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Daily => "daily",
            Self::Laptop => "laptop",
            Self::Gaming => "gaming",
            Self::Custom => "custom",
        }
    }
}

#[derive(Default)]
struct Args {
    detect: bool,
    dry_run: bool,
    write: bool,
    interactive: bool,
    force: bool,
    profile: Option<Profile>,
    output: Option<PathBuf>,
}

struct Detection {
    terminals: Vec<String>,
    launchers: Vec<Launcher>,
    bars: Vec<Bar>,
    monitors: Vec<String>,
    glxinfo: bool,
    x_session: bool,
}

struct Plan {
    profile: Profile,
    terminal: Option<String>,
    launcher: Option<Launcher>,
    bar: Option<Bar>,
    compositor_enabled: bool,
    gaps_inner: u32,
    gaps_outer: u32,
    border_width: u32,
    theme: &'static str,
    focus_mouse: bool,
    warp_cursor: bool,
    autostart_portals: bool,
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(msg) => {
            eprintln!("maverick-setup: {msg}");
            usage();
            std::process::exit(2);
        }
    };

    if args.detect {
        print_detection(&detect());
        return;
    }

    let detection = detect();
    let profile = args.profile.unwrap_or(Profile::Daily);
    let plan = if args.interactive {
        interactive_plan(profile, &detection)
    } else {
        auto_plan(profile, &detection)
    };
    let config = render_config(&plan);

    if args.dry_run || !args.write {
        print_summary(&plan, &detection);
        println!();
        print!("{config}");
        return;
    }

    let path = args.output.unwrap_or_else(default_config_path);
    if path.exists() && !args.force {
        eprintln!(
            "maverick-setup: {} already exists; use --force to replace it",
            path.display()
        );
        std::process::exit(1);
    }

    if let Err(e) = write_config(&path, &config) {
        eprintln!("maverick-setup: failed to write {}: {e}", path.display());
        std::process::exit(1);
    }

    match validate_config(&path) {
        Ok(()) => {
            print_summary(&plan, &detection);
            println!("config: {}", path.display());
            println!("validation: OK");
        }
        Err(e) => {
            eprintln!("maverick-setup: config was written but validation failed:\n{e}");
            std::process::exit(1);
        }
    }
}

fn parse_args() -> Result<Args, String> {
    let mut out = Args::default();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                usage();
                std::process::exit(0);
            }
            "--detect" => out.detect = true,
            "--dry-run" => out.dry_run = true,
            "--write" => out.write = true,
            "--interactive" => out.interactive = true,
            "--force" => out.force = true,
            "--profile" => {
                let Some(value) = args.next() else {
                    return Err("--profile requires a value".into());
                };
                out.profile = Profile::parse(&value);
                if out.profile.is_none() {
                    return Err(format!("unknown profile '{value}'"));
                }
            }
            "--output" => {
                let Some(value) = args.next() else {
                    return Err("--output requires a path".into());
                };
                out.output = Some(PathBuf::from(value));
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(out)
}

fn usage() {
    println!("Usage: maverick-setup [--detect] [--dry-run] [--write] [--force]");
    println!(
        "                      [--interactive] [--profile minimal|daily|laptop|gaming|custom]"
    );
    println!("                      [--output <path>]");
}

fn detect() -> Detection {
    let path = current_path();
    Detection {
        terminals: TERMINALS
            .iter()
            .filter(|bin| find_in_path(bin, &path).is_some())
            .map(|s| (*s).to_string())
            .collect(),
        launchers: LAUNCHERS
            .iter()
            .copied()
            .filter(|launcher| find_in_path(launcher.bin, &path).is_some())
            .collect(),
        bars: BARS
            .iter()
            .copied()
            .filter(|bar| find_in_path(bar.bin, &path).is_some())
            .collect(),
        monitors: detect_monitors(),
        glxinfo: find_in_path("glxinfo", &path).is_some(),
        x_session: env::var_os("DISPLAY").is_some(),
    }
}

fn print_detection(d: &Detection) {
    println!("Maverick First Flight detection");
    println!("x11 display: {}", yes_no(d.x_session));
    println!("glxinfo: {}", yes_no(d.glxinfo));
    println!("terminals: {}", list_or_none(&d.terminals));
    println!(
        "launchers: {}",
        list_or_none(
            &d.launchers
                .iter()
                .map(|l| l.bin.to_string())
                .collect::<Vec<_>>()
        )
    );
    println!(
        "bars: {}",
        list_or_none(&d.bars.iter().map(|b| b.bin.to_string()).collect::<Vec<_>>())
    );
    println!("monitors: {}", list_or_none(&d.monitors));
}

fn auto_plan(profile: Profile, d: &Detection) -> Plan {
    let terminal = d.terminals.first().cloned();
    let launcher = d.launchers.first().copied();
    let bar = match profile {
        Profile::Daily | Profile::Laptop | Profile::Custom => d.bars.first().copied(),
        Profile::Minimal | Profile::Gaming => None,
    };
    let compositor_enabled = !matches!(profile, Profile::Minimal);
    let (gaps_inner, gaps_outer, border_width, focus_mouse, warp_cursor, theme) = match profile {
        Profile::Minimal => (4, 4, 2, false, false, "nord"),
        Profile::Daily => (8, 12, 2, false, false, "catppuccin-mocha"),
        Profile::Laptop => (6, 8, 2, false, false, "everforest"),
        Profile::Gaming => (4, 4, 2, false, false, "gruvbox"),
        Profile::Custom => (8, 12, 2, false, false, "catppuccin-mocha"),
    };
    Plan {
        profile,
        terminal,
        launcher,
        bar,
        compositor_enabled,
        gaps_inner,
        gaps_outer,
        border_width,
        theme,
        focus_mouse,
        warp_cursor,
        autostart_portals: true,
    }
}

fn interactive_plan(default_profile: Profile, d: &Detection) -> Plan {
    let profile = ask_profile(default_profile);
    let mut plan = auto_plan(profile, d);
    if d.terminals.len() > 1 {
        plan.terminal = ask_choice("Select terminal", &d.terminals);
    }
    if d.launchers.len() > 1 {
        let labels: Vec<String> = d.launchers.iter().map(|l| l.bin.to_string()).collect();
        if let Some(selected) = ask_choice("Select launcher", &labels) {
            plan.launcher = d.launchers.iter().copied().find(|l| l.bin == selected);
        }
    }
    if matches!(profile, Profile::Daily | Profile::Laptop | Profile::Custom) && d.bars.len() > 1 {
        let labels: Vec<String> = d.bars.iter().map(|b| b.bin.to_string()).collect();
        if let Some(selected) = ask_choice("Autostart bar", &labels) {
            plan.bar = d.bars.iter().copied().find(|b| b.bin == selected);
        }
    }
    plan
}

fn ask_profile(default_profile: Profile) -> Profile {
    println!("Profile [{}]:", default_profile.name());
    println!("  1. minimal");
    println!("  2. daily");
    println!("  3. laptop");
    println!("  4. gaming");
    println!("  5. custom");
    print!("Choice [default: {}]: ", default_profile.name());
    io::stdout().flush().ok();

    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return default_profile;
    }
    match line.trim() {
        "" => default_profile,
        "1" | "minimal" => Profile::Minimal,
        "2" | "daily" => Profile::Daily,
        "3" | "laptop" => Profile::Laptop,
        "4" | "gaming" => Profile::Gaming,
        "5" | "custom" => Profile::Custom,
        _ => default_profile,
    }
}

fn ask_choice(prompt: &str, choices: &[String]) -> Option<String> {
    println!("{prompt}:");
    for (idx, choice) in choices.iter().enumerate() {
        println!("  {}. {}", idx + 1, choice);
    }
    print!("Choice [default: 1]: ");
    io::stdout().flush().ok();

    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return choices.first().cloned();
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return choices.first().cloned();
    }
    trimmed
        .parse::<usize>()
        .ok()
        .and_then(|n| choices.get(n.saturating_sub(1)))
        .cloned()
}

fn print_summary(plan: &Plan, d: &Detection) {
    println!("Maverick First Flight");
    println!("profile: {}", plan.profile.name());
    println!(
        "terminal: {}",
        plan.terminal.as_deref().unwrap_or("not found")
    );
    println!(
        "launcher: {}",
        plan.launcher.map_or("not found", |launcher| launcher.bin)
    );
    println!("bar: {}", plan.bar.map_or("none", |bar| bar.bin));
    println!("compositor: {}", yes_no(plan.compositor_enabled));
    println!("monitors: {}", list_or_none(&d.monitors));
    if plan.terminal.is_none() {
        println!("warning: no terminal was found; install one before starting Maverick");
    }
    if plan.launcher.is_none() {
        println!("warning: no launcher was found; Super+p will be omitted");
    }
}

fn render_config(plan: &Plan) -> String {
    let mut out = String::new();
    out.push_str("# Generated by maverick-setup (Maverick First Flight).\n");
    out.push_str("# Keep this file small: omitted keys use Maverick's compiled defaults.\n\n");
    out.push_str("[general]\n");
    push_kv(&mut out, "border_width", &plan.border_width.to_string());
    push_kv(&mut out, "gaps_inner", &plan.gaps_inner.to_string());
    push_kv(&mut out, "gaps_outer", &plan.gaps_outer.to_string());
    push_kv(
        &mut out,
        "compositor_enabled",
        if plan.compositor_enabled {
            "true"
        } else {
            "false"
        },
    );
    push_kv(&mut out, "focus_mouse", bool_lit(plan.focus_mouse));
    push_kv(&mut out, "warp_cursor", bool_lit(plan.warp_cursor));
    push_kv(&mut out, "theme", &toml_str(plan.theme));
    out.push('\n');

    render_keybindings(&mut out, plan);
    render_rules(&mut out, plan.profile);
    render_autostart(&mut out, plan);
    out
}

fn push_kv(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(" = ");
    out.push_str(value);
    out.push('\n');
}

fn bool_lit(v: bool) -> &'static str {
    if v {
        "true"
    } else {
        "false"
    }
}

fn render_keybindings(out: &mut String, plan: &Plan) {
    let terminal = plan.terminal.as_deref().unwrap_or("xterm");
    push_bind(out, "Mod4+Return", &format!("spawn:{terminal}"));
    if let Some(launcher) = plan.launcher {
        push_bind(out, "Mod4+p", &format!("spawn:{}", launcher.drun.join(" ")));
        push_bind(
            out,
            "Mod4+Shift+p",
            &format!("spawn:{}", launcher.run.join(" ")),
        );
    }

    for (key, action) in [
        ("Mod4+Shift+c", "kill"),
        ("Mod4+Shift+space", "toggle_float"),
        ("Mod4+Shift+f", "toggle_fullscreen"),
        ("Mod4+Shift+m", "toggle_maximize"),
        ("Mod4+h", "focus:left"),
        ("Mod4+l", "focus:right"),
        ("Mod4+j", "focus:down"),
        ("Mod4+k", "focus:up"),
        ("Mod4+Shift+h", "move:left"),
        ("Mod4+Shift+l", "move:right"),
        ("Mod4+Shift+j", "move:down"),
        ("Mod4+Shift+k", "move:up"),
        ("Mod4+Shift+Return", "new_column"),
        ("Mod4+Control+h", "grow_col:-50"),
        ("Mod4+Control+l", "grow_col:50"),
        ("Mod4+Control+j", "collapse_column"),
        ("Mod4+space", "set_layout:column"),
        ("Mod4+g", "layout:grid"),
        ("Mod4+t", "layout:column"),
        ("Mod4+Shift+q", "quit"),
        ("Mod4+Shift+r", "restart"),
        ("Mod4+F5", "restart"),
        ("Mod4+Tab", "focus_mon:next"),
        ("Mod4+Shift+Tab", "move_mon:next"),
        ("Mod4+o", "toggle_overview"),
        ("Mod4+n", "overview_nav:right"),
        ("Mod4+Shift+o", "overview_nav:left"),
        ("Mod4+e", "overview_enter"),
        ("Mod4+equal", "viewport_zoom:0.2"),
        ("Mod4+minus", "viewport_zoom:-0.2"),
        ("Mod4+bracketright", "page_snap:right"),
        ("Mod4+bracketleft", "page_snap:left"),
    ] {
        push_bind(out, key, action);
    }
}

fn push_bind(out: &mut String, key: &str, action: &str) {
    out.push_str("[[keybindings]]\n");
    push_kv(out, "key", &toml_str(key));
    push_kv(out, "action", &toml_str(action));
    out.push('\n');
}

fn render_rules(out: &mut String, profile: Profile) {
    for window_type in ["dialog", "utility", "splash"] {
        out.push_str("[[rules]]\n");
        push_kv(out, "window_type", &toml_str(window_type));
        push_kv(out, "float", "true");
        out.push('\n');
    }

    for class in ["xdg-desktop-portal", "pinentry", "gpick"] {
        out.push_str("[[rules]]\n");
        push_kv(out, "class", &toml_str(class));
        push_kv(out, "float", "true");
        out.push('\n');
    }

    if matches!(profile, Profile::Gaming) {
        for class in ["Steam", "steam_app_", "lutris", "heroic"] {
            out.push_str("[[rules]]\n");
            push_kv(out, "class", &toml_str(class));
            push_kv(out, "true_fullscreen", "true");
            out.push('\n');
        }
    }
}

fn render_autostart(out: &mut String, plan: &Plan) {
    let mut commands: Vec<Vec<String>> = Vec::new();
    if plan.autostart_portals {
        for portal in [
            "/usr/lib/xdg-desktop-portal-gtk",
            "/usr/lib/xdg-desktop-portal",
        ] {
            if Path::new(portal).exists() {
                commands.push(vec![portal.to_string()]);
            }
        }
    }
    if let Some(bar) = plan.bar {
        commands.push(bar.command.iter().map(|s| (*s).to_string()).collect());
    }
    if commands.is_empty() {
        return;
    }

    out.push_str("[autostart]\n");
    out.push_str("commands = [\n");
    for command in commands {
        out.push_str("  [");
        for (idx, part) in command.iter().enumerate() {
            if idx > 0 {
                out.push_str(", ");
            }
            out.push_str(&toml_str(part));
        }
        out.push_str("],\n");
    }
    out.push_str("]\n");
}

fn toml_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn current_path() -> String {
    env::var("PATH")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into())
}

fn find_in_path(program: &str, path_var: &str) -> Option<PathBuf> {
    env::split_paths(path_var)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn detect_monitors() -> Vec<String> {
    let Ok(output) = Command::new("xrandr")
        .arg("--query")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter(|line| line.contains(" connected"))
        .map(|line| {
            line.split_whitespace()
                .take(3)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".into()
    } else {
        items.join(", ")
    }
}

fn yes_no(v: bool) -> &'static str {
    if v {
        "yes"
    } else {
        "no"
    }
}

fn default_config_path() -> PathBuf {
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("maverick/config.toml");
    }
    let home = env::var_os("HOME").unwrap_or_else(|| OsString::from("."));
    PathBuf::from(home).join(".config/maverick/config.toml")
}

fn write_config(path: &Path, config: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, config)
}

fn validate_config(path: &Path) -> Result<(), String> {
    let validator = current_exe_sibling("maverick")
        .filter(|p| is_executable(p))
        .or_else(|| find_in_path("maverick", &current_path()))
        .unwrap_or_else(|| PathBuf::from("maverick"));
    let output = Command::new(validator)
        .arg("--check-config")
        .arg(path)
        .output()
        .map_err(|e| format!("failed to run maverick --check-config: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let mut msg = String::new();
        msg.push_str(&String::from_utf8_lossy(&output.stdout));
        msg.push_str(&String::from_utf8_lossy(&output.stderr));
        Err(msg)
    }
}

fn current_exe_sibling(name: &str) -> Option<PathBuf> {
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|parent| parent.join(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_plan() -> Plan {
        Plan {
            profile: Profile::Minimal,
            terminal: None,
            launcher: None,
            bar: None,
            compositor_enabled: false,
            gaps_inner: 4,
            gaps_outer: 8,
            border_width: 1,
            theme: "catppuccin-mocha",
            focus_mouse: false,
            warp_cursor: false,
            autostart_portals: false,
        }
    }

    #[test]
    fn generated_keybindings_quit_natively() {
        // The generated config is what users actually run: quitting must stay an
        // in-process action (`quit`), never a shell-out to `maverickctl`, which
        // would put a confirmation prompt and a second process on the quit path.
        let mut out = String::new();
        render_keybindings(&mut out, &minimal_plan());
        assert!(
            out.contains("key = \"Mod4+Shift+q\"\naction = \"quit\"\n"),
            "generated keybindings must bind Mod4+Shift+q to the native quit action:\n{out}"
        );
        assert!(
            !out.contains("maverickctl"),
            "generated keybindings must not spawn maverickctl:\n{out}"
        );
    }
}
