//! The contract the private runtime directory has to keep.
//!
//! `XDG_RUNTIME_DIR` is a process-global, and every `maverickctl` test binary
//! points it at a directory of its own so the properties do not depend on
//! whether the machine running them has a live window manager, and so no test
//! can read or delete the developer's real sessions. One shared helper now
//! answers for all of them, which is the shape in which a mistake here stays
//! invisible: a wrong path or a lost per-process component still compiles, still
//! runs, and only shows up as a property that passes for the wrong reason or
//! fails against a neighbour.
//!
//! These tests are the contract, not the call sites. `dir_for` is pure, so the
//! naming rules are checked without creating anything; `isolate` is exercised in
//! its own binary, because the one thing it does that `dir_for` cannot — mutate
//! the environment — is process-global and would race the fixtures of whichever
//! binary compiled the module.

mod runtime_dir;

use runtime_dir::{dir_for, isolate};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Hold the environment for as long as a test is asserting on it.
///
/// `XDG_RUNTIME_DIR` is one variable for the whole process and `isolate` writes
/// it, so the tests that read it back would otherwise race each other into
/// seeing a neighbour's directory. Scoped to this file: the binaries that
/// declare the module for real fixtures keep it to themselves.
fn exclusive_env() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The process id is the only component that keeps two binaries apart.
///
/// The prefix is documentation: a stray directory in `$TMPDIR` is attributable
/// to the binary that left it. Drop the id and two binaries running at the same
/// moment resolve to one directory, where each one's identity ficha satisfies
/// the other's discovery — which is the claim `ctl_replies` and `ctl_props`
/// depend on.
#[test]
fn the_process_id_is_part_of_the_runtime_directory_name() {
    let name = dir_for("maverick-contract");
    let name = name.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        name,
        format!("maverick-contract-{}", std::process::id()),
        "the runtime directory must be the prefix followed by the process id, or \
         two binaries running at once share one"
    );
}

/// A prefix is a namespace, not decoration.
///
/// Two binaries that passed the same prefix would differ only by process id,
/// which is enough while both are alive and useless once a stale directory has
/// to be told apart from a live one by hand.
#[test]
fn distinct_prefixes_resolve_to_distinct_directories() {
    let a = dir_for("maverick-ns-a");
    let b = dir_for("maverick-ns-b");
    assert_ne!(
        a, b,
        "two prefixes must not resolve to the same directory, or one binary's \
         fixtures satisfy the other's discovery"
    );
    assert_eq!(
        a.parent(),
        b.parent(),
        "both must live under the temporary directory, which is what keeps them \
         out of the runtime directory the developer's own session uses"
    );
}

/// The name is a function of the prefix and the process, and of nothing else.
///
/// No clock, no counter, no randomness: two calls in one process have to agree,
/// because the fixture socket and the record written for it are written from
/// different tests that run in parallel.
#[test]
fn the_name_does_not_vary_between_calls_within_one_process() {
    assert_eq!(dir_for("maverick-stable"), dir_for("maverick-stable"));
    // A different prefix must not perturb the first one's answer either: the
    // second binary to ask is not the first one's business.
    let _ = dir_for("maverick-other");
    assert_eq!(dir_for("maverick-stable"), dir_for("maverick-stable"));
}

/// `isolate` publishes the directory it created, through the environment.
///
/// The variable is the contract — nothing reads the path back by name — so a
/// helper that computed the right path and never published it would leave the
/// CLI resolving the developer's real instance.
#[test]
fn isolate_creates_the_directory_and_publishes_it() {
    let _env = exclusive_env();
    let dir = isolate("maverick-contract-published");
    assert!(dir.is_dir(), "the runtime directory must exist: {dir:?}");
    assert_eq!(
        dir.file_name().unwrap().to_string_lossy(),
        format!("maverick-contract-published-{}", std::process::id()),
        "isolate must publish the same path dir_for computes for this prefix"
    );
    assert_eq!(
        std::env::var("XDG_RUNTIME_DIR").ok().map(PathBuf::from),
        Some(dir),
        "XDG_RUNTIME_DIR must name the directory that was created, or the CLI \
         resolves some other instance"
    );
}

/// Set once per process, so a test that moves the target cannot be undone.
///
/// `cli_options` has a test that points the variable at a fresh directory to
/// observe a verb resolving no session. If `isolate` re-published on every call,
/// the next test to ask for the binary's directory would move the target back
/// under a fixture that had already been written to the old one, and the two
/// would disagree about where the session is.
#[test]
fn isolate_publishes_once_so_a_later_move_is_not_undone() {
    let _env = exclusive_env();
    let first = isolate("maverick-contract-once");
    assert_eq!(
        first,
        std::path::PathBuf::from(
            std::env::var("XDG_RUNTIME_DIR").expect("published by the first call")
        )
    );

    // Stand in for the test that needs a different directory mid-binary.
    let elsewhere = dir_for("maverick-contract-elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("scratch directory");
    std::env::set_var("XDG_RUNTIME_DIR", &elsewhere);

    // The second call must not move it back: the fixtures already published to
    // `first` are what the environment is expected to be pointing at.
    assert_eq!(
        isolate("maverick-contract-once"),
        first,
        "isolate must return the same path it published first"
    );
    assert_eq!(
        std::env::var("XDG_RUNTIME_DIR").ok().map(PathBuf::from),
        Some(elsewhere),
        "a second isolate call republished the environment and undid a test that \
         had deliberately moved it"
    );

    std::env::set_var("XDG_RUNTIME_DIR", &first);
}

/// An existing directory is reused, not emptied.
///
/// Nothing here removes what is already there, and that is the contract: a
/// fixture another test in this binary published is state, not garbage. A
// recycled process id can therefore land on a previous run's directory — the
/// alternative would be a helper that deletes a live binary's fixtures, which is
/// the failure this module exists to prevent.
#[test]
fn a_fixture_published_before_isolation_survives_it() {
    let _env = exclusive_env();
    let dir = dir_for("maverick-contract-kept");
    std::fs::create_dir_all(&dir).expect("scratch directory");
    let ficha = dir.join("maverick-identity.json");
    std::fs::write(&ficha, b"{}").expect("seed a fixture");

    let published = isolate("maverick-contract-kept");
    assert_eq!(published, dir, "isolate must reuse the existing directory");
    assert!(
        Path::new(&ficha).exists(),
        "isolate emptied a directory a fixture was already published in: {ficha:?}"
    );
    std::fs::remove_file(&ficha).ok();
}

/// Two binaries' namespaces do not see each other's fixtures.
///
/// The mechanism a property relies on when it says "no instance is
/// discoverable": the fixture is a file in one directory, and the other
/// directory is a different path. If isolation ever collapsed to one shared
/// directory, a fixture would satisfy a test that asserts the opposite.
#[test]
fn a_fixture_in_one_namespace_is_invisible_to_another() {
    let a = dir_for("maverick-vis-a");
    let b = dir_for("maverick-vis-b");
    std::fs::create_dir_all(&a).expect("scratch directory");
    std::fs::create_dir_all(&b).expect("scratch directory");

    // The shape `maverick_sys::identity` publishes: what discovery looks for.
    let ficha = a.join("maverick-identity.json");
    std::fs::write(&ficha, b"{\"name\":\"a\"}").expect("publish a fixture");

    assert!(
        !b.join("maverick-identity.json").exists(),
        "a fixture published under one prefix was visible under another, so a \
         test asserting that no instance is discoverable would pass for the \
         wrong reason"
    );

    std::fs::remove_file(&ficha).ok();
    std::fs::remove_dir(&a).ok();
    std::fs::remove_dir(&b).ok();
}

/// Each binary keeps a namespace of its own.
///
/// The process id already keeps two live binaries apart, so a shared prefix
/// costs nothing at run time — which is why passing one is invisible: nothing
/// fails. What it costs is the prefix's only job. A directory left in `$TMPDIR`
/// by a run that died becomes unattributable, and the two namespaces the
/// documentation promises to be separable stop being so.
///
/// Checked against the sources because the prefixes are arguments at seven call
/// sites in seven crates, and no single process can observe another's choice.
#[test]
fn every_binary_asks_for_its_own_namespace() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).expect("the tests directory") {
        let path = entry.expect("a readable directory entry").path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let name = path
            .file_name()
            .expect("a named entry")
            .to_string_lossy()
            .into_owned();
        let source = std::fs::read_to_string(&path).expect("a readable test source");
        for line in source.lines() {
            let Some(rest) = line.split("runtime_dir::isolate(").nth(1) else {
                continue;
            };
            let prefix = rest
                .trim_start()
                .trim_start_matches('"')
                .split('"')
                .next()
                .expect("a closed string literal")
                .to_string();
            owners.entry(prefix).or_default().push(name.clone());
        }
    }

    assert!(
        owners.len() >= 2,
        "expected one namespace per test binary, found {:?}",
        owners
    );
    for (prefix, mut users) in owners {
        users.sort();
        users.dedup();
        assert_eq!(
            users.len(),
            1,
            "namespace `{prefix}` is shared by {users:?}; each binary names its \
             own so a directory left in the temporary directory can be traced \
             back to the run that made it"
        );
    }
}
