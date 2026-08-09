use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
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

const BINARIES: &[&str] = &["maverick", "maverickctl", "maverick-dialog"];

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

fn is_root() -> bool {
    env::var("USER").map(|u| u == "root").unwrap_or(false)
}

/// Verifica si el linker de alto rendimiento `mold` está disponible en el PATH
fn has_mold() -> bool {
    Command::new("mold")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn compile_workspace(lang: Lang) -> Result<(), String> {
    let rustflags = match env::var("RUSTFLAGS") {
        Ok(existing) => format!("{} -C target-cpu=native", existing),
        Err(_) => "-C target-cpu=native".to_string(),
    };

    let use_mold = has_mold();
    if use_mold {
        println!(
            "  {} [OK] {}{}",
            GREEN,
            lang.msg(
                "Linker de alto rendimiento 'mold' detectado",
                "High-performance 'mold' linker detected"
            ),
            RESET
        );
    }

    let mut cmd = if use_mold {
        let mut c = Command::new("mold");
        c.arg("-run").arg("cargo");
        c
    } else {
        Command::new("cargo")
    };

    let mut child = cmd
        .arg("build")
        .arg("--release")
        .arg("--workspace")
        .env("RUSTFLAGS", rustflags)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {}", lang.msg("No se pudo ejecutar la compilación", "Failed to spawn build command"), e))?;

    let stderr = child.stderr.take().unwrap();
    let mut reader = BufReader::new(stderr);

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
            }
            Err(_) => break,
        }
    }

    let status = child
        .wait()
        .map_err(|e| format!("{}: {}", lang.msg("Fallo al esperar el proceso de compilación", "Failed waiting for build process"), e))?;

    if status.success() {
        println!(
            "\r\x1b[K  {} [OK] {}{}",
            GREEN,
            lang.msg("Compilación completada con éxito", "Build completed successfully"),
            RESET
        );
        Ok(())
    } else {
        Err(lang
            .msg("La compilación falló", "Compilation failed")
            .to_string())
    }
}

fn install_binaries(target_dir: &Path, lang: Lang) -> Result<(), String> {
    if !target_dir.exists() {
        fs::create_dir_all(target_dir).map_err(|e| {
            format!(
                "{}: {}",
                lang.msg("No se pudo crear el directorio", "Failed to create directory"),
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
