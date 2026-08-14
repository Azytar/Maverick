use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// Estilos ANSI limpios (ASCII puro, cero emojis)
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

// `maverick-msg` and `maverickctl` both come from `maverick-sys/src/bin/`;
// both are built by the workspace build, so both get installed.
const BINARIES: &[&str] = &["maverick", "maverickctl", "maverick-msg", "maverick-dialog"];

#[derive(Clone, Copy)]
enum Lang {
    Es,
    En,
}

impl Lang {
    fn detect() -> Self {
        let lang_env = env::var("LC_ALL")
            .or_else(|_| env::var("LC_MESSAGES"))
            .or_else(|_| env::var("LANG"))
            .unwrap_or_default();

        if lang_env.to_lowercase().starts_with("es") {
            Lang::Es
        } else {
            Lang::En
        }
    }

    fn msg<'a>(&self, es: &'a str, en: &'a str) -> &'a str {
        match self {
            Lang::Es => es,
            Lang::En => en,
        }
    }
}

fn main() {
    let lang = Lang::detect();
    print_banner(lang);

    // 1. Detección de CPU vía CPUID directo
    let cpu_model = get_cpu_model();
    println!(
        "{} [1/5] {}:{} {}",
        CYAN,
        lang.msg("Hardware detectado (CPUID)", "Hardware detected (CPUID)"),
        RESET,
        cpu_model
    );

    // 2. Definir ruta de instalación
    let is_root = is_root();
    let install_dir = if is_root {
        PathBuf::from("/usr/local/bin")
    } else {
        let home_msg = lang.msg(
            "No se encontró la variable $HOME",
            "$HOME environment variable not found",
        );
        let home = env::var("HOME").expect(home_msg);
        PathBuf::from(home).join(".local/bin")
    };

    println!(
        "{} [2/5] {}:{} {}{}{}",
        CYAN,
        lang.msg("Destino de instalación", "Installation target"),
        RESET,
        BOLD,
        install_dir.display(),
        RESET
    );

    // 3. Compilación nativa optimizada (+ detección de mold)
    println!(
        "{} [3/5] {} (-C target-cpu=native)...{}",
        CYAN,
        lang.msg(
            "Compilando con optimizaciones de arquitectura",
            "Building with native CPU optimizations"
        ),
        RESET
    );

    if let Err(e) = compile_workspace(lang) {
        eprintln!(
            "\n{} [ERR] {}:{}\n{}",
            RED,
            lang.msg("Error durante la compilación", "Build failed"),
            RESET,
            e
        );
        std::process::exit(1);
    }

    // 4. Copia e instalación de binarios
    println!(
        "\n{} [4/5] {}...{}",
        CYAN,
        lang.msg(
            "Instalando binarios del workspace",
            "Installing workspace binaries"
        ),
        RESET
    );
    if let Err(e) = install_binaries(&install_dir, lang) {
        eprintln!(
            "\n{} [ERR] {}:{}\n{}",
            RED,
            lang.msg("Error instalando binarios", "Binary installation failed"),
            RESET,
            e
        );
        std::process::exit(1);
    }

    // 5. Configuración del sistema (Desktop Entry & PATH)
    println!(
        "{} [5/5] {}...{}",
        CYAN,
        lang.msg(
            "Verificando integración del sistema",
            "Checking system integration"
        ),
        RESET
    );
    check_path_variable(&install_dir, lang);
    install_desktop_entry(is_root, lang);

    // Resumen final
    println!("\n{}{}==", BOLD, GREEN);
    println!(
        "  {}",
        lang.msg(
            "Maverick se ha instalado correctamente",
            "Maverick installed successfully"
        )
    );
    println!(
        "  {}: {}",
        lang.msg("Ubicación", "Target path"),
        install_dir.display()
    );
    println!(
        "  {}:  {}",
        lang.msg("Binarios", "Binaries"),
        BINARIES.join(", ")
    );
    println!(
        "  {}: {}",
        lang.msg("Optimizado para", "Optimized for"),
        cpu_model
    );
    println!("=={}\n", RESET);
}

fn print_banner(lang: Lang) {
    println!(
        "{}{}\n  === MAVERICK NATIVE BUILD & SETUP ENGINE ==={}\n  [*] {}: {}\n",
        BOLD,
        CYAN,
        RESET,
        lang.msg("Idioma detectado", "Detected language"),
        lang.msg("Español", "English")
    );
}

/// Obtiene la marca del procesador directamente llamando a la instrucción CPUID de x86_64
#[cfg(target_arch = "x86_64")]
fn get_cpu_model() -> String {
    use std::arch::x86_64::__cpuid;

    // CPUID is safe on x86_64: every supported CPU implements it.
    let extended_max = __cpuid(0x8000_0000).eax;
    if extended_max >= 0x8000_0004 {
        let mut bytes = Vec::with_capacity(48);
        for leaf in 0x8000_0002..=0x8000_0004 {
            let cpuid = __cpuid(leaf);
            bytes.extend_from_slice(&cpuid.eax.to_le_bytes());
            bytes.extend_from_slice(&cpuid.ebx.to_le_bytes());
            bytes.extend_from_slice(&cpuid.ecx.to_le_bytes());
            bytes.extend_from_slice(&cpuid.edx.to_le_bytes());
        }
        if let Ok(brand) = std::str::from_utf8(&bytes) {
            let trimmed = brand.trim_matches('\0').trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    fallback_cpu_model()
}

#[cfg(not(target_arch = "x86_64"))]
fn get_cpu_model() -> String {
    fallback_cpu_model()
}

fn fallback_cpu_model() -> String {
    if let Ok(cpuinfo) = fs::read_to_string("/proc/cpuinfo") {
        for line in cpuinfo.lines() {
            if line.starts_with("model name") {
                if let Some((_, model)) = line.split_once(':') {
                    return model.trim().to_string();
                }
            }
        }
    }
    env::consts::ARCH.to_string()
}

/// Effective UID (0 == root). Reads `/proc/self/status` so it is correct under
/// `sudo` (where `$USER` may still be the invoking user) without extra crates.
fn effective_uid() -> u32 {
    if let Ok(status) = fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                // Uid: <real> <effective> <saved> <fs>
                if let Some(eff) = rest.split_whitespace().nth(1) {
                    if let Ok(uid) = eff.parse::<u32>() {
                        return uid;
                    }
                }
            }
        }
    }
    if env::var("USER").map(|u| u == "root").unwrap_or(false) {
        return 0;
    }
    u32::MAX
}

fn is_root() -> bool {
    effective_uid() == 0
}

/// When running as root (typically via `sudo`), return the unprivileged user
/// that invoked us, so the build can run as them and `target/` stays theirs.
fn build_user() -> Option<String> {
    if !is_root() {
        return None;
    }
    if let Ok(u) = env::var("SUDO_USER") {
        if !u.is_empty() && u != "root" {
            return Some(u);
        }
    }
    None
}

/// PATH of the current process, with a sane fallback for `sudo`'s stripped env.
fn current_path() -> String {
    env::var("PATH")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| {
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string()
        })
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// First executable named `program` found in a PATH-like string.
fn find_in_path(program: &str, path_var: &str) -> Option<PathBuf> {
    env::split_paths(path_var)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

fn warn(msg: &str) {
    println!("  {} [WARN] {}{}", YELLOW, msg, RESET);
}

/// `(uid, gid, home)` for `user`, via `getent` (works with LDAP/SSSD) and
/// falling back to `/etc/passwd`.
fn passwd_entry(user: &str) -> Option<(u32, u32, PathBuf)> {
    let line = Command::new("getent")
        .arg("passwd")
        .arg(user)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|out| out.lines().next().map(str::to_string))
        .or_else(|| {
            let passwd = fs::read_to_string("/etc/passwd").ok()?;
            passwd
                .lines()
                .find(|l| l.split(':').next() == Some(user))
                .map(str::to_string)
        })?;

    // name:passwd:uid:gid:gecos:home:shell
    let fields: Vec<&str> = line.split(':').collect();
    if fields.len() < 6 {
        return None;
    }
    let uid = fields[2].parse().ok()?;
    let gid = fields[3].parse().ok()?;
    let home = fields[5];
    if home.is_empty() {
        return None;
    }
    Some((uid, gid, PathBuf::from(home)))
}

/// Everything needed to run the build as the unprivileged invoking user.
struct BuildAs {
    user: String,
    uid: u32,
    gid: u32,
    home: PathBuf,
    /// argv prefix that drops privileges, e.g. `sudo -n -u <user> --`.
    dropper: Vec<String>,
    /// PATH the build runs with — the user's `~/.cargo/bin` comes first, since
    /// rustup installs there and root's `secure_path` never includes it.
    path: String,
}

/// Decide whether (and how) the build can be de-escalated to the invoking user.
fn build_as(lang: Lang) -> Option<BuildAs> {
    let user = build_user()?;

    let (uid, gid, home) = match passwd_entry(&user) {
        Some(entry) => entry,
        None => {
            warn(lang.msg(
                "No se pudo resolver el usuario que invocó sudo; se compilará como root",
                "Could not resolve the invoking user; building as root",
            ));
            return None;
        }
    };

    let root_path = current_path();
    let dropper = if let Some(sudo) = find_in_path("sudo", &root_path) {
        // `-n`: we are already root, so this must never stop to ask a password.
        vec![
            sudo.display().to_string(),
            "-n".to_string(),
            "-u".to_string(),
            user.clone(),
            "--".to_string(),
        ]
    } else if let Some(runuser) = find_in_path("runuser", &root_path) {
        vec![
            runuser.display().to_string(),
            "-u".to_string(),
            user.clone(),
            "--".to_string(),
        ]
    } else {
        warn(lang.msg(
            "Ni 'sudo' ni 'runuser' disponibles; se compilará como root",
            "Neither 'sudo' nor 'runuser' available; building as root",
        ));
        return None;
    };

    let path = format!("{}:{}", home.join(".cargo/bin").display(), root_path);

    Some(BuildAs {
        user,
        uid,
        gid,
        home,
        dropper,
        path,
    })
}

/// True as soon as any entry under `dir` is not owned by `uid`. Uses
/// `symlink_metadata` so a symlink pointing outside the tree is judged by the
/// link itself, never by its target.
fn has_foreign_owner(dir: &Path, uid: u32) -> bool {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return false,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.uid() != uid {
            return true;
        }
        if meta.is_dir() && has_foreign_owner(&path, uid) {
            return true;
        }
    }

    false
}

/// An earlier root build may have left artifacts inside `target/` owned by
/// root, which makes the user's own `cargo build` fail afterwards. While we
/// still have privileges, hand the tree back.
///
/// The whole tree is checked, not just the top directory: `target/` itself is
/// usually still owned by the user (they created it) while the artifacts a root
/// build rewrote underneath are not.
fn repair_target_ownership(build: &BuildAs, lang: Lang) {
    if build.uid == 0 {
        return;
    }

    let target = Path::new("target");
    let owned_by_user = match fs::symlink_metadata(target) {
        Ok(meta) => meta.uid() == build.uid,
        // No target/ yet: nothing to repair.
        Err(_) => return,
    };

    if owned_by_user && !has_foreign_owner(target, build.uid) {
        return;
    }

    let chown = find_in_path("chown", &current_path()).unwrap_or_else(|| PathBuf::from("chown"));
    let ok = Command::new(chown)
        .arg("-R")
        .arg(format!("{}:{}", build.uid, build.gid))
        .arg(target)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok {
        println!(
            "  {} [OK] {} {}{}{}",
            GREEN,
            lang.msg(
                "'target/' (con archivos de root) devuelto a",
                "root-owned files in 'target/' handed back to"
            ),
            BOLD,
            build.user,
            RESET
        );
    } else {
        warn(lang.msg(
            "No se pudo restaurar el propietario de 'target/'",
            "Could not restore ownership of 'target/'",
        ));
    }
}

fn compile_workspace(lang: Lang) -> Result<(), String> {
    let rustflags = match env::var("RUSTFLAGS") {
        Ok(existing) if !existing.trim().is_empty() => {
            format!("{} -C target-cpu=native", existing)
        }
        _ => "-C target-cpu=native".to_string(),
    };

    // Under `sudo` the build must NOT run as root: cargo would write every
    // artifact in `target/` as root and break the user's later builds. Only the
    // copy into /usr/local/bin actually needs privileges.
    let dropped = build_as(lang);
    if dropped.is_none() && is_root() {
        warn(lang.msg(
            "Compilando como root: 'target/' quedará en propiedad de root",
            "Building as root: 'target/' will be left owned by root",
        ));
    }

    if let Some(build) = dropped.as_ref() {
        repair_target_ownership(build, lang);
        println!(
            "  {} [OK] {}:{} {}{}{}",
            GREEN,
            lang.msg(
                "Compilando sin privilegios como",
                "Building unprivileged as"
            ),
            RESET,
            BOLD,
            build.user,
            RESET
        );
    }

    let search_path = dropped
        .as_ref()
        .map(|b| b.path.clone())
        .unwrap_or_else(current_path);

    // Resolve the toolchain against the *build* PATH, not root's.
    let cargo = find_in_path("cargo", &search_path).unwrap_or_else(|| PathBuf::from("cargo"));

    let mut argv: Vec<String> = Vec::new();
    if let Some(mold) = find_in_path("mold", &search_path) {
        println!(
            "  {} [OK] {}{}",
            GREEN,
            lang.msg(
                "Linker de alto rendimiento 'mold' detectado",
                "High-performance 'mold' linker detected"
            ),
            RESET
        );
        argv.push(mold.display().to_string());
        argv.push("-run".to_string());
    }
    argv.push(cargo.display().to_string());
    for arg in [
        "build",
        "--release",
        "--workspace",
        "--exclude",
        "maverick-installer",
    ] {
        argv.push(arg.to_string());
    }

    let mut cmd = match dropped.as_ref() {
        Some(build) => {
            // `sudo`/`runuser` reset the environment, so the build env is
            // rebuilt explicitly through `env` on the far side of the drop.
            let env_bin =
                find_in_path("env", &current_path()).unwrap_or_else(|| PathBuf::from("env"));
            let mut c = Command::new(&build.dropper[0]);
            c.args(&build.dropper[1..]);
            c.arg(env_bin);
            c.arg(format!("HOME={}", build.home.display()));
            c.arg(format!("PATH={}", build.path));
            c.arg(format!("RUSTFLAGS={}", rustflags));
            c.args(&argv);
            c
        }
        None => {
            let mut c = Command::new(&argv[0]);
            c.args(&argv[1..]);
            c.env("RUSTFLAGS", &rustflags);
            c.env("PATH", &search_path);
            c
        }
    };

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "{}: {}",
                lang.msg(
                    "No se pudo ejecutar la compilación",
                    "Failed to spawn build command"
                ),
                e
            )
        })?;

    let stderr = child.stderr.take().unwrap();
    let mut reader = BufReader::new(stderr);

    // Keep a short tail so a failure (bad sudo policy, missing cargo, a real
    // compile error) reports something actionable instead of just "failed".
    let mut tail: Vec<String> = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let line = line.trim_end();
                if line.contains("Compiling") || line.contains("Finished") {
                    print!("\r\x1b[K  {}{}{}", DIM, line, RESET);
                    std::io::stdout().flush().ok();
                }
                if !line.trim().is_empty() {
                    tail.push(line.to_string());
                    if tail.len() > 20 {
                        tail.remove(0);
                    }
                }
            }
            Err(_) => break,
        }
    }

    let status = child.wait().map_err(|e| {
        format!(
            "{}: {}",
            lang.msg(
                "Fallo al esperar el proceso de compilación",
                "Failed waiting for build process"
            ),
            e
        )
    })?;

    if status.success() {
        println!(
            "\r\x1b[K  {} [OK] {}{}",
            GREEN,
            lang.msg(
                "Compilación completada con éxito",
                "Build completed successfully"
            ),
            RESET
        );
        Ok(())
    } else {
        Err(format!(
            "{}\n{}",
            lang.msg("La compilación falló", "Compilation failed"),
            tail.join("\n")
        ))
    }
}

fn install_binaries(target_dir: &Path, lang: Lang) -> Result<(), String> {
    if !target_dir.exists() {
        fs::create_dir_all(target_dir).map_err(|e| {
            format!(
                "{}: {}",
                lang.msg(
                    "No se pudo crear el directorio",
                    "Failed to create directory"
                ),
                e
            )
        })?;
    }

    let release_dir = PathBuf::from("target/release");

    for bin in BINARIES {
        let src = release_dir.join(bin);
        let dest = target_dir.join(bin);

        if !src.exists() {
            return Err(format!(
                "{}: {}",
                lang.msg("El binario no existe", "Binary does not exist"),
                src.display()
            ));
        }

        fs::copy(&src, &dest).map_err(|e| {
            format!(
                "{} {} -> {}: {}",
                lang.msg("Error copiando", "Error copying"),
                src.display(),
                dest.display(),
                e
            )
        })?;

        let mut perms = fs::metadata(&dest).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dest, perms).ok();

        let size_kb = fs::metadata(&dest).map(|m| m.len() / 1024).unwrap_or(0);

        println!(
            "  {} [OK] Installed:{} {:<16} {}({} KB){}",
            GREEN, RESET, bin, DIM, size_kb, RESET
        );
    }

    Ok(())
}

fn check_path_variable(install_dir: &Path, lang: Lang) {
    if let Ok(path_var) = env::var("PATH") {
        let in_path = env::split_paths(&path_var).any(|p| p == install_dir);
        if !in_path {
            println!(
                "  {} [WARN] {}:{} {} {}",
                YELLOW,
                lang.msg("Advertencia", "Warning"),
                RESET,
                install_dir.display(),
                lang.msg("no está en tu $PATH.", "is not in your $PATH.")
            );
            println!(
                "    {}: {}export PATH=\"$PATH:{}\"{}",
                lang.msg("Añade esto a tu ~/.bashrc", "Add this to your ~/.bashrc"),
                BOLD,
                install_dir.display(),
                RESET
            );
        } else {
            println!(
                "  {} [OK] {}{}",
                GREEN,
                lang.msg(
                    "El directorio está en el $PATH",
                    "Directory is present in $PATH"
                ),
                RESET
            );
        }
    }
}

fn install_desktop_entry(is_root: bool, lang: Lang) {
    let desktop_dir = if is_root {
        PathBuf::from("/usr/share/xsessions")
    } else {
        let home = env::var("HOME").unwrap_or_default();
        PathBuf::from(home).join(".local/share/xsessions")
    };

    let desktop_content = r#"[Desktop Entry]
Name=maverick
Comment=Columnar tiling WM
Exec=maverick
Type=XSession
"#;

    if fs::create_dir_all(&desktop_dir).is_ok() {
        let file_path = desktop_dir.join("maverick.desktop");
        if fs::write(&file_path, desktop_content).is_ok() {
            println!(
                "  {} [OK] {}:{} {}",
                GREEN,
                lang.msg("Entrada XSession creada en", "XSession entry created at"),
                RESET,
                file_path.display()
            );
        }
    }
}
