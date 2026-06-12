#!/usr/bin/env bash
set -euo pipefail
TARGET="${TARGET:-x86_64-unknown-linux-gnu}"
case "$TARGET" in
  x86_64) ARCH=amd64 ;;
  aarch64) ARCH=arm64 ;;
  *) echo "Unknown arch for $TARGET"; exit 1 ;;
esac
echo "TODO: build .deb for $ARCH from dist/rustfox"
echo "See docs/superpowers/specs/2026-06-12-multi-platform-service-setup-design.md"
