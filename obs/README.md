# Plugin de OBS para RAD

Referencia de la integración oficial para desarrolladores y agentes. Plugin
independiente `org.rad.obs` (`rad-plugin-obs`), versión 0.4.0, contrato de plugins v1
con capacidades negociadas de credenciales, ajustes, opciones, textos traducidos y
plazo de respuesta ([ADR 0020](https://github.com/KroxiDev/RAD/blob/eb86e1cc596dd63e07a38d89e03011284682b49f/docs/adr/0020-textos-de-plugins-y-plazo-fijo.md)).
Consultar el [contrato y la guía para autores](../README.md).

Se conecta únicamente a **127.0.0.1** mediante obs-websocket 5.x, RPC 1 y WebSocket
JSON/texto. El ajuste opcional del plugin `port` es un entero de 1 a 65535
(predeterminado **4455**); debe coincidir con el puerto de OBS. Viaja en las copias
del deck. Cada operación captura su puerto: cambiarlo no traslada trabajos ni
consultas activos a otra conexión. Los visuales se vuelven a suscribir con los
ajustes nuevos. No admite hosts remotos, TLS, control de Studio/preview, creación
de objetos ni varias instancias de OBS.

El slot opcional de credenciales `password` contiene la contraseña del WebSocket
de OBS. Una conexión que exige autenticación y no tiene contraseña queda no
disponible; un servidor sin desafío de autenticación no recibe la contraseña.
Las credenciales se aíslan por revisión: guardar otra contraseña no vuelve a
autenticar trabajos antiguos.

## Controles y visuales

Cada acción tiene un visual con el mismo ID que muestra su estado. Las
operaciones se muestran con su texto («Alternar», «Iniciar»…) y la primera es la
predeterminada.

| Acción (`id`) | Campos | Operaciones | Texto visual |
|---|---|---|---|
| Escena (`scene`) | escena | Pasa la escena al programa | Program / Inactive |
| Grabación (`record`) | — | `toggle`, `start`, `stop` | Recording / Paused / Stopped |
| Pausar grabación (`pause`) | — | `toggle`, `pause`, `resume`; requiere grabar | Paused / Not paused |
| Transmisión (`stream`) | — | `toggle`, `start`, `stop` | Streaming / Stopped |
| Silenciar audio de OBS (`mute`) | fuente | `toggle`, `mute`, `unmute` | Muted / On |
| Mostrar u ocultar fuente (`item`) | escena, fuente | `toggle`, `show`, `hide` | Visible / Hidden |
| Filtro (`filter`) | fuente, filtro | `toggle`, `on`, `off` | Enabled / Disabled |
| Cámara virtual (`virtualcam`) | — | `toggle`, `start`, `stop` | On / Off |
| Repetición (`replay`) | — | `save`, `toggle`, `start`, `stop` | On / Off |

Cada acción lee primero el estado de OBS y decide a partir de él: alternar
invierte lo leído; iniciar lo que ya está activo, o detener lo detenido, termina
con éxito sin enviar nada. El éxito se confirma leyendo el estado real de OBS
después del cambio; aceptar una orden no basta. «Guardar repetición» exige el
búfer activo y se confirma cuando OBS informa una repetición guardada distinta
de la anterior.

Las acciones declaran 20 segundos de plazo al host. Si OBS no acepta la conexión,
como mientras se está abriendo, cada acción lo espera hasta 15 segundos; después
falla con `unavailable`. Así «Abrir aplicación OBS» seguida de una acción de OBS
funciona en una sola tecla. Las acciones sobre un mismo OBS se ejecutan de a una
y en orden, de modo que cada una lee el estado que dejó la anterior. Una orden
que ya llegó a OBS nunca se repite tras reconectar: si la conexión se pierde
durante la acción, esta falla. La reconexión refresca observaciones. Los
visuales observan también los cambios hechos fuera de RAD. Una selección
desconectada, incompatible, ausente o desactualizada se declara no disponible en
vez de adivinarla.

Las opciones dinámicas enumeran fuentes y filtros, escenas, entradas y elementos
de escena existentes, incluidos los hijos de grupos. Se muestran por su nombre en
OBS; un hijo de grupo antepone el grupo y una fuente repetida en la misma escena
añade su número. La identidad del elemento incluye su
contenedor y su ID numérico, no solo el nombre de la fuente. Las selecciones
guardadas incluyen también colección y nombres: renombrar o reemplazar objetos
puede exigir seleccionarlos otra vez. No se redirigen selecciones entre
colecciones. Los frames/mensajes de OBS superiores a 64 KiB se rechazan sin
truncarlos (límite defensivo de compatibilidad independiente del framing del SDK).
Las listas de más de 64 opciones o las identidades/etiquetas de más de 256 bytes
UTF-8 quedan no disponibles, nunca truncadas.

Las peticiones se detienen mientras OBS cambia de colección de escenas. La
compatibilidad se comprueba contra las peticiones que OBS anuncia realmente;
los controles no soportados se rechazan sin intentar ejecutarlos. Esto no afirma
compatibilidad con todas las compilaciones ni distribuciones de OBS.

## Validación aislada

`cargo test --offline --locked --release -p rad-plugin-obs`, desde el workspace de
plugins, ejecuta contratos críticos con peers falsos. Los tests de red solo usan
endpoints loopback efímeros inyectados, nunca el puerto de producción 4455.
Los ejemplos `obs-fixture` y `obs-conformance-server`, habilitados con
`test-support`, son helpers para pruebas aisladas, no configuración pública del
endpoint del plugin.
`obs-fixture` admite un puerto de prueba como argumento; sin argumentos, lee un
archivo privado regular `0600` en `$XDG_RUNTIME_DIR/rad-obs-fixture.port`. Rechaza
el puerto 4455 y nunca recurre a producción como alternativa.
Compilar primero los helpers según [Pruebas y validación](https://github.com/KroxiDev/RAD/blob/eb86e1cc596dd63e07a38d89e03011284682b49f/docs/testing.md).

La [evidencia real disponible](https://github.com/KroxiDev/RAD/blob/eb86e1cc596dd63e07a38d89e03011284682b49f/docs/performance.md#evidencia-y-límites)
delimita las consultas OBS medidas mediante relay de solo lectura; no certifica
acciones, autenticación directa ni todas las versiones, compilaciones o
distribuciones de OBS.
`--licenses` imprime las licencias incorporadas.

## Caras por estado

Las acciones vinculan su visual automáticamente al guardarse en RAD. El plugin
publica el identificador del estado observado mediante la capacidad negociada
`visual_states`; no alterna un contador local de pulsaciones. El manifest incluye
dos etiquetas traducibles e iconos PNG por visual, con fuentes originales SVG en
`icons/`. Las imágenes propias y textos por estado se editan desde las pestañas de
la tecla. Por defecto se muestra solo el icono; sin fuente disponible se usa el
segundo estado con marca roja. Los iconos caben en el límite existente de 64 KiB
del manifest. El texto anterior sigue como respaldo para un host sin la capacidad.
