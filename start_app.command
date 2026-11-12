#!/bin/zsh
set -euo pipefail

project_dir="${0:A:h}"
cd "$project_dir"
if ! command -v cargo >/dev/null 2>&1; then
  print -u2 "Rust no está instalado. Instálalo desde https://rustup.rs y vuelve a abrir este archivo."
  exit 1
fi
exec cargo run --locked -p yt-app --bin yt-ultimate -- "$@"
