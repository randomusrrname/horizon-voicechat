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
(`native/voicechat_client/compatibility.json` + `native/voicechat_client/mixins/voicechat_client/`)
copiando este proyecto, descarga el `native.py` oficial y ejecuta `package voicechat_client`. No
compila ni ejecuta el mod: solo escribe el zip. La salida es
`<temporal>/horizon-mods/native/target/voicechat_client-<version>.hnmod`.

Para usar otra carpeta:

```
powershell -ExecutionPolicy Bypass -File hnmod/package.ps1 -Work D:\tmp\voicechat-hnmod
```

## Qué lleva dentro

- `horizon-native.json`: `game: "26.3"`, `hooks: 411`, `horizon: 2ebf00a…`, `mixins: e8cd0aa…`,
  `id: "voicechat_client"`, `version: "0.1.0"`, `schema: 1`.
- `mixins/voicechat_client/horizon-mixin.toml` y `mixins/voicechat_client/api/` (todo el código,
  incluido `ui.rs`), más el `LICENSE` MIT en la raíz del mod.
- `native/voicechat_client/compatibility.json` no va dentro del zip; lo lee el registro. Es una
  copia de `hnmod/compatibility.json` con los mismos datos que el `horizon-native.json`.

`hnmod/registry-entry.toml` es un borrador de la ficha del registro (`mods/voicechat_client.toml`)
con el nombre, la descripción honesta, la licencia, el repositorio de origen y el hash del paquete.

## Antes de publicarlo

- **Licencia MIT** en `LICENSE` (titular `randomusrrname`); `package.ps1` la copia al mod, así que
  el `.hnmod` la lleva dentro.
- El id `voicechat` ya está ocupado en el registro por el mod de Nirokx; esta entrada usa
  `voicechat_client` ("Simple Voice Chat Client") para no chocar.
- Publicar requiere que el launcher y `cargo-horizon-mod` del lado de Horizon acepten mods nativos
  del registro.
- El mod no se probó contra un servidor real de Simple Voice Chat: el handshake y el audio siguen
  sin verificar (ver `RESUMEN.md`).
- `native.py check`/`build` usan `rsync` y bloqueos de fichero de Linux; en Windows solo se validó
  `package`.
