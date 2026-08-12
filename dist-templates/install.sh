#!/bin/sh
# reporch installer.
#
# AUTHORED BUT NEVER EXECUTED, and never published — https://reporch.dev/install.sh does not
# exist. cargo-dist generates the real installer from the release artifacts; this copy is
# committed so the script a user would pipe into their shell is reviewable in the repository
# rather than only on a CDN.
#
#   curl -LsSf https://reporch.dev/install.sh | sh
#
set -eu

REPO="sezeryavuz/reporch"
BIN="reporch"
ALIAS="rp"

say()  { printf '%s\n' "$*"; }
err()  { printf 'error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || err "need $1 (command not found)"; }

detect_target() {
    _os=$(uname -s)
    _arch=$(uname -m)
    case "$_os" in
        Darwin) _os=apple-darwin ;;
        Linux)  _os=unknown-linux-gnu ;;
        MINGW*|MSYS*|CYGWIN*)
            err "reporch does not run on native Windows: Claude Code's cross-session messaging
       is not available there. Install inside WSL 2 and run it from that shell." ;;
        *) err "unsupported operating system: $_os" ;;
    esac
    case "$_arch" in
        x86_64|amd64) _arch=x86_64 ;;
        arm64|aarch64) _arch=aarch64 ;;
        *) err "unsupported architecture: $_arch" ;;
    esac
    printf '%s-%s' "$_arch" "$_os"
}

install_dir() {
    if [ -n "${REPORCH_INSTALL_DIR:-}" ]; then printf '%s' "$REPORCH_INSTALL_DIR"; return; fi
    if [ -n "${CARGO_HOME:-}" ]; then printf '%s/bin' "$CARGO_HOME"; return; fi
    if [ -d "$HOME/.cargo/bin" ]; then printf '%s/.cargo/bin' "$HOME"; return; fi
    printf '%s/.local/bin' "$HOME"
}

need uname
need mktemp
if command -v curl >/dev/null 2>&1; then DL="curl -LsSf -o"
elif command -v wget >/dev/null 2>&1; then DL="wget -qO"
else err "need curl or wget"; fi

TARGET=$(detect_target)
DIR=$(install_dir)
VERSION="${REPORCH_VERSION:-latest}"

if [ "$VERSION" = "latest" ]; then
    URL="https://github.com/$REPO/releases/latest/download/$BIN-$TARGET.tar.gz"
else
    URL="https://github.com/$REPO/releases/download/v$VERSION/$BIN-$TARGET.tar.gz"
fi

say "installing $BIN ($TARGET) to $DIR"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM

$DL "$TMP/$BIN.tar.gz" "$URL" || err "download failed: $URL"
tar -xzf "$TMP/$BIN.tar.gz" -C "$TMP" || err "could not unpack the archive"

mkdir -p "$DIR"
_found=$(find "$TMP" -type f -name "$BIN" -perm -u+x | head -n 1)
[ -n "$_found" ] || err "the archive did not contain a $BIN binary"
install -m 755 "$_found" "$DIR/$BIN"

# `rp` is the short alias, installed by every channel (§14). Prefer the real second binary from
# the archive; fall back to a symlink so the alias always exists.
_alias=$(find "$TMP" -type f -name "$ALIAS" -perm -u+x | head -n 1)
if [ -n "$_alias" ]; then
    install -m 755 "$_alias" "$DIR/$ALIAS"
else
    ln -sf "$DIR/$BIN" "$DIR/$ALIAS"
fi

say ""
say "  installed $BIN and $ALIAS to $DIR"
case ":$PATH:" in
    *":$DIR:"*) ;;
    *) say ""
       say "  $DIR is not on your PATH. Add it:"
       say ""
       say "    export PATH=\"$DIR:\$PATH\"" ;;
esac
say ""
say "  reporch drives Claude Code, which it does not install. Check the environment with:"
say ""
say "    reporch doctor"
say ""
