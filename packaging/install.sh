#!/bin/sh
# Installs or updates TerminalOmega for the current user. No sudo needed.
# Usage: ./install.sh [--uninstall]
# Saved sessions and settings are never touched.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
action=${1:-install}
case "$action" in
    install | --uninstall) ;;
    *) echo "Usage: $0 [--uninstall]" >&2; exit 2 ;;
esac

install_linux() {
    bin_dir="$HOME/.local/bin"
    data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
    apps_dir="$data_dir/applications"
    icons_root="$data_dir/icons/hicolor"
    icon_dir="$icons_root/512x512/apps"
    exe="$bin_dir/terminal-omega"

    if [ "$action" = --uninstall ]; then
        rm -f "$exe" "$apps_dir/terminal-omega.desktop" "$icon_dir/terminal-omega.png"
        echo "TerminalOmega removed."
    else
        # Fails here, before touching any installed version, if this build
        # can't run on this system (e.g. it needs a newer glibc).
        if ! "$here/terminal-omega" --version >/dev/null; then
            echo "This build can't run on this system. Create a package on this machine with release.sh." >&2
            exit 1
        fi
        mkdir -p "$bin_dir" "$apps_dir" "$icon_dir"
        # Renaming into place works even while the old version is running.
        cp "$here/terminal-omega" "$exe.new"
        chmod 755 "$exe.new"
        mv -f "$exe.new" "$exe"
        cp "$here/icon.png" "$icon_dir/terminal-omega.png"
        sed "s|@EXEC@|$exe|" "$here/terminal-omega.desktop" > "$apps_dir/terminal-omega.desktop"
        echo "Installed $("$exe" --version). Find it in Show Apps as TerminalOmega."
    fi

    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q "$apps_dir" || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t "$icons_root" 2>/dev/null || true
    fi
}

install_macos() {
    apps_dir="$HOME/Applications"
    app="$apps_dir/TerminalOmega.app"

    if [ "$action" = --uninstall ]; then
        rm -rf "$app"
        echo "TerminalOmega removed."
        return
    fi

    new="$apps_dir/.TerminalOmega.new.app"
    mkdir -p "$apps_dir"
    rm -rf "$new"
    cp -R "$here/TerminalOmega.app" "$new"
    # The build isn't notarized by Apple, so clear the "downloaded" flag.
    xattr -dr com.apple.quarantine "$new" 2>/dev/null || true
    if ! "$new/Contents/MacOS/terminal-omega" --version >/dev/null; then
        rm -rf "$new"
        echo "This build can't run on this Mac. Create a package on this machine with release.sh." >&2
        exit 1
    fi
    rm -rf "$app"
    mv "$new" "$app"
    echo "Installed $("$app/Contents/MacOS/terminal-omega" --version). Find it in ~/Applications or Spotlight."
}

case "$(uname -s)" in
    Linux) install_linux ;;
    Darwin) install_macos ;;
    *) echo "Unsupported system: $(uname -s)" >&2; exit 1 ;;
esac
