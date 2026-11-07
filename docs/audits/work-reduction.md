# Maverick: trabajo repetido y consistencia

Fecha: 2026-11-06. Rama de publicación: `config/toml-hardening`.
Esta fase continúa el árbol publicado en `3692c27`. Los cinco commits de la
fase anterior se publicaron mediante la conexión de GitHub: se conservaron
mensajes, orden y árboles; las fechas de creación y los SHA cambiaron respecto
de la entrega local. Autor y committer: Azytar, `stardust_ech0@proton.me`.
El árbol publicado se comparó con el árbol de aquella entrega. `main` no se tocó.

## Cambios y justificación

| Zona | Trabajo anterior | Cambio | Invariante conservada |
|---|---|---|---|
| Carga de configuración | Crear baseline completo con atajos numéricos, descartarlo y crear otro baseline al leer correctamente | Construir un único baseline; completar sus atajos sólo si hay fallback | Los atajos generados respetan `view_count` y `auto_view_binds`; los fallbacks conservan el mapa de emergencia |
| Sugerencias | Comparar cada byte de una clave con todos los candidatos y crear dos vectores por comparación | Descartar diferencias de longitud superiores al máximo aceptado de dos ediciones y reutilizar una fila de Levenshtein | Mismo resultado, umbral y ganador en empates; no se limita la longitud admitida de las claves |
| Atajos duplicados | Buscar si existe una combinación y volver a buscar su acción para el diagnóstico | Una búsqueda obtiene directamente la acción anterior | Gana la primera combinación y se conserva el mensaje de conflicto |
| Tipos de ventana | Convertir a minúsculas al validar y repetir la conversión al construir la regla | Reutilizar la cadena normalizada | La regla almacena el tipo canónico; un tipo inválido se muestra con la escritura original |
| Espera X11 | Crear y a veces hacer crecer un vector para uno o dos descriptores en cada espera | Array local de dos elementos y slice del tamaño utilizado | Mismos descriptores y orden, timeout, interrupciones y punto de espera |
| Adaptador de poll | Crear un vector de `BorrowedFd` y después otro de `PollFd` | Convertir cada préstamo directamente en `PollFd` | La API sigue aceptando slices de cualquier tamaño; no cambia la propiedad ni la vida útil de los descriptores |
| Recursos Overview | Buscar presencia, insertar cuando faltaba y volver a buscar la imagen | Usar `BTreeMap::entry` y el recurso obtenido directamente | Mismas llamadas X11 y mismo tratamiento de errores; no se modifica escala, geometría ni orden de pintado |
| Logger | Crear un `String` con `format!` antes de formatearlo otra vez al escribirlo | Pasar `format_args!` directamente | Misma salida, nivel de filtrado y escritura de stderr |
| Cookie Xauthority | Crear una cadena temporal por cada byte hexadecimal | Una cadena con capacidad para los 32 caracteres finales | Mismos 16 bytes aleatorios, formato y validación; no se reduce entropía |

La eliminación de buffers y cadenas se deduce de las estructuras y llamadas
retiradas; no es una medición de RSS ni de número de llamadas al allocator.
El adaptador de poll aún construye su vector final, porque su API no está
limitada a los dos descriptores del WM. No se introducen dependencias ni nuevas
operaciones `unsafe`: se conserva el mismo préstamo existente de RawFd, con
la misma justificación, y se elimina su almacenamiento intermedio.

## Inconsistencias corregidas

Nueve aserciones en pruebas del core/layout invocaban `assert_invariants`, una
API presente sólo en debug. `cargo test --release -p maverick --bin maverick`
fallaba al compilar incluso si se seleccionaba un único test de configuración.
Ahora llaman a `check_invariants().expect(...)`, disponible en ambos perfiles.
No se elimina ni se condiciona ninguna de esas comprobaciones.

Se resolvieron nueve diagnósticos preexistentes de Clippy: cadena de
comparaciones, búsqueda seguida de `is_some`, dos negaciones booleanas,
tres construcciones de texto con `format!` por elemento y dos accesos a mapas
con `contains_key` seguido de `insert`. Los ajustes restantes de tests son
equivalencias directas y conservan sus aserciones. No se añaden supresiones.

## Mediciones

Linux x86-64 en este contenedor, Rust 1.85.1, perfil release, siete lotes por
caso. La carga incluye lectura del archivo, parseo, validación, merge y
destrucción del resultado. Los fixtures se crean una vez fuera del intervalo.
La medición de poll usa dos sockets emparejados, sin datos y con timeout cero;
no representa el tiempo que una sesión real pasa esperando un evento.

| Caso | Iteraciones por lote | Antes, mediana | Después, mediana | Observación |
|---|---:|---:|---:|---|
| Configuración pequeña | 4 000 | 2 882 ns | 1 897 ns | 34,2 % menos en esta ejecución |
| Ejemplo del repositorio | 1 000 | 5 538 ns | 4 824 ns | 12,9 % menos en esta ejecución |
| Clave desconocida de 4 KiB | 32 | 621 730 ns | 5 381 ns | Aproximadamente 115,5 veces más rápida |
| Poll de dos descriptores | 50 000 | 116 ns | 108 ns | Los rangos se solapan; no se afirma una mejora temporal robusta |

El caso largo pasa de 614 667–638 187 ns a 5 348–6 562 ns. La longitud ya
demuestra que ningún candidato puede estar a dos ediciones: no hay razón para
calcular la matriz de distancia. No se crea un caché persistente ni se conserva
un resultado que pudiera quedar desactualizado.

Se hicieron mediciones secuenciales antes/después; no un ensayo A/B aleatorizado.
Son resultados de workloads concretos, no FPS, consumo de batería ni latencia
de entrada en el portátil. Las rutas de X11/Overview sólo tienen justificación
estructural y comprobaciones de compilación aquí; falta validación gráfica.

Datos y salida original de los workloads:
[`benchmarks/results/2026-11-06-work-reduction`](../../benchmarks/results/2026-11-06-work-reduction).
`results.json` conserva medianas, rangos, iteraciones, entorno y estados de
validación. Los logs incluyen Clippy con directorio de compilación nuevo.

### Reproducción

El primer commit de esta fase, `test(perf): add repeatable workloads and repair
release invariant checks`, contiene los workloads antes de optimizar producción.
Puede localizarse por su mensaje y ejecutarse en un worktree separado. Ejecutar
los mismos comandos allí y en el HEAD actual, con las mismas herramientas:

```bash
cargo test --release -p maverick --bin maverick config_load_workload --locked -- --ignored --nocapture
cargo test --release -p maverick-sys poll_workload --locked -- --ignored --nocapture
```

Los dos workloads se marcan como manuales para que tiempos y syscalls repetidos
no formen parte de cada ejecución ordinaria. Ambos se ejecutaron explícitamente
y aprobaron. No se usa `ignore` para apartar ninguna prueba existente.

## Validación y límites

| Comprobación | Resultado |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --all-targets --locked` | PASS |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | PASS, también con caché completamente nueva |
| `cargo test --workspace --locked --no-fail-fast` | 928 PASS, 63 FAIL, 2 workloads manuales; un target adicional (`config_check`) no se pudo ejecutar por EACCES |
| `cargo test --workspace --all-features --locked --no-fail-fast` | 930 PASS, 63 FAIL, 2 workloads manuales |
| `cargo test --release -p maverick --bin maverick --locked` | Compila; 573 PASS, 1 FAIL por socket de teardown, 1 workload manual |
| `cargo build --release --locked` | PASS |
| `cargo build --release -p maverick -p maverickctl --locked` | PASS |
| `python3 tests/xvfb-reload.py` | Bloqueado: Xvfb no puede abrir sockets; Maverick no llega a iniciarse |

Los 63 nombres de pruebas fallidas son un subconjunto de los ya observados en
la fase anterior. Incluyen EPERM para sockets y discrepancias de PID/identidad
en `/proc`. La ejecución default añade una denegación al ejecutar el harness
del checker; ese mismo target sí aprueba sus dos tests en all-features. Estos
resultados no constituyen una batería completa verde.

Las nuevas comprobaciones verifican: sugerencias contra el algoritmo anterior,
incluidos empates, umbrales y ediciones de cada clave general; normalización de
tipos y conservación del texto del diagnóstico; y espera observable con 0, 1,
2 y 5 descriptores. Las pruebas existentes de timeout e interrupciones siguen
ejecutándose. No se cambia la política de configuración ni de recarga.

## Rutas investigadas que se conservaron

- Los dos puntos de reconciliación y flush alrededor de la espera responden a
  eventos que pueden llegar entre ambos. Suprimir uno por parecer repetido
  puede dejar geometría o propiedades pendientes mientras el WM duerme.
- `publish_state` vuelve a serializar para detectar cambios. Introducir una
  revisión global exige identificar todos los escritores de estado, incluidas
  rutas del backend que no pasan por comandos. No se añade un caché sin esa
  garantía; sigue siendo un candidato a una auditoría propia.
- El ordenado de previews recalcula rangos de foco. Cambiar el algoritmo puede
  intercambiar CPU por memoria temporal; queda para una medición específica
  con fixtures y validación de stacking, antes de alterar ese recorrido.

Queda ejecutar Xvfb y la matriz completa en un Linux que permita IPC y exponga
identidades de proceso coherentes. La versión sigue siendo 1.1.1; el changelog
se mantiene en Unreleased. No se crean releases, tags, PR ni cambios en main.
