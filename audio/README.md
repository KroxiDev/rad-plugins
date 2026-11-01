# Plugin de audio

Referencia de la integración oficial para desarrolladores y agentes. Silencia,
reactiva y alterna el **micrófono** y la **salida de audio**, y muestra si cada uno
está silenciado. ID `org.rad.audio`, versión 0.4.0, Linux x86_64, contrato de plugins
v1. Las capacidades negociadas están en el [contrato y la guía para autores](../README.md).

## Requisitos

- PipeWire con `pipewire-pulse`, o PulseAudio.
- `pactl` con salida JSON (`-f json`) en `/usr/bin`, `/bin` o `/usr/local/bin`.
  Probado con pactl 17 sobre el servidor PulseAudio de PipeWire.

El plugin se comunica con el servidor de sonido únicamente mediante `pactl`; no
requiere otras dependencias para esa comunicación. Si falta `pactl` o el servidor
no está activo, las acciones fallan con `unavailable` y el estado aparece no
disponible hasta que vuelva el servidor.

## Acciones y estados

| Acción | Operaciones | Dispositivo |
| --- | --- | --- |
| Salida de audio | `toggle` (predeterminada), `mute`, `unmute` | una salida (sink) |
| Micrófono | `toggle` (predeterminada), `mute`, `unmute` | una entrada (source), excluidos los monitores |

El editor muestra las operaciones como «Alternar», «Silenciar» y «Activar
sonido», con el resto de los textos traducidos en el manifest
([ADR 0020](https://github.com/KroxiDev/RAD/blob/eb86e1cc596dd63e07a38d89e03011284682b49f/docs/adr/0020-textos-de-plugins-y-plazo-fijo.md)). El
dispositivo se elige primero.

| Estado | Muestra |
| --- | --- |
| Estado de salida de audio | `On` (verde) o `Muted` (rojo) |
| Estado de micrófono | `On` (verde) o `Muted` (rojo) |

El **dispositivo** es *Default device (follows changes)* o un dispositivo concreto.

- *Default* actúa sobre el dispositivo predeterminado al ejecutar la acción; el
  estado sigue al predeterminado cuando cambia.
- Un dispositivo *concreto* debe existir al ejecutar la acción. Si no existe,
  falla con `unavailable` sin tocar otro dispositivo ni sustituirlo. Su estado
  aparece no disponible y vuelve a `On`/`Muted` cuando reaparece.

Los cambios de estado externos (una tecla de volumen, un mezclador) provocan una
nueva observación. Las fuentes sin cambios se vuelven a observar cada 10 segundos;
las observaciones renuevan la vigencia por 25 segundos sin repetir la carga de
imagen/estado. El SDK no tiene un heartbeat independiente. Mientras se muestra un
estado, el plugin sigue al servidor con un único proceso `pactl subscribe`; sin
estados mostrados no mantiene ningún proceso de fondo.

El micrófono *Default device* se resuelve por nombre antes de actuar. Si la entrada
predeterminada del servidor es un monitor (una grabación de lo que reproduce una
salida) en vez de un micrófono, la acción falla con `unavailable` y el estado
aparece no disponible.

Si no se puede alcanzar al servidor, el plugin reintenta tras 1 s, luego 2 s, 4 s y
así hasta 30 s; vuelve a empezar en 1 s cuando el servidor responde. Por eso puede
tardar hasta 30 s en detectar que volvió.

## Compilar y empaquetar

Desde el directorio `plugins`, con las dependencias ya disponibles. El ejemplo
escribe un paquete local, no una publicación; para la caché offline y el gate
completo, seguir [Pruebas y validación](https://github.com/KroxiDev/RAD/blob/eb86e1cc596dd63e07a38d89e03011284682b49f/docs/testing.md).

```sh
cargo build --release -p rad-plugin-audio
cargo run --release -p rad-plugin-sdk --bin radp-pack -- \
    audio/manifest.json target/release/rad-plugin-audio org.rad.audio.radp
```

## Pruebas

`cargo test -p rad-plugin-audio` no cambia el audio de la máquina. Ejecuta el plugin
contra un `pactl` falso (`examples/fake_pactl.rs`) que conserva su estado en un
directorio temporal y puede simular un servidor detenido, un comando bloqueado y
salida inválida. La prueba de producción sin servidor ejecuta el binario real y
pactl dentro de un namespace bwrap con runtime, HOME, dispositivos y red privados,
sin acceso al servidor de audio personal. Si no está disponible ese aislamiento,
la prueba falla sin continuar. Compilar primero los helpers según la
[validación del workspace](../README.md#compilar-probar-y-empaquetar).

## Caras por estado

Las acciones vinculan su visual automáticamente al guardarse en RAD. El plugin
publica el identificador del estado observado mediante la capacidad negociada
`visual_states`; no alterna un contador local de pulsaciones. El manifest incluye
dos etiquetas traducibles e iconos PNG por visual, con fuentes originales SVG en
`icons/`. Las imágenes propias y textos por estado se editan desde las pestañas de
la tecla. Por defecto se muestra solo el icono; sin fuente disponible se usa el
segundo estado con marca roja. Los iconos caben en el límite existente de 64 KiB
del manifest. El texto anterior sigue como respaldo para un host sin la capacidad.
