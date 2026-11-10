# Resumen de la sesión (2026-11-10)

## Hecho
- Leí ambos README y el código (`lib.rs`, `util.rs`, `audio.rs`).
- Resolución DNS fuera del hilo del juego (`api/src/lib.rs`): `connect()` lanza el hilo `voicechat-dns`; `poll_resolving()` (llamado en cada tick) abre el `UdpClient` cuando llega la dirección. Tests ajustados a la resolución asíncrona.
- README actualizado (quitado de "Not done yet").

## No verificado
- No hay `cargo` ni red en este entorno: nada compilado ni testeado. Primero correr `cargo test` en `api/` y `horizon-voicechat-net`.

## Pendiente (en orden sugerido)
1. Compilar contra Horizon real y corregir nombres de hooks / `rusty-opus` / `cpal` (lista en README).
2. Hilo propio para el socket UDP y el envío del micrófono (hoy sale en ráfagas cada 50 ms).
3. Indicador en pantalla, teclas y volumen configurables.
4. Audio posicional (necesita posiciones de entidades y mezcla estéreo).
5. Menú de grupos (falta el payload de unirse con contraseña), susurros y ensordecer (faltan capturas).
6. Elegir licencia y coordinar con issue #7759 antes de abrir PR.
