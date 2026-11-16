#!/bin/sh
# macOS: run a scripted demo against hermes-mock and capture ONLY the app window.
# Needs a debug build (script mode is compiled out of release builds), a release hermes-mock and a window-id helper (`winid PID` prints: id layer x y w h onscreen name).
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${OUT:-$ROOT/docs/screenshots}
WINID=${WINID:?set WINID to a helper that lists a pid's windows}
HOME_DIR=$(mktemp -d)
mkdir -p "$OUT"
"$ROOT/target/release/hermes-mock" --port 9139 --token demo --pace 30 & MOCK=$!
trap 'kill $MOCK ${APP:-} 2>/dev/null; rm -rf "$HOME_DIR"' EXIT
sleep 0.5
shot() { # $1 = name, $2 = env script. USER is neutral: the sidebar shows it as the display name.
  USER=you HERMES_DESKTOP_HOME=$HOME_DIR HERMES_DESKTOP_URL=http://127.0.0.1:9139 HERMES_DESKTOP_TOKEN=demo \
  HERMES_DESKTOP_SCRIPT="$2" "$ROOT/target/debug/hermes-desktop" & APP=$!
  sleep "${3:-4}"
  ID=$("$WINID" $APP | awk '$NF=="Hermes"{print $1; exit}')
  screencapture -x -o -l "$ID" "$OUT/$1.png"
  kill $APP; wait $APP 2>/dev/null || true
}
shot chat "wait:800;select:researcher" 3
shot streaming "wait:800;select:coder;type:search the docs then delegate in parallel;send" 3
shot mention "wait:800;select:default;type:ask @res" 3
shot files "wait:800;select:scribe;type:make a chart image;send" 5
