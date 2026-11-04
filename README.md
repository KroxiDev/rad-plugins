# Plugins de RAD

Los plugins oficiales y el SDK viven en este workspace para que el host y el SDK
puedan cambiar de forma atómica. Ambos usan el mismo crate
[`rad-plugin-contract`](crates/rad-plugin-contract); el SDK no define por separado
los tipos del protocolo.

La documentación canónica para autores está en
**[KroxiDev/rad-plugins](https://github.com/KroxiDev/rad-plugins)**:

- [Contrato de plugins v1](https://github.com/KroxiDev/rad-plugins/blob/sdk-v0.3.0/docs/contract-v1.md)
- [Guía para autores](https://github.com/KroxiDev/rad-plugins/blob/sdk-v0.3.0/docs/author-guide.md)
- [Paquetes publicados](https://github.com/KroxiDev/rad-plugins/releases)

Para trabajar en este repositorio, consultar el [índice técnico](https://github.com/KroxiDev/RAD/blob/98fb7ce54a65cf38e57a697b4edbddf598d6319e/docs/README.md)
y la [validación completa](https://github.com/KroxiDev/RAD/blob/98fb7ce54a65cf38e57a697b4edbddf598d6319e/docs/testing.md). Esta guía describe el workspace de
extensiones, no define un segundo contrato de plugins.

El contrato v1 usa una versión mayor y capacidades negociadas, capturas inmutables
de ajustes, renovación de visuales desde la observación de la fuente y salida del
SDK no bloqueante y acotada. Las antiguas API lineales del host 2/3 no son el
contrato v1. Usar versiones compatibles del host y los plugins; instalar un RAD
nuevo no actualiza silenciosamente los paquetes instalados.
La lista de oficiales del editor
([official.rs](https://github.com/KroxiDev/RAD/blob/98fb7ce54a65cf38e57a697b4edbddf598d6319e/crates/rad-editor/src/official.rs)) indica la release vigente de
cada uno: publicar una release oficial requiere actualizar su entrada allí.
Los paquetes prerelease ya instalados permanecen como inventario de solo lectura
hasta actualizarlos: se conservan sus metadatos originales y su versión anterior,
pero su protocolo antiguo nunca se ejecuta como contrato v1. La administración de
paquetes sigue disponible.

| Directorio | Crate | Integración |
| --- | --- | --- |
| [`sdk`](sdk) | `rad-plugin-sdk` | Contrato compartido, handshake, despacho, salida acotada y `radp-pack`. |
| [`audio`](audio) | `rad-plugin-audio` | Micrófono y salida de audio mediante `pactl`. |
| [`media`](media) | `rad-plugin-media` | Reproducción MPRIS y carátulas mediante el D-Bus de sesión. |
| [`obs`](obs) | `rad-plugin-obs` | Acciones de OBS, opciones dinámicas y estado visual. |
| [`metrics`](metrics) | `rad-plugin-metrics` | Estado visual de CPU/RAM y GPU/VRAM NVIDIA. |

Los iconos PNG de estado de los plugins oficiales tienen un presupuesto máximo
de **4 KiB por icono** (8192 caracteres hexadecimales). El manifest completo sigue
limitado a **64 KiB**, incluidos todos sus iconos.

Los fixtures son instrumentos de prueba, nunca artefactos de release. Los tests
no certifican hardware real, todas las distribuciones ni la seguridad de un plugin
nativo arbitrario.

## Compilar, probar y empaquetar

Desde el directorio `plugins`, con dependencias y herramientas ya disponibles.
Estos comandos focalizados no sustituyen el gate completo de
[Pruebas y validación](https://github.com/KroxiDev/RAD/blob/98fb7ce54a65cf38e57a697b4edbddf598d6319e/docs/testing.md):

```sh
cargo build --locked --release --workspace --all-features --bins --examples
cargo test --locked --release --workspace --all-features
cargo clippy --locked --release --workspace --all-features --all-targets -- -D warnings
```

Los tests unitarios y de protocolo de audio usan un `pactl` falso. La prueba de
producción sin servidor ejecuta el `pactl` real dentro de un sandbox `bwrap`, con
HOME, runtime, temporales y espacios de nombres de dispositivos, red y PID
privados, y ejecutables y bibliotecas del sistema en solo lectura. No puede
alcanzar el servidor de audio personal. Si no puede establecer ese aislamiento,
la prueba falla sin continuar. `bwrap`, `pactl` y los namespaces Linux deben estar
disponibles de antemano; los tests no instalan dependencias.
Los tests de multimedia usan un `dbus-daemon` privado (debe estar instalado) con
reproductores MPRIS falsos, nunca el bus de sesión. Los de OBS usan credenciales
sintéticas y servidores loopback falsos en puertos efímeros, nunca una instancia
instalada de OBS ni su puerto predeterminado. Los de métricas usan archivos proc
controlados y una biblioteca NVML falsa, nunca sensores del sistema ni el driver
real. Su ejemplo `metrics-fixture` (feature `test-support`) ejecuta el mismo plugin
y SDK con lecturas fijas y el manifest oficial; admite `--licenses`.
`test-support` habilita constructores de fixtures y ejemplos; no cambia el endpoint
de producción de OBS. Antes de ejecutar un único target de tests
(`cargo test --test process`), compilar los ejemplos auxiliares.
Con una caché ya poblada, agregar `--offline` a estos comandos. Estos ejemplos no
autorizan descargas ni la instalación de herramientas por inferencia.

Un paquete de plugin (`.radp`) reúne el manifest y el ejecutable en un archivo.
El siguiente ejemplo escribe un paquete local; no lo publica:

```sh
cargo build --release -p rad-plugin-audio
cargo run --release -p rad-plugin-sdk --bin radp-pack -- \
    audio/manifest.json target/release/rad-plugin-audio org.rad.audio.radp
```

`radp-pack <manifest.json> <executable> <output.radp>` escribe el contenedor RADP
v1: `RADP`, la versión del formato, las longitudes del manifest y del ejecutable,
y después ambos bloques de bytes. El manifest debe ocupar de 1 a 65536 bytes y el
ejecutable de 1 byte a 64 MiB; cualquier otro tamaño se rechaza antes de leerlo.

## Licencias y distribución

El código del proyecto tiene [licencia MIT](LICENSE). Esto **no** cambia las
licencias de las dependencias: las [licencias de terceros](notices/README.md)
conservan sus términos y textos originales, incluidos los textos específicos de
build/target y de bibliotecas de Rust retenidos de forma conservadora. El árbol de
fuentes incluye esas copias. Todos los ejecutables de plugins incorporan los
textos retenidos y los imprimen cuando `--licenses` es su primer argumento; después
salen sin leer la entrada del host, conectarse a audio/D-Bus/OBS ni cargar NVML.
Así esos textos viajan dentro del ejecutable, incluso en el contenedor RADP de dos
bloques. Un fallo de salida devuelve un código distinto de cero. Mantener la lista
de inclusiones de `sdk/src/licenses.rs` sincronizada cuando cambien dependencias o
textos retenidos, y contrastarla con los archivos originales de dependencias y
toolchain.

GLib/GObject/GIO y el runtime de C son dependencias del sistema enlazadas
dinámicamente, no copias incluidas. `pactl` es un programa instalado por separado,
no incluido. Sus licencias siguen siendo aplicables; consultar los paquetes de la
distribución para obtener sus fuentes y licencias.
Esta documentación de fuentes es autosuficiente: usar el plugin no requiere
acceso a un repositorio privado de la aplicación. Un árbol de fuentes o paquete
preparado localmente no demuestra que se haya publicado un repositorio o release
públicos.

## Procedencia del código

Copia mecánica para distribución, no una segunda definición del protocolo.
[SOURCE.md](SOURCE.md) indica el commit canónico de RAD, los hashes de las fuentes
y los pocos ajustes de rutas que necesita este workspace independiente.
Fijar `sdk-v0.3.0`.
