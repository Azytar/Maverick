use std::env;
use std::process::{Command, ExitCode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Task {
    Check,
    Fmt,
    Clippy,
    Test,
    Release,
    X11,
    Help,
}

impl Task {
    fn parse(arg: Option<&str>) -> Result<Self, String> {
        match arg {
            None | Some("check") => Ok(Self::Check),
            Some("fmt") => Ok(Self::Fmt),
            Some("clippy") => Ok(Self::Clippy),
            Some("test") => Ok(Self::Test),
            Some("release") => Ok(Self::Release),
            Some("x11") => Ok(Self::X11),
            Some("help" | "-h" | "--help") => Ok(Self::Help),
            Some(other) => Err(format!("unknown xtask '{other}'")),
        }
    }
}

#[derive(Clone, Copy)]
struct Step {
    label: &'static str,
    program: &'static str,
    args: &'static [&'static str],
}

const FMT: Step = Step {
    label: "format",
    program: "cargo",
    args: &["fmt", "--all", "--", "--check"],
};

const CHECK: Step = Step {
    label: "workspace check",
    program: "cargo",
    args: &["check", "--workspace", "--all-targets", "--locked"],
};

const CLIPPY: Step = Step {
    label: "clippy",
    program: "cargo",
    args: &[
        "clippy",
        "--workspace",
        "--all-targets",
        "--all-features",
        "--locked",
        "--",
        "-D",
        "warnings",
    ],
};

const CLIPPY_DEFAULT_FEATURES: Step = Step {
    label: "clippy (default features)",
    program: "cargo",
    args: &[
        "clippy",
        "--workspace",
        "--all-targets",
        "--locked",
        "--",
        "-D",
        "warnings",
    ],
};

const TEST_ALL_FEATURES: Step = Step {
    label: "tests (all features)",
    program: "cargo",
    args: &[
        "test",
        "--workspace",
        "--all-targets",
        "--all-features",
        "--locked",
    ],
};

const TEST_DEFAULT_FEATURES: Step = Step {
    label: "tests (default features)",
    program: "cargo",
    args: &["test", "--workspace", "--all-targets", "--locked"],
};

const X11_STEPS: &[Step] = &[
    Step {
        label: "stacking startup contracts",
        program: "python3",
        args: &["tests/stacking-startup.py"],
    },
    Step {
        label: "Xvfb stacking regression",
        program: "python3",
        args: &["tests/xvfb-stacking.py"],
    },
    Step {
        label: "shell harness isolation",
        program: "python3",
        args: &["tests/harness-isolation.py"],
    },
    Step {
        label: "session teardown",
        program: "python3",
        args: &["tests/session-teardown.py"],
    },
    Step {
        label: "session teardown (exec client)",
        program: "python3",
        args: &["tests/session-teardown.py", "--exec-client"],
    },
    Step {
        label: "session teardown (stale group)",
        program: "python3",
        args: &["tests/session-teardown.py", "--stale-group"],
    },
    Step {
        label: "live configuration reload",
        program: "python3",
        args: &["tests/xvfb-reload.py"],
    },
    Step {
        label: "explicit layout restoration",
        program: "python3",
        args: &["tests/xvfb-restore.py"],
    },
    Step {
        label: "floating focus",
        program: "python3",
        args: &["tests/xvfb-floats.py", "--case", "focus"],
    },
    Step {
        label: "transient stacking",
        program: "python3",
        args: &["tests/xvfb-floats.py", "--case", "transients"],
    },
    Step {
        label: "floating metadata",
        program: "python3",
        args: &["tests/xvfb-floats.py", "--case", "metadata"],
    },
    Step {
        label: "vertical floating",
        program: "python3",
        args: &["tests/xvfb-floats.py", "--case", "vertical"],
    },
    Step {
        label: "layout restart",
        program: "python3",
        args: &["tests/xvfb-restart.py"],
    },
    Step {
        label: "layout restart (pointer focus)",
        program: "python3",
        args: &["tests/xvfb-restart.py", "--pointer-focus"],
    },
    Step {
        label: "root cursor",
        program: "python3",
        args: &["tests/xvfb-cursor.py"],
    },
    Step {
        label: "root cursor theme",
        program: "python3",
        args: &["tests/xvfb-cursor-theme.py"],
    },
    Step {
        label: "input hint contracts",
        program: "python3",
        args: &["tests/input-hint-contracts.py"],
    },
    Step {
        label: "input hint X11",
        program: "python3",
        args: &[
            "tests/xvfb-input-hint.py",
            "--wm",
            "target/release/maverick",
            "--ctl",
            "target/release/maverickctl",
            "--sanitize",
        ],
    },
    Step {
        label: "input protocol",
        program: "python3",
        args: &["tests/xvfb-input.py", "--case", "protocol"],
    },
    Step {
        label: "input ownership",
        program: "python3",
        args: &["tests/xvfb-input.py", "--case", "ownership"],
    },
    Step {
        label: "input child",
        program: "python3",
        args: &["tests/xvfb-input.py", "--case", "child"],
    },
    Step {
        label: "input capture",
        program: "python3",
        args: &["tests/xvfb-input.py", "--case", "capture"],
    },
    Step {
        label: "input capture exclusive",
        program: "python3",
        args: &["tests/xvfb-input.py", "--case", "capture", "--exclusive"],
    },
    Step {
        label: "C probe campaign",
        program: "python3",
        args: &[
            "tests/c-probe-campaign.py",
            "--wm",
            "target/release/maverick",
            "--evidence",
            "/tmp/maverick-c-probes",
        ],
    },
    Step {
        label: "overview isolation",
        program: "python3",
        args: &["tests/overview-isolation.py"],
    },
    Step {
        label: "overview geometry",
        program: "python3",
        args: &["tests/xvfb-overview.py"],
    },
    Step {
        label: "overview compositor",
        program: "python3",
        args: &["tests/xvfb-overview.py", "--compositor"],
    },
    Step {
        label: "overview without composite",
        program: "python3",
        args: &["tests/xvfb-overview.py", "--without-composite"],
    },
];

const FOCUS_X11_STEPS: &[Step] = &[
    Step {
        label: "stacking measurement and external equivalence",
        program: "python3",
        args: &[
            "tests/xvfb-stacking-campaign.py",
            "--out",
            "/tmp/maverick-stage11.json",
            "--external",
        ],
    },
    Step {
        label: "focus request profile (cycle)",
        program: "python3",
        args: &[
            "tests/xvfb-floats.py",
            "--case",
            "profile",
            "--clients",
            "2",
            "--actions",
            "20",
            "--idle-seconds",
            "0.05",
            "--request-output",
            "/tmp/maverick-focus-cycle.jsonl",
        ],
    },
    Step {
        label: "focus request profile (repeat)",
        program: "python3",
        args: &[
            "tests/xvfb-floats.py",
            "--case",
            "profile",
            "--clients",
            "2",
            "--actions",
            "20",
            "--idle-seconds",
            "0.05",
            "--focus-pattern",
            "repeat",
            "--request-output",
            "/tmp/maverick-focus-repeat.jsonl",
        ],
    },
    Step {
        label: "urgent focus appearance",
        program: "python3",
        args: &["tests/xvfb-floats.py", "--case", "urgency"],
    },
];

const RELEASE: Step = Step {
    label: "release build",
    program: "cargo",
    args: &[
        "build",
        "--release",
        "-p",
        "maverick",
        "-p",
        "maverickctl",
        "--locked",
    ],
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let task = Task::parse(args.next().as_deref())?;
    if let Some(extra) = args.next() {
        return Err(format!("unexpected argument '{extra}'"));
    }

    match task {
        Task::Check => run_steps(&[
            FMT,
            CHECK,
            CLIPPY,
            CLIPPY_DEFAULT_FEATURES,
            TEST_ALL_FEATURES,
            TEST_DEFAULT_FEATURES,
        ]),
        Task::Fmt => run_steps(&[FMT]),
        Task::Clippy => run_steps(&[CLIPPY, CLIPPY_DEFAULT_FEATURES]),
        Task::Test => run_steps(&[TEST_ALL_FEATURES, TEST_DEFAULT_FEATURES]),
        Task::Release => run_steps(&[RELEASE]),
        Task::X11 => {
            run_steps(X11_STEPS)?;
            run_steps(FOCUS_X11_STEPS)
        }
        Task::Help => {
            print_help();
            Ok(())
        }
    }
}

fn run_steps(steps: &[Step]) -> Result<(), String> {
    for step in steps {
        eprintln!(
            "[xtask] {}: {} {}",
            step.label,
            step.program,
            step.args.join(" ")
        );
        let status = Command::new(step.program)
            .args(step.args)
            .status()
            .map_err(|error| format!("failed to start '{}': {error}", step.label))?;
        if !status.success() {
            return Err(format!(
                "'{}' failed with status {}",
                step.label,
                status.code().map_or_else(
                    || "terminated by signal".to_string(),
                    |code| code.to_string()
                )
            ));
        }
    }
    Ok(())
}

fn print_help() {
    println!(
        "maverick-xtask\n\n         Usage:\n  cargo run -p maverick-xtask -- <task>\n\n         Tasks:\n           check    Run format, workspace check, clippy and both test configurations\n           fmt      Run cargo fmt --check\n           clippy   Run both feature configurations with warnings denied\n           test     Run all-features and default-features workspace tests\n           release  Build the release maverick and maverickctl binaries\n           x11      Run the existing isolated X11 regression campaign\n           help     Show this help"
    );
}

#[cfg(test)]
mod tests {
    use super::Task;

    #[test]
    fn default_task_is_check() {
        assert_eq!(Task::parse(None).unwrap(), Task::Check);
    }

    #[test]
    fn known_tasks_parse() {
        for (text, expected) in [
            ("check", Task::Check),
            ("fmt", Task::Fmt),
            ("clippy", Task::Clippy),
            ("test", Task::Test),
            ("release", Task::Release),
            ("x11", Task::X11),
            ("help", Task::Help),
            ("--help", Task::Help),
        ] {
            assert_eq!(Task::parse(Some(text)).unwrap(), expected);
        }
    }

    #[test]
    fn unknown_task_is_rejected() {
        assert!(Task::parse(Some("publish")).is_err());
    }
}
