# Plugin de multimedia

Referencia de la integración oficial para desarrolladores y agentes. Controla un
reproductor (play, pause, play/pause, stop, previous, next) y muestra lo que reproduce:
la carátula si está disponible o, en su defecto, el estado. Se comunica mediante
[MPRIS](https://specifications.freedesktop.org/mpris/latest/Player_Interface.html)
sobre el D-Bus de sesión. ID `org.rad.media`, versión 0.3.0, Linux x86_64, contrato de
plugins v1. Las capacidades negociadas están en el
[contrato y la guía para autores](../README.md).

## Requisitos

- Un bus de sesión y reproductores que implementen MPRIS (su nombre de bus empieza
  por `org.mpris.MediaPlayer2.`). Los navegadores y la mayoría de los reproductores
  de escritorio lo implementan. El bus se obtiene de `DBUS_SESSION_BUS_ADDRESS`;
  si no está definida, se usa el socket estándar `$XDG_RUNTIME_DIR/bus` si existe.
  El plugin nunca inicia un bus.
- GLib y GIO (`libglib-2.0`, `libgio-2.0`); el plugin usa GIO para D-Bus.

El plugin no requiere nada más del entorno. Si no hay bus de sesión, las acciones
fallan con `unavailable` y el estado aparece no disponible hasta que vuelva el bus.

## Acciones y estados

| Acción | Operaciones | Reproductor |
| --- | --- | --- |
| Control multimedia | play/pause, play, pause, stop, previous, next | una aplicación |

| Estado | Muestra |
| --- | --- |
| Reproducción actual | carátula; sin ella, `Playing` (verde), `Paused` (ámbar) o `Stopped` (gris) |

El **reproductor** es una aplicación, no uno de sus procesos: corresponde a la
parte del nombre de bus posterior a `org.mpris.MediaPlayer2.`, quitando el sufijo
`.instance<number>`. Los navegadores agregan ese sufijo: `...brave.instance4242`
es `brave` y sigue siéndolo después de reiniciar el navegador. Las opciones son
las aplicaciones que están ejecutándose, más Brave y Strawberry marcadas como
*(not running)* cuando no lo están, para configurar un botón antes de iniciarlas.

- Un reproductor **sin ejecutar** nunca se sustituye por otro: la acción falla con
  `unavailable` sin tocar nada más, y el estado aparece no disponible.
- Con **varias instancias** de una aplicación se usa la primera encontrada: la de
  nombre de bus anterior en orden textual (`brave.instance10` va antes que
  `brave.instance9`). No hay selector ni error por este caso.
- A un reproductor que declara no poder realizar una operación (`CanPlay`,
  `CanPause`, `CanGoNext`, `CanGoPrevious`; `CanControl` para stop) no se le pide:
  la acción falla con `incompatible`.
- Si el reproductor no responde en 3 segundos, la acción falla con `timed_out`;
  también si el bus acepta la conexión pero no responde. Los 3 segundos cubren
  toda la acción, incluida la conexión. Las otras acciones no quedan bloqueadas.
- Si el reproductor sale durante la acción, esta falla con `unavailable`.
- Consultar un reproductor nunca lo inicia: si no estaba ejecutándose, sigue así.

El estado sigue al reproductor: el plugin escucha el bus (inicio o salida de un
reproductor, o anuncio de un cambio) y vuelve a leerlo. Además, lee cada reproductor
mostrado una vez cada 10 segundos, para que uno bloqueado que ya no pueda anunciar
cambios aparezca como stale en vez de conservar como actual lo último que informó.
Cada observación completada renueva la vigencia por 25 segundos; si el contenido no
cambió, envía una renovación liviana, nunca repite la carátula. Un lector bloqueado
no puede renovar el estado almacenado. No se sondea nada más. Sin estados mostrados,
el plugin no mantiene ningún hilo de escucha (un segundo después de desaparecer el
último).

## Recursos

El contrato actual permite 32 suscripciones por plugin y multimedia limita sus
hilos lectores a 32. La demanda se comparte por fuente y se libera con el último
consumidor.

La memoria y los hilos crecen con las aplicaciones mostradas, no con los botones:
cada aplicación mostrada tiene un hilo lector y todas comparten un monitor del bus.
La [referencia de rendimiento](https://github.com/KroxiDev/RAD/blob/98fb7ce54a65cf38e57a697b4edbddf598d6319e/docs/performance.md#evidencia-y-límites)
delimita las mediciones disponibles y su alcance. Los valores de compilaciones
anteriores no son presupuestos vigentes ni prueban la cuota máxima actual.

## Límites conocidos

- Una aplicación es un nombre de bus sin el sufijo `.instance<digits>`. Un
  reproductor que numera instancias de otra forma, como Firefox con
  `...firefox.instance_1_82`, cuenta como una aplicación por instancia.
- La carátula se lee cuando el reproductor la anuncia. Si la anuncia antes de
  terminar de escribirla y no anuncia nada después, se sigue mostrando el estado
  hasta el anuncio siguiente.
- Cualquier anuncio de cualquier reproductor provoca una nueva lectura de todas
  las aplicaciones mostradas; un reproductor muy activo hace que se lean los
  demás algo más a menudo.

## Carátulas

La carátula es el `mpris:artUrl` ofrecido por el reproductor cuando es una URL
`file://`. No se descarga nada: se ignoran `http(s)` y otras URL y el estado se
muestra como texto. Solo se lee un archivo regular de hasta 8 MiB, y solo se
decodifica si es PNG o JPEG de hasta 2048 x 2048 píxeles (el tipo se determina por
su contenido, no por su nombre). Se escala para caber en 96 x 96 sin deformarlo,
sobre fondo negro, y se publica como JPEG de hasta 8 KiB.

## Compilar y empaquetar

Desde el directorio `plugins`, con las dependencias ya disponibles. El ejemplo
escribe un paquete local, no una publicación; para la caché offline y el gate
completo, seguir [Pruebas y validación](https://github.com/KroxiDev/RAD/blob/98fb7ce54a65cf38e57a697b4edbddf598d6319e/docs/testing.md).

```sh
cargo build --release -p rad-plugin-media
cargo run --release -p rad-plugin-sdk --bin radp-pack -- \
    media/manifest.json target/release/rad-plugin-media org.rad.media.radp
```

## Pruebas

`cargo test -p rad-plugin-media` no toca reproductores reales ni el bus de sesión.
Inicia un `dbus-daemon` privado (debe estar instalado, al igual que `touch`) con una
configuración mínima propia, y reproductores falsos (`examples/fake_player.rs`)
sobre ese bus. Los únicos servicios que el bus puede iniciar son los escritos por
una prueba para demostrar que el plugin nunca inicia un reproductor. El falso
registra las llamadas, responde propiedades MPRIS, emite `PropertiesChanged` y
permite dejar de responder o salir en un momento dado. Antes de ejecutar un único
target (`cargo test --test actions`), compilar el helper con
`cargo build --examples`. Para el gate completo, seguir
[Pruebas y validación](https://github.com/KroxiDev/RAD/blob/98fb7ce54a65cf38e57a697b4edbddf598d6319e/docs/testing.md).

**No ejecutar `fake_player` contra el entorno personal.** Registra un reproductor
MPRIS falso en el bus de `DBUS_SESSION_BUS_ADDRESS`, lo que introduciría un
reproductor inexistente en la sesión real. Por eso rechaza el arranque salvo que
`FAKE_PLAYER_PRIVATE_BUS=1`; los tests la definen y solo debe usarse junto a la
dirección de un bus privado creado para la prueba.
