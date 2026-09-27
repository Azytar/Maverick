# Maverick Distributed Test Lab — Auditoría

## Fecha: 2026-09-26

---

## PC A — Development/Build Machine

### Hardware
| Componente | Valor |
|------------|-------|
| CPU | Intel Core i5-7300HQ @ 2.50GHz |
| Núcleos | 4 |
| Hilos | 4 (1 hilo por núcleo, sin HT) |
| RAM | 15 GB |
| Arquitectura | x86_64 |

### Sistema Operativo
| Componente | Valor |
|------------|-------|
| OS | Arch Linux |
| Kernel | Linux 7.2.6-zen2-1-zen |
| Hostname | blackout |
| glibc | 2.44 |
| systemd | 261 (261.3-1-arch) |

### Red
| Interfaz | IP | Estado |
|----------|-----|--------|
| lo | 127.0.0.1/8 | UP |
| enp3s0 | — | DOWN |
| wlp2s0 | 192.168.18.24/24 | UP |

### Usuario Actual
| Campo | Valor |
|-------|-------|
| Usuario | (omitido) |
| UID | 1000 |
| GID | 1000 |
| Grupos | (omitido) |
| Shell | /bin/bash |
| Home | ~ |

### Rust Toolchain
| Componente | Valor |
|------------|-------|
| rustc | 1.98.1 (48a229cea 2026-09-01) |
| cargo | 1.98.1 (797e8a9bc 2026-08-05) |
| Instalación | Paquete Arch Linux (rust 1:1.98.1-1) |
| rustup | NO INSTALADO |
| rust-toolchain file | NO EXISTE |

### Herramientas
| Herramienta | Estado |
|-------------|--------|
| git | 2.55.0 |
| ssh | OpenSSH_10.5p1 |
| sshd | ACTIVO (enabled, running) |
| rsync | NO INSTALADO |
| taskset | /usr/bin/taskset |
| firewalld | NO INSTALADO |
| ufw | NO DISPONIBLE |
| cargo-nextest | NO INSTALADO |

### SSH
| Componente | Estado |
|------------|--------|
| ~/.ssh/ | Existe |
| Claves | NINGUNA |
| Config | NO EXISTE |
| known_hosts | Existe |

### systemd user
| Componente | Estado |
|------------|--------|
| systemctl --user | DISPONIBLE |
| linger | HABILITADO (Linger=yes) |
| ~/.config/systemd/user/ | NO EXISTE |

---

## PC B — Test Server

### Hardware
| Componente | Valor |
|------------|-------|
| CPU | AMD Ryzen 3 3250U |
| Hilos | 4 |
| Arquitectura | x86_64 (asumido, pendiente verificación) |

### Red
| Interfaz | IP | Estado |
|----------|-----|--------|
| — | 192.168.18.61 | Responde al ping |

### Acceso
| Componente | Estado |
|------------|--------|
| Usuario existente | NO EXISTE |
| Acceso | Consola física |
| SSH | Pendiente de configuración |

---

## Repositorio Maverick

### Ubicación
`/path/to/Maverick-reconstructed`

### Estructura
```
Maverick-reconstructed/
├── Cargo.toml          # Workspace con 9 crates
├── Cargo.lock
├── maverick-core/      # Núcleo puro (types, layout, commands)
│   ├── src/
│   │   ├── lib.rs
│   │   ├── types.rs    # 150 KB
│   │   └── wallpaper.rs
│   └── tests/
│       ├── common/
│       ├── animation_props.rs
│       ├── rect_geometry_props.rs
│       ├── reservation_props.rs
│       └── state_model_props.rs
├── maverick-sys/
├── maverick-x11/
├── maverick-render/
├── maverick-gl/
├── maverick-dialog/
├── maverick-toml/
├── maverick-img/
├── maverick-vk/
├── tests/              # Tests de integración X11
├── .github/workflows/ci.yml
└── docs/
```

### Dependencias de Test
- `proptest` (workspace)
- `tempfile` (workspace)

### CI Actual
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `bash -n install.sh`
- `python3 tests/install-smoke.py`
- Xvfb stacking regression

---

## Conectividad

| Desde | Hacia | Resultado |
|-------|-------|-----------|
| PC A (192.168.18.24) | PC B (192.168.18.61) | ✅ Ping OK (51-75 ms) |
| PC A | PC B (SSH) | ❌ No hay usuario/clave |

---

## Pendiente de Verificación en PC B

- [ ] `hostname`
- [ ] `uname -a`
- [ ] `cat /etc/os-release`
- [ ] `lscpu`
- [ ] `free -h`
- [ ] `nproc`
- [ ] `rustc --version`
- [ ] `cargo --version`
- [ ] `rustup show`
- [ ] `cargo nextest --version`
- [ ] `git --version`
- [ ] `ssh -V`
- [ ] `rsync --version`
- [ ] `systemctl --version`
- [ ] `ip -br addr`
- [ ] `ip route`
- [ ] Usuarios existentes
- [ ] sshd status
- [ ] Firewall status
- [ ] taskset availability
- [ ] systemd user services
