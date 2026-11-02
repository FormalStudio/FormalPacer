#!/bin/sh
# Runs fakegame at each rate given (default 200 100 60) under `formalpacer --headless`, with the
# layer from this checkout registered for these runs only. Pass FAKEGAME_FLAGS=--fullscreen to
# get VRR from a compositor that turns it on for fullscreen windows. Logs go to target/pacing/.
set -eu
cd "$(dirname "$0")/.."
cargo build -q --release -p formalpacer -p formalpacer-layer -p fakegame
out=target/pacing
mkdir -p "$out/layer"
sed "s|@LIBRARY_PATH@|$PWD/target/release/libformalpacer_layer.so|" packaging/linux/formalpacer-layer.json.in > "$out/layer/formalpacer.json"
export VK_ADD_IMPLICIT_LAYER_PATH="$PWD/$out/layer" XDG_CONFIG_HOME="$PWD/$out/config" DISABLE_MANGOHUD=1
[ $# -gt 0 ] || set -- 200 100 60
target/release/formalpacer --headless 2> "$out/engine.log" &
engine=$!
trap 'kill $engine 2>/dev/null' EXIT
for fps in "$@"; do
  FORMALPACER=1 target/release/fakegame ${FAKEGAME_FLAGS:-} --fps "$fps" --seconds 10 2> "$out/game-$fps.log"
  echo "fakegame --fps $fps: $(tail -1 "$out/game-$fps.log")"
  echo "  engine: $(grep -F 'fakegame on' "$out/engine.log" | tail -1 | sed 's/.*: cap /cap /')"
done
