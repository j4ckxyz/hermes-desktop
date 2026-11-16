#!/bin/sh
# Build dependencies for GPUI (X11 + Wayland), fonts and the Secret Service on Debian/Ubuntu.
set -eu
apt-get update
apt-get install -y --no-install-recommends \
  build-essential pkg-config cmake clang \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev \
  libx11-dev libx11-xcb-dev libxcb1-dev libxcb-xkb-dev libxi-dev \
  libfontconfig-dev libfreetype-dev libdbus-1-dev libssl-dev libzstd-dev
