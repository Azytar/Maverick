use std::env;
use std::process::{Command, ExitCode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Task {
    Check,
    Fmt,
    Clippy,
    Test,
    Release,
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
            CLIPPY,
            TEST_ALL_FEATURES,
            TEST_DEFAULT_FEATURES,
        ]),
        Task::Fmt => run_steps(&[FMT]),
        Task::Clippy => run_steps(&[CLIPPY]),
        Task::Test => run_steps(&[TEST_ALL_FEATURES, TEST_DEFAULT_FEATURES]),
        Task::Release => run_steps(&[RELEASE]),
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
                status
                    .code()
                    .map_or_else(|| "terminated by signal".to_string(), |code| code.to_string())
            ));
        }
    }
    Ok(())
}

fn print_help() {
    println!(
        "maverick-xtask\n\n         Usage:\n  cargo run -p maverick-xtask -- <task>\n\n         Tasks:\n           check    Run format, clippy and both workspace test configurations\n           fmt      Run cargo fmt --check\n           clippy   Run workspace clippy with warnings denied\n           test     Run all-features and default-features workspace tests\n           release  Build the release maverick and maverickctl binaries\n           help     Show this help"
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
