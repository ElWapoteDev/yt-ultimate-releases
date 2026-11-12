$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'Rust no está instalado. Instálalo desde https://rustup.rs con el toolchain MSVC.'
}
& cargo run --locked -p yt-app --bin yt-ultimate -- @args
exit $LASTEXITCODE
