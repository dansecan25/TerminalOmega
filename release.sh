#!/usr/bin/env bash
# Builds an installable package for the system this runs on (Linux or macOS).
# Usage: ./release.sh [new-version]
#
# Output: dist/terminal-omega-<version>-<os>-<arch>.tar.gz
# Passing a version edits Cargo.toml. Nothing is committed, tagged or uploaded.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
export PATH="$HOME/.cargo/bin:$PATH"

die() { echo "error: $*" >&2; exit 1; }

command -v cargo >/dev/null || die "cargo not found. Install Rust from https://rustup.rs"

if [[ $# -gt 1 ]]; then
    die "usage: ./release.sh [new-version]"
fi
if [[ $# -eq 1 ]]; then
    [[ $1 =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version must look like 1.2.3"
    # Only the first match, which is the [package] version. Not `sed -i`:
    # its syntax differs between Linux and macOS.
    awk -v v="$1" '!done && /^version = /{print "version = \"" v "\""; done=1; next} {print}' \
        Cargo.toml > Cargo.toml.new
    mv Cargo.toml.new Cargo.toml
fi
version=$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)

case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    *) die "unsupported system: $(uname -s)" ;;
esac
arch=$(uname -m)
name="terminal-omega-$version-$os-$arch"

cargo test --release
cargo build --release

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
root="$stage/$name"
mkdir -p "$root"
cp packaging/install.sh "$root/"
chmod 755 "$root/install.sh"

if [[ $os == linux ]]; then
    cp target/release/terminal-omega packaging/icon.png packaging/terminal-omega.desktop "$root/"
else
    command -v iconutil >/dev/null || die "iconutil not found. Install the Xcode command line tools"
    app="$root/TerminalOmega.app/Contents"
    mkdir -p "$app/MacOS" "$app/Resources"
    cp target/release/terminal-omega "$app/MacOS/"
    sed "s/@VERSION@/$version/g" packaging/Info.plist > "$app/Info.plist"

    iconset="$stage/icon.iconset"
    mkdir "$iconset"
    for size in 16 32 128 256 512; do
        sips -z "$size" "$size" packaging/icon.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
        double=$((size * 2))
        if [[ $double -le 512 ]]; then
            sips -z "$double" "$double" packaging/icon.png \
                --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
        fi
    done
    iconutil -c icns "$iconset" -o "$app/Resources/icon.icns"

    # Apple Silicon refuses to run a bundle with no signature at all.
    codesign --force --deep --sign - "$root/TerminalOmega.app"
fi

mkdir -p dist
tar -C "$stage" -czf "dist/$name.tar.gz" "$name"
echo
echo "Created dist/$name.tar.gz"
echo "To install or update: extract it, then run $name/install.sh"
