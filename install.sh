#!/bin/sh
# fastcription installer.
#
#   curl -fsSL https://raw.githubusercontent.com/alejandro-llanes/fastcription/master/install.sh | sh
#
# Installs into your home directory by default — no root, nothing outside
# ~/.local — and tells you what it did. It is run through a pipe, so it never
# asks a question: everything it would ask is an environment variable.
#
#   FASTCRIPTION_VERSION=v0.1.0   install a particular release (default: latest)
#   PREFIX=/usr/local             install there instead of ~/.local (needs write access)
#   BIN_DIR=/somewhere/bin        just the binary's directory
#   NO_DESKTOP=1                  skip the menu entry and the icon
#
# What it does *not* do: install voxtype, PipeWire, whisper or Ollama. It
# checks for the ones fastcription cannot run without and tells you which are
# missing — see docs/INSTALL.md for those.

set -eu

REPO="alejandro-llanes/fastcription"
TARGET="x86_64-unknown-linux-gnu"

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="${BIN_DIR:-$PREFIX/bin}"
DESKTOP_DIR="${DESKTOP_DIR:-$PREFIX/share/applications}"
ICON_DIR="${ICON_DIR:-$PREFIX/share/icons/hicolor/scalable/apps}"

# Colour only when a terminal is watching; a logged pipe should stay plain.
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    B=$(printf '\033[1m'); DIM=$(printf '\033[2m'); R=$(printf '\033[0m')
    GREEN=$(printf '\033[32m'); YELLOW=$(printf '\033[33m'); RED=$(printf '\033[31m')
else
    B=''; DIM=''; R=''; GREEN=''; YELLOW=''; RED=''
fi

say()  { printf '%s\n' "$*"; }
step() { printf '%s==>%s %s\n' "$B" "$R" "$*"; }
warn() { printf '%s warn%s %s\n' "$YELLOW" "$R" "$*"; }
die()  { printf '%serror%s %s\n' "$RED" "$R" "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# ---------------------------------------------------------------- the host

[ "$(uname -s)" = "Linux" ] || die "fastcription is Linux-only (this is $(uname -s))."

arch=$(uname -m)
case "$arch" in
    x86_64|amd64) ;;
    *) die "No $arch build yet — only $TARGET is published.
      Build from source instead: https://github.com/$REPO#build-from-source" ;;
esac

have curl || die "curl is needed to download the release."
have tar  || die "tar is needed to unpack the release."

if   have sha256sum; then checksum() { sha256sum "$1" | cut -d' ' -f1; }
elif have shasum;    then checksum() { shasum -a 256 "$1" | cut -d' ' -f1; }
else die "Neither sha256sum nor shasum is available; cannot verify the download."
fi

# --------------------------------------------------------------- the release

version="${FASTCRIPTION_VERSION:-}"
if [ -z "$version" ]; then
    step "Looking up the latest release"
    version=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1) || true
    [ -n "$version" ] || die "Could not find a release. Is one published yet?
      https://github.com/$REPO/releases"
fi
# Accept 0.1.0 as well as v0.1.0.
case "$version" in v*) tag="$version" ;; *) tag="v$version" ;; esac
plain="${tag#v}"

name="fastcription-$plain-$TARGET"
base="https://github.com/$REPO/releases/download/$tag"

tmp=$(mktemp -d)
cleanup() { rm -rf "$tmp"; }
trap cleanup EXIT INT TERM

step "Downloading fastcription $plain"
curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/$name.tar.gz" "$base/$name.tar.gz" \
    || die "Could not download $base/$name.tar.gz
      Check that $tag exists: https://github.com/$REPO/releases"
curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/$name.tar.gz.sha256" "$base/$name.tar.gz.sha256" \
    || die "Could not download the checksum for $tag. Refusing to install unverified."

step "Verifying"
want=$(cut -d' ' -f1 < "$tmp/$name.tar.gz.sha256")
got=$(checksum "$tmp/$name.tar.gz")
[ "$want" = "$got" ] || die "Checksum mismatch — not installing.
      expected $want
      got      $got"
say "    ${DIM}sha256 $got${R}"

tar -C "$tmp" -xzf "$tmp/$name.tar.gz"
[ -f "$tmp/$name/fastcription" ] || die "The archive did not contain the binary."

# --------------------------------------------------------------- installing

step "Installing"
mkdir -p "$BIN_DIR" || die "Cannot create $BIN_DIR"
install -m 755 "$tmp/$name/fastcription" "$BIN_DIR/fastcription" \
    || die "Cannot write to $BIN_DIR. Set PREFIX or BIN_DIR to somewhere you can write."
say "    $BIN_DIR/fastcription"

if [ -z "${NO_DESKTOP:-}" ]; then
    if mkdir -p "$DESKTOP_DIR" "$ICON_DIR" 2>/dev/null; then
        install -m 644 "$tmp/$name/fastcription.desktop" "$DESKTOP_DIR/" 2>/dev/null \
            && say "    $DESKTOP_DIR/fastcription.desktop"
        install -m 644 "$tmp/$name/fastcription.svg" "$ICON_DIR/" 2>/dev/null \
            && say "    $ICON_DIR/fastcription.svg"
        # Best effort: a desktop that does not have these still finds the
        # entry, it may just take a re-login to notice it.
        have update-desktop-database && update-desktop-database "$DESKTOP_DIR" 2>/dev/null || true
        have gtk-update-icon-cache && gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" 2>/dev/null || true
    else
        warn "Could not create the desktop directories; skipping the menu entry."
    fi
fi

# ------------------------------------------------------------- what is left

installed_version=$("$BIN_DIR/fastcription" --version 2>/dev/null || echo "")
say ""
printf '%s%sfastcription %s installed.%s\n' "$GREEN" "$B" "${installed_version:-$plain}" "$R"
say ""

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        warn "$BIN_DIR is not on your PATH."
        say "      Add it to your shell's rc file:"
        say ""
        say "          ${B}export PATH=\"\$PATH:$BIN_DIR\"${R}"
        say ""
        ;;
esac

missing=""
have pactl || missing="$missing pactl"
have parec || missing="$missing parec"
if [ -n "$missing" ]; then
    warn "Missing from PATH:$missing"
    say "      fastcription records through PipeWire or PulseAudio. On Arch these are"
    say "      in ${B}libpulse${R}; on Debian and Ubuntu, ${B}pulseaudio-utils${R}."
    say ""
fi

if have voxtype; then
    if voxtype info models 2>/dev/null | grep -qi installed; then
        say "${GREEN}  ok${R}  voxtype, with a model installed"
    else
        warn "voxtype is installed but has no model yet:"
        say ""
        say "          ${B}voxtype setup --download${R}"
        say ""
    fi
else
    warn "voxtype was not found. fastcription transcribes with it and cannot run without it."
    say "      https://github.com/peteonrails/voxtype"
    say ""
fi

say "Next: ${B}fastcription${R}, pick a source, press Start."
say "${DIM}Transcription on a GPU, and meanings for unknown words, are optional and"
say "documented in docs/INSTALL.md and docs/SERVER.md.${R}"
