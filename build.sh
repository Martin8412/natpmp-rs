#!/bin/sh
# Cross-compile natpmp for all supported targets.
#
# Linux targets produce fully static musl binaries (no glibc dependency).
# Linux builds require `cross` and a running Docker daemon.
# macOS builds use `cargo` directly (no extra tooling needed).
# Windows builds require `cross` and produce .exe binaries.
# FreeBSD builds must be run natively on a FreeBSD host (no cross toolchain);
# the freebsd-* targets are available for manual use but excluded from the default.
#
# Usage:
#   ./build.sh                   # build all default targets
#   ./build.sh linux-arm64       # primary target: aarch64 Linux (Raspberry Pi etc.)
#   ./build.sh linux-arm64 macos-arm64
#
# Default targets:
#   linux-arm64    aarch64-unknown-linux-musl   (static, via cross)
#   linux-amd64    x86_64-unknown-linux-musl    (static, via cross)
#   macos-arm64    aarch64-apple-darwin
#   macos-amd64    x86_64-apple-darwin
#   windows-amd64  x86_64-pc-windows-gnu        (.exe, via cross)
#
# Manual-only (native FreeBSD host required):
#   freebsd-arm64  aarch64-unknown-freebsd
#   freebsd-amd64  x86_64-unknown-freebsd

set -eu

BIN=natpmp
DIST=dist
ALL_TARGETS="linux-arm64 linux-amd64 macos-arm64 macos-amd64 windows-amd64"

die()  { echo "error: $*" >&2; exit 1; }
info() { printf '==> %s\n' "$*"; }

triple_for() {
    case "$1" in
        linux-arm64)   echo "aarch64-unknown-linux-musl"  ;;
        linux-amd64)   echo "x86_64-unknown-linux-musl"   ;;
        macos-arm64)   echo "aarch64-apple-darwin"         ;;
        macos-amd64)   echo "x86_64-apple-darwin"          ;;
        freebsd-arm64) echo "aarch64-unknown-freebsd"      ;;
        freebsd-amd64) echo "x86_64-unknown-freebsd"       ;;
        windows-amd64) echo "x86_64-pc-windows-gnu"        ;;
        *) die "unknown target '$1'. Valid: $ALL_TARGETS" ;;
    esac
}

tool_for() {
    case "$1" in
        linux-*)   echo "cross" ;;
        macos-*)   echo "cargo" ;;
        freebsd-*) echo "cross" ;;
        windows-*) echo "cross" ;;
    esac
}

exe_ext() {
    case "$1" in
        windows-*) echo ".exe" ;;
        *) echo "" ;;
    esac
}

check_deps() {
    command -v rustup >/dev/null 2>&1 || die "rustup not found"

    for t in "$@"; do
        if [ "$(tool_for "$t")" = "cross" ]; then
            command -v cross >/dev/null 2>&1 \
                || die "'cross' not found — install with: cargo install cross"
            docker info >/dev/null 2>&1 \
                || die "Docker is not running (required by cross for Linux builds)"
            break
        fi
    done
}

build_target() {
    local label="$1"
    local triple; triple="$(triple_for "$label")"
    local tool;   tool="$(tool_for "$label")"
    local ext;    ext="$(exe_ext "$label")"

    info "$label  →  $triple"

    rustup target add "$triple" 2>/dev/null || true

    "$tool" build --release --target "$triple"

    local out="$DIST/$BIN-$label$ext"
    cp "target/$triple/release/$BIN$ext" "$out"
    printf '    %-32s %s\n' "$out" "$(du -sh "$out" | cut -f1)"
}

main() {
    if [ $# -eq 0 ]; then
        set -- $ALL_TARGETS
    else
        for t in "$@"; do triple_for "$t" >/dev/null; done  # validate early
    fi

    check_deps "$@"
    mkdir -p "$DIST"

    for t in "$@"; do
        build_target "$t"
    done

    printf '\nArtifacts in %s/:\n' "$DIST"
    ls -lh "$DIST/$BIN"-*
}

main "$@"
