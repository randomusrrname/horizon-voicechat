# Arma el `.hnmod` de voicechat sin duplicar el código.
#
# El registro horizon-mods espera native/<id>/compatibility.json y el bundle en
# native/<id>/mixins/<id>/. Este script construye ese árbol en una carpeta temporal, copia
# este proyecto como bundle (horizon-mixin.toml + la crate de [api] + LICENSE si existe) y
# llama al `native.py` del registro, que solo escribe el archivo: no compila ni ejecuta nada.
#
#   powershell -ExecutionPolicy Bypass -File hnmod/package.ps1
#   powershell -ExecutionPolicy Bypass -File hnmod/package.ps1 -Work D:\tmp\voicechat-hnmod
#
# Resultado: <Work>/horizon-mods/native/target/voicechat-<version>.hnmod
param(
    [string]$Work = (Join-Path $env:TEMP "horizon-voicechat-hnmod"),
    [string]$Tool = "https://raw.githubusercontent.com/horizon-rust/horizon-mods/main/native/tools/native.py"
)

$ErrorActionPreference = "Stop"
$repo = Split-Path $PSScriptRoot -Parent
$tree = Join-Path $Work "horizon-mods"
$native = Join-Path $tree "native"
$mod = Join-Path $native "voicechat"
$bundle = Join-Path $mod "mixins\voicechat"

Write-Host "Preparing $tree"
Remove-Item $tree -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path (Join-Path $native "tools"), $bundle | Out-Null

Write-Host "Fetching native.py"
Invoke-WebRequest -Uri $Tool -OutFile (Join-Path $native "tools\native.py")

Write-Host "Copying the bundle"
Copy-Item (Join-Path $repo "hnmod\compatibility.json") (Join-Path $mod "compatibility.json")
Copy-Item (Join-Path $repo "horizon-mixin.toml") (Join-Path $bundle "horizon-mixin.toml")
Copy-Item (Join-Path $repo "api") $bundle -Recurse
$license = Join-Path $repo "LICENSE"
if (Test-Path $license) {
    Copy-Item $license (Join-Path $mod "LICENSE")
} else {
    Write-Host "No LICENSE in the project yet: the .hnmod will not carry one (a release needs it)."
}

Write-Host "Packaging"
python (Join-Path $native "tools\native.py") package voicechat
