# Maverick: auditoría y endurecimiento de configuración

Fecha: 2026-11-06. Rama local: `config/compiler-hardening`.
Base: `config/toml-hardening`, commit `74fd4331` (44 commits sobre `main`).
`main` apuntaba a `807218896c05e3ffdaa565799ab523fca84e418e`.

## Resultado y límites de aceptación

Se corrigieron fallos reproducidos del parser, límites numéricos, atajos,
autostart y diagnósticos. Se conservó el frontend existente: eventos prestados,
modelo temporal mínimo y construcción de `Cfg`. No hay nuevas dependencias,
`unsafe`, formatos TOML, servicios ni módulos de producción.

La implementación está entregada con commits locales; **la fase no cumple aún
todos los criterios de aceptación**. Compilan el workspace y release, y pasan
las pruebas específicas. La batería completa mantiene fallos de entorno, Xvfb
no puede abrir sockets, y Clippy global encuentra advertencias preexistentes
fuera de configuración. No se hicieron push, merge, rebase, tags ni PR.

## Referencia y auditoría

El checkout inicial estaba limpio. Se inspeccionaron los 44 commits de la rama,
el diff de configuración contra `main`, parser, modelo `Cfg`, CLI, cargador,
recarga y sus pruebas. Tres commits heredados de la rama corresponden a
rendimiento/auditoría X11; sus cambios se conservaron.

Antes de modificar producción se ejecutaron las pruebas existentes. El parser
tenía 28 pruebas unitarias aprobadas. El binario tenía 561 aprobadas y tres
fallidas: dos pruebas defectuosas de configuración y una prueba de socket
bloqueada por el entorno. `cargo fmt --all -- --check` también fallaba en
`src/userconfig.rs`; su formato existente se regularizó con el comando pedido.
El primer intento con Rust 1.75 ni siquiera pudo leer el lockfile v4. Se instaló
Rust 1.85.1, superior al mínimo 1.82 declarado, sin cambiar manifests ni lock.

| Componente | Estado inicial / gravedad | Problema demostrado | Solución y evidencia |
|---|---|---|---|
| Parser numérico | Alto | `v = -0x10` y `v = [-0x10]` perdían el signo y producían un valor positivo | Rechazar el hexadecimal con signo; regresión en escalares y arrays |
| Parser de arrays | Alto | `v = [["a"], x"b"]]` se aceptaba al saltar un byte sin comprobar `[` | Comprobar cada apertura de fila; regresión con fila falsa y fila vacía falsa |
| Parser de strings | Medio | Un salto de línea literal dentro de `"..."` se aceptaba y falseaba posiciones posteriores | Rechazar LF, CR y CRLF dentro de strings básicos; conservar escapes y arrays multilínea |
| Validación numérica | Medio | `column_width = 1.0000000001` se redondeaba a 1 antes de validarlo; también ocurría con opacidad y otros límites | Validar el valor `f64` y convertir después a `f32`; regresiones cerca de máximos, mínimos y subdesbordamiento |
| Resolución de atajos | Alto | `+a`, `Super++a`, `Super+a+` y componentes en blanco se aceptaban al filtrarlos | Conservar cada componente y rechazar los vacíos; rechazar también NoSymbol (`0x0`) |
| Acciones compartidas | Alto | `shrink-col -2147483648` provocaba pánico por negación con overflow en debug; en release podía representar una magnitud incorrecta | `checked_neg`; prueba del extremo inválido y del máximo representable |
| Esquema | Medio | `[rules]`, `[keybindings]`, `[[general]]`, `[[colors]]` y `[[autostart]]` se ignoraban con checker limpio | Advertir en la línea de cabecera e indicar los corchetes correctos; mantener ignorado el contenido incompatible |
| Autostart | Medio | Un `commands` con tipo inválido borraba comandos del baseline en vez de descartar el valor | Construir el override únicamente al validar una lista; prueba con baseline no vacío |
| Diagnósticos | Medio | Mensajes semánticos sin ruta; errores cruzados sin ubicación; tipos esperados poco concretos | Formatear con la ruta en la salida, conservar ubicación de `[general]`, describir el tipo esperado y señalar tipos incorrectos de atajos en su línea |
| Runtime | Conservado | No se demostró necesidad de cambiar geometría, renderer o construcción del motor | El `Cfg` validado sigue siendo el límite de entrada; sólo se corrigió la negación en el parser de acciones |
| Recarga | Contrato correcto; cobertura incompleta | El test de gaps aún esperaba clamping para valores que la rama ya rechazaba | Expectativa explícita de gap compilado 8; ampliar casos de fuente inválida y ausencia de archivo. Ejecución X11 bloqueada |
| Pruebas existentes | Bajo | El helper `parse_string` descartaba diagnósticos que una prueba intentaba inspeccionar después | Usar el mismo acumulador en parse y merge; conservar todas las aserciones de campos y errores |
| Prueba del ejemplo | Bajo | Se exigía el atajo no operativo `Super+T` que el commit `7aacbb4` había retirado deliberadamente del ejemplo | Comprobar que ese único bind existe en el baseline y comparar íntegramente el resto del mapa |

No se identificó un defecto crítico demostrado. Los tests añadidos reprodujeron
los fallos antes de corregir producción; los logs de regresión se entregan.

## Decisiones de arquitectura

- Se mantuvieron `Diagnostics` y sus listas. Una jerarquía nueva de diagnósticos
  no era necesaria para añadir ruta, ubicación y expectativas verificables.
  El formateo conoce la ruta; las decisiones semánticas no la incorporan a cada
  cadena. `ParseError` conserva su categoría estructurada.
- Las posiciones de valores se mantienen en el evento. Los errores de reglas y
  atajos señalan su cabecera, como antes. Los errores que relacionan varios
  valores generales señalan la cabecera real de `[general]`, no una columna
  calculada por aproximación. No se inventan spans, códigos ni sugerencias.
- El análisis de acordes usa un iterador inverso en lugar de un `Vec` temporal.
  Se aprovecha el recorrido para validar todos los componentes. No se atribuye
  una mejora de rendimiento medida a este cambio.
- Se conservan prioridades documentadas, por ejemplo atajos del usuario sobre
  los generados y políticas de fullscreen. Etiquetas de View duplicadas no se
  prohíben: son nombres, no identificadores, y no había un defecto demostrado.
- No se convirtió el subconjunto en un validador TOML completo ni se dividió
  `userconfig.rs` por tamaño. La cobertura aleatoria no demuestra corrección de
  toda la especificación TOML; prueba totality y determinismo sobre texto UTF-8.

## Compatibilidad

### Cambios incompatibles que ya estaban en la rama base

- `view_count`, `view_names`, `auto_view_binds` y `[[rules]].view` sustituyen
  nombres anteriores de tags/workspaces en TOML.
- La rama original retiraba aliases de claves generales, colores, autostart y
  reglas. Al consolidarla con la iniciativa de ingeniería se conservan los
  aliases de semántica exacta para evitar regresiones de configuración;
  configuraciones antiguas sin equivalente exacto y presets de tema retirados
  siguen produciendo diagnósticos de migración.
- Gaps y bordes superiores al límite de seguridad de layout ya se rechazaban.
  Tamaños, posiciones y opacidad de reglas ya tenían validaciones más estrictas.
- El ejemplo dejó de activar comandos de portal propios de una distribución y
  de anunciar el selector de layout sin efecto.

### Comportamiento de esta entrega

Se preserva el comportamiento de las configuraciones válidas del vocabulario
actual. Se rechazan construcciones malformadas antes aceptadas accidentalmente
(hex con signo, saltos literales en strings, filas sin apertura, acordes vacíos y
NoSymbol), valores `f64` fuera de rango aunque redondeen a un `f32` válido y la
negación no representable. Son correcciones de entradas inválidas; no se añade
una sintaxis, alias ni ruptura deliberada de configuración soportada.

| Situación | Política preservada |
|---|---|
| Startup sin archivo | Baseline compilado, sin terminar la sesión por configuración |
| Startup con sintaxis inválida | Descartar el archivo y usar baseline con diagnóstico |
| Valor semántico inválido | Descartar ese campo/entrada y aplicar el resto según el contrato |
| Reload sin archivo, ilegible o con sintaxis inválida | Conservar configuración activa; no reconciliar primero el estado |
| Reload con errores semánticos | Aplicar el overlay con valores inválidos descartados; no convertirlo en rechazo transaccional total |
| `--check-config` | 0 sin diagnósticos; 1 ante advertencias o errores; no ejecutar comandos ni abrir backend |
| Checker con ruta explícita inexistente | 0 con fallback limpio, conforme al contrato existente |
| Tabla desconocida no relacionada | Ignorar por compatibilidad futura |
| Autostart `[]` | Lista vacía válida que reemplaza el baseline |
| Autostart con tipo inválido | Se corrige para conservar el baseline, en coherencia con el descarte del valor |

Versiones de workspace/manifests/lock y `--version`: 1.1.1. Se actualiza
`[Unreleased]`; no se inventa una versión ni se publica una release.

## Validación

Entorno: contenedor Linux x86-64, Rust/Cargo/Clippy/rustfmt 1.85.1, bibliotecas
X11 de Ubuntu obtenidas en un sysroot temporal. `RUSTFLAGS`, `LIBRARY_PATH`,
`LD_LIBRARY_PATH` y `CPATH` apuntan a ese sysroot para construir y compilar probes;
no son cambios del repositorio ni dependencias nuevas del proyecto.

| Comando | Resultado | Pruebas / advertencias / límites |
|---|---|---|
| `cargo fmt --all` y `cargo fmt --all -- --check` | PASS | Formato normalizado; sin cambios finales de formato pendientes |
| `cargo check --workspace --all-targets --locked` | PASS | Todos los targets compilan, sin advertencias de rustc |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | FAIL preexistente | 6 diagnósticos fuera de los archivos de configuración; véase inventario inferior |
| `cargo clippy -p maverick-toml --all-targets --locked -- -D warnings` | PASS | Sin advertencias del parser ni sus tests |
| `cargo test --workspace --locked --no-fail-fast` | FAIL por entorno | 926 PASS, 63 FAIL, 0 ignored declarados; cada fallo ya figuraba en la referencia |
| `cargo test --workspace --all-features --locked --no-fail-fast` | FAIL por entorno | 926 PASS, 63 FAIL; mismo conjunto de fallos que default |
| `cargo test -p maverick --bin maverick --locked userconfig::tests` | PASS | 49 pruebas de configuración |
| `cargo test -p maverick --bin maverick --locked core::action::tests` | PASS | 15 pruebas del parser compartido de acciones |
| `PROPTEST_RNG_SEED=1794000000 cargo test -p maverick-toml --locked` | PASS | 31 unitarias y 10 propiedades, incluidos texto arbitrario y límites existentes |
| `cargo test --test config_check --locked` | PASS | 2 tests de proceso: 12 casos de configuración más ruta inexistente, y ausencia de ejecución de comandos |
| `cargo build --release --locked` | PASS | Binario de producción generado |
| `target/release/maverick --check-config config/config.toml` | PASS | Ejemplo limpio en release |
| `target/release/maverick --version` | PASS | `maverick 1.1.1` |
| `python3 -m py_compile tests/xvfb-reload.py` | PASS | Sólo sintaxis del harness ampliado |
| `python3 tests/xvfb-reload.py` | BLOQUEADO | Xvfb no puede crear sockets y termina antes de iniciar Maverick |

`--no-fail-fast` permite observar todos los targets; no oculta ni omite pruebas.
Las features son trazas `input-trace` y `window-trace`, compatibles entre sí.
Algunas pruebas X11 existentes retornan sin display y el harness las cuenta
como aprobadas; estos conteos **no acreditan una sesión gráfica**.

Los 63 fallos incluyen `socket(AF_UNIX)` con `EPERM`, imposibilidad de localizar
el PID real mediante `/proc/<pid>` y pruebas de identidad/control de procesos
que dependen de ello. Se comprobó separadamente la denegación de crear un socket
Unix. No se cambiaron esas pruebas, no se añadieron skips ni supresiones. La
prueba Xvfb se intentó realmente; los logs adjuntos incluyen un extracto de su
fallo, evitando millones de líneas repetidas del servidor.

En una repetición individual posterior, Cargo no pudo lanzar el binario desde
los dos tests del checker (`PermissionDenied`, `EACCES`). No se rebajaron las
aserciones: el mismo ejecutable de tests invocado directamente aprobó ambos,
y ambos habían aprobado en las dos ejecuciones completas. Se adjuntan también
ese fallo y la ejecución directa; la restricción de procesos del entorno hace
que el resultado de ese relanzamiento individual no sea un gate estable aquí.

Inventario de Clippy preexistente observado:

| Archivo | Diagnóstico |
|---|---|
| `maverick-core/src/types.rs:847` | `comparison_chain` |
| `maverick-sys/src/control.rs:1182` | `search_is_some`, helper de tests |
| `maverickctl/src/session/lifecycle.rs:515` | `nonminimal_bool` |
| `maverickctl/src/session/xserver.rs:496` | `format_collect` |
| `maverickctl/src/session/mod.rs:1623,1640` | Dos `format_collect` en tests |
| `src/backend/x11/overview.rs:303,412` | Dos `map_entry`, observados al comprobar el binario más allá de los bloqueos de dependencias |

Las advertencias de Overview exigirían modificar el renderer expresamente
excluido de la fase. Se mantiene ese límite y no se usan `allow`, `expect` ni
flags que oculten lints. El pequeño cambio de `actions.rs` sólo pasa la ruta al
formateador de diagnósticos; no altera el algoritmo de recarga.

## Rendimiento y siguientes pasos

No se ejecutaron benchmarks de tiempo ni de memoria. Se conservan los valores
prestados del parser; no se añade un AST. No se declara que Maverick tenga más
FPS, menos RSS o una mejora porcentual. Quedan fuera las optimizaciones de
geometría y render.

Para cerrar la aceptación: ejecutar la matriz completa y `tests/xvfb-reload.py`
en Linux sin las restricciones de sockets/proc de este contenedor, y resolver
las advertencias globales en una fase que autorice tocar esos componentes.
Después conviene probar startup/reload en una sesión X11 aislada real, sin
confundir Xvfb con validación de todas las configuraciones de hardware.

## Archivos, commits y entrega

- `maverick-toml/src/lib.rs`: rechazos sintácticos y regresiones.
- `maverick-toml/tests/parser_props.rs`: totality y determinismo con UTF-8 arbitrario.
- `src/core/action.rs`: negación comprobada y regresión.
- `src/userconfig.rs`: validación, ubicaciones, tipos esperados y pruebas.
- `src/main.rs`, `src/backend/x11/actions.rs`: ruta para presentar diagnósticos.
- `tests/config_check.rs`: contrato externo del checker sin display.
- `tests/xvfb-reload.py`: expectativas correctas y fuentes de reload rechazadas.
- `CHANGELOG.md` y este informe: efectos y límites documentados.

Commits funcionales locales (autor Azytar, identidad del historial existente):

1. `65772b2` — `fix(toml): reject sign loss and malformed string grids`.
2. `2f121e5` — `fix(config): check overflow in legacy shrink actions`.
3. `3e48986` — `fix(config): validate source values and preserve diagnostic context`.
4. `22af272` — `test(config): cover arbitrary text and rejected reload sources`.
5. El commit final de documentación contiene este informe y el changelog;
   su SHA exacto figura en `COMMITS.txt` de la entrega.

La entrega incluye el árbol actualizado, parche completo desde `74fd433`, un
bundle con la rama y su historia y evidencia de validación. No incluye `target/`.
El bundle permite recuperar los commits sin depender de un push remoto.
