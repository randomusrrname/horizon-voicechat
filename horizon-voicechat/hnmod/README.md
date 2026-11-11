# `.hnmod`: el paquete del registro

Este mod es un **trait mod** (`[api] path = "api"`): el launcher lo compila dentro del juego. El
registro [horizon-mods](https://github.com/horizon-rust/horizon-mods) publica ese mismo proyecto
empaquetado como un archivo `.hnmod` (un zip con el bundle y un `horizon-native.json` que fija
Horizon, hook API, mixins y juego).

## Crear el archivo

```
powershell -ExecutionPolicy Bypass -File hnmod/package.ps1
```

El script arma en una carpeta temporal el árbol que espera el registro
(`native/voicechat/compatibility.json` + `native/voicechat/mixins/voicechat/`) copiando este
proyecto, descarga el `native.py` oficial y ejecuta `package voicechat`. No compila ni ejecuta el
mod: solo escribe el zip. La salida es
`<temporal>/horizon-mods/native/target/voicechat-<version>.hnmod`.

Para usar otra carpeta:

```
powershell -ExecutionPolicy Bypass -File hnmod/package.ps1 -Work D:\tmp\voicechat-hnmod
```

## Qué lleva dentro

- `horizon-native.json`: `game: "26.3"`, `hooks: 411`, `horizon: 2ebf00a…`, `mixins: e8cd0aa…`,
  `id: "voicechat"`, `version: "0.1.0"`, `schema: 1`.
- `mixins/voicechat/horizon-mixin.toml` y `mixins/voicechat/api/` (todo el código, incluido `ui.rs`).
- `native/voicechat/compatibility.json` no va dentro del zip; lo lee el registro. Es una copia de
  `hnmod/compatibility.json` con los mismos datos que el `horizon-native.json`.

## Antes de publicarlo

- **Falta una licencia.** `package.ps1` incluye `LICENSE` solo si existe en la raíz del proyecto;
  hoy no hay ninguna. Horizon es MIT y el registro revisa el contenido, así que hay que elegir una.
- Publicar requiere que el launcher y `cargo-horizon-mod` del lado de Horizon acepten mods nativos
  del registro; hasta entonces el `.hnmod` sirve para revisión, no para jugar.
- El mod no se probó contra un servidor real de Simple Voice Chat: el handshake y el audio siguen
  sin verificar (ver `RESUMEN.md`).
- `native.py check`/`build` usan `rsync` y bloqueos de fichero de Linux; en Windows solo se validó
  `package`.
