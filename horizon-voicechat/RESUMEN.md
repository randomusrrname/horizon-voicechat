# Resumen del mod Simple Voice Chat para Horizon

Fecha: 2026-11-11. Implementación limpia (clean-room) del protocolo de Simple Voice Chat
(compat 20) escrita a partir de capturas de tráfico; no se copió código de Simple Voice Chat.

## Hecho

### Paso 1 — compilar y tests
- Fijado Horizon `2ebf00a0281c592f79191e76fce0e7d522c6f425` en `horizon-mixin.toml`.
- `horizon-launcher native-build` compila el mod contra el API real de Horizon, con `ring`,
  `cpal` 0.16 y `rusty-opus` 1.0.2 de su `Cargo.lock`.
- El núcleo de red (`api/src/net`) pasa **30/30** tests con paquetes reales de la captura y `ring` real.
- El mod pasa **33/33** tests propios (indicador, menú de grupos, estado de susurro/ensordecer).
- Commit: `Step 1: build against real Horizon 2ebf00a and pass protocol capture tests`.

### Paso 2 — indicador en pantalla
- Panel abajo a la izquierda con `gui_render_post` (API 410): estado de conexión, micrófono
  (mudo/hablando), grupo propio, ensordecido, objetivo de susurro y quién habla.
- Se oculta con F1 (`gui_hidden`) y fuera de una partida.
- Los hablantes se marcan con los paquetes de voz y se limpian con el marcador de fin; si se pierde,
  caducan a los 2 s (`SPEAKING_TIMEOUT`).
- Commit: `Step 2: on-screen voice indicator with connection, mic and speaker state`.

### Paso 3 — menú de grupos
- Pantalla propia del mod (API 111): se abre con `G` (`mod_screen_open("voicechat")`), se dibuja en
  `screen_render_post` y toma clic, teclas y texto de los hooks de pantalla.
- Lista los grupos que anuncia el servidor (`add_group`), marca el grupo propio (`joined_group` y
  `state` del propio UUID) y resalta el grupo del jugador.
- **Crear** grupo con nombre (`create_group`, formato capturado) mediante Enter o el botón.
- Unirse y salir están **desactivados**: el clic explica que falta la captura de `set_group` /
  `leave_group`; no se envía ningún payload inventado.
- `remove_group`, `add_category`, `remove_category` y `remove_state` se ignoran con un aviso en el
  log (una vez por sesión), porque su formato no está verificado.
- Commit: `Step 3: group menu (list groups, show own group, create; join/leave disabled)`.

### Paso 4 — susurros y ensordecer
- Ensordecer con `N`: es **local**, omite la reproducción de los demás, pero la sesión UDP sigue
  viva y el micrófono no cambia. Se conserva al reconectar.
- Objetivo de susurro con `H`: recorre los jugadores listados por nombre y luego vuelve a ninguno.
- El HUD muestra la línea `Whisper` con el nombre; el menú, la pestaña Players.
- **No se envía nada de susurro ni de ensordecido**: sus formatos de red no están capturados.
  El micrófono sigue saliendo como voz normal de proximidad/grupo.

### Paso 5 — GUI estilo Plasmo Voice
- Módulo `api/src/ui.rs`: todo el dibujo en un solo sitio. Paneles oscuros translúcidos con borde y
  franja violeta (`0xFF8B5CF6`), cabecera, pestañas, filas con hover y selección. Solo rectángulos
  rellenos y la fuente del juego: sin texturas ni recursos copiados.
- HUD: puntos de estado, medidor de micrófono de ocho segmentos con el pico real de los frames
  (`frame_level`), grupo, ensordecido, susurro y hablantes con punto verde.
- Menú: cabecera con estado, pestañas **Groups**/**Players** con contadores, filas que reaccionan al
  puntero, formulario de crear grupo con cursor parpadeante y línea de avisos/pie.
- `Tab` cambia de página. En **Players** se puede hacer clic en un jugador para fijarlo como objetivo
  de susurro (local, no se envía). Escribir/Enter solo actúan en **Groups**.
- El estado se resuelve en `hud_view()`, `status()`, `mic_state()` y `menu_players()`; el dibujo no
  lee la sesión. `menu_layout()` lo usan tanto el dibujo como los clics, así no se desalinean.
- **37/37** tests propios contra el `horizon-client` real.
- Commit: `Step 5: Plasmo-style GUI (tabs, rows, live mic meter, player whisper pick)`.

## Cómo compilar y probar

```
horizon-launcher.exe native-build "C:\Users\Mauro\Documents\Horizon Voicechat\horizon-voicechat" ^
  --instance horizon --version nightly-2026-11-07-2ebf00a --trust-native-code
```

Última compilación verificada: `native-2ebf00a-a29fba3121ff`, seleccionada para la instancia
`horizon` (compilación incremental de 7 min 33 s). Commits: `cd7a480` (paso 1), `c951b37`
(paso 2), `c4609fc` (paso 3), `86af919` (paso 4).

El `--version nightly-2026-11-07-2ebf00a` es necesario en este entorno: sin él el launcher basa la
compilación en `c9d474d` en vez del commit fijado.

Teclas: `V` mantener para hablar, `M` mudo, `N` ensordecer, `H` objetivo de susurro, `G` menú de
grupos; dentro del menú, `Tab` cambia Groups/Players y clic en un jugador lo pone como objetivo de
susurro. El puerto UDP del servidor (24454 por defecto) debe ser alcanzable.

Tests:
- `cargo test` en `horizon-voicechat-net` (30 tests).
- Tests del mod: se ejecutan contra la copia preparada por el launcher en
  `%APPDATA%\horizon-launcher\native\stage\native-api\voicechat` (37 tests).

### Paquete `.hnmod`
- `hnmod/compatibility.json` (juego 26.3, hooks 411, Horizon `2ebf00a…`, mixins `e8cd0aa…`) y
  `hnmod/package.ps1`, que arma el árbol del registro en una carpeta temporal y llama a su
  `native.py package` (solo escribe el zip: no compila ni ejecuta nada).
- Generado: `voicechat-0.1.0.hnmod`, 39 276 bytes, sha256
  `e890fccd58f8c863ad886a6d40ae36cb32d211658362ecaa580a905d15990a28`. Contiene
  `horizon-native.json`, `horizon-mixin.toml` y todo `api/src` (incluido `ui.rs`).
- Sin `LICENSE` en el proyecto, el zip no lleva licencia: hay que elegirla antes de publicar.

## No verificado

- **No se probó contra un servidor real de Simple Voice Chat**: el handshake completo (secreto,
  autenticación UDP, keep-alive) y el audio real siguen sin verificar de extremo a extremo.
- Los formatos de red de `set_group`, `leave_group`, `remove_group`, categorías, susurro y
  ensordecido no están capturados; por eso esas funciones no envían nada.
- El formulario de `create_group` **sin contraseña** es la lectura obvia del capturado con
  contraseña, no una observación directa.
- `voice_host` no vacío en el secreto, otras versiones de compat y el audio posicional no están
  verificados.
- `CUSTOM_PAYLOAD_ID = 22` es el id del `CustomPayload` serverbound en `2ebf00a`; `send_payload`
  comprueba que el paquete decodificado sea realmente un `CustomPayload`.

## Pendiente

1. Probar en un servidor real y, si se puede, capturar `set_group`, `leave_group`, susurro y
   ensordecido para habilitarlos con formatos verificados.
2. Audio posicional (posición de los hablantes + mezcla estéreo).
3. Hilo propio para el socket UDP y el micrófono (hoy los frames salen en ráfagas cada 50 ms).
4. Teclas y volumen configurables.
5. Elegir licencia (y añadirla a la raíz para que el `.hnmod` la lleve), coordinar con el issue
   #7759 de Horizon y pedir permiso/crédito al autor de Simple Voice Chat antes de publicar.
6. Publicar en el registro: hace falta que el launcher y `cargo-horizon-mod` de Horizon acepten
   mods nativos del registro, y revisar el `.hnmod` con el equipo de horizon-mods.
