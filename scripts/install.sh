#!/bin/sh
# Installs Pistolame for the current user on Linux (x86-64), without administrator rights:
#
#   curl -fsSL https://raw.githubusercontent.com/Dacryoserum/pistolame/main/scripts/install.sh | sh
#
# - the app goes to ~/.local/share/pistolame, with a `pistolame` command in ~/.local/bin and an
#   entry in the applications menu;
# - the archive is checked against the SHA-256 published with the release;
# - Pistolame then updates itself (signed updates), like on Windows and macOS.
#
# Options:  --uninstall   remove the app (your library and settings in ~/.config/pistolame stay)
#           --version X   install release X (for example 0.6.0) instead of the latest one
# Licensed like Pistolame: GPL-3.0.

set -eu

REPOSITORY="Dacryoserum/pistolame"
RELEASES_URL="https://github.com/$REPOSITORY/releases"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
INSTALL_FOLDER="$DATA_HOME/pistolame"
COMMAND_FOLDER="$HOME/.local/bin"
COMMAND_LINK="$COMMAND_FOLDER/pistolame"
DESKTOP_ENTRY="$DATA_HOME/applications/pistolame.desktop"
ICON_FILE="$DATA_HOME/icons/hicolor/256x256/apps/pistolame.png"
EXECUTABLE_NAME="Pistolame"

say() { printf '%s\n' "$*"; }
fail() { printf 'Erreur : %s\n' "$*" >&2; exit 1; }

# Downloads $1 to the file $2 with curl or wget.
download() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1"
    else
        fail "curl ou wget est nécessaire."
    fi
}

# Prints the SHA-256 of the file $1.
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
    else
        fail "sha256sum est nécessaire pour vérifier le téléchargement."
    fi
}

uninstall() {
    rm -rf "$INSTALL_FOLDER"
    rm -f "$COMMAND_LINK" "$DESKTOP_ENTRY" "$ICON_FILE"
    refresh_menus
    say "Pistolame est désinstallé. Votre bibliothèque et vos réglages restent dans ~/.config/pistolame."
}

# Lets the desktop see the new entry and icon at once (optional tools).
refresh_menus() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q "$DATA_HOME/applications" 2>/dev/null || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t "$DATA_HOME/icons/hicolor" 2>/dev/null || true
    fi
}

# Warns about the system libraries Pistolame needs and how to install them.
check_libraries() {
    ldconfig_command=""
    for candidate in ldconfig /sbin/ldconfig /usr/sbin/ldconfig; do
        if command -v "$candidate" >/dev/null 2>&1; then
            ldconfig_command="$candidate"
            break
        fi
    done
    [ -n "$ldconfig_command" ] || return 0
    libraries=$("$ldconfig_command" -p 2>/dev/null || true)
    missing=""
    for library in libasound.so.2 libfontconfig.so.1 libxkbcommon.so.0; do
        case "$libraries" in
            *"$library"*) ;;
            *) missing="$missing $library" ;;
        esac
    done
    [ -z "$missing" ] && return 0
    say ""
    say "Bibliothèques système manquantes :$missing"
    if command -v apt-get >/dev/null 2>&1; then
        say "  sudo apt install libasound2t64 libfontconfig1 libxkbcommon0   (Debian 12 : libasound2)"
    elif command -v dnf >/dev/null 2>&1; then
        say "  sudo dnf install alsa-lib fontconfig libxkbcommon"
    elif command -v pacman >/dev/null 2>&1; then
        say "  sudo pacman -S alsa-lib fontconfig libxkbcommon"
    elif command -v zypper >/dev/null 2>&1; then
        say "  sudo zypper install libasound2 fontconfig libxkbcommon0"
    fi
}

install_release() {
    requested_version="$1"
    [ "$(uname -s)" = "Linux" ] || fail "ce script est pour Linux (Windows et macOS : voir $RELEASES_URL)."
    case "$(uname -m)" in
        x86_64 | amd64) ;;
        *) fail "seuls les processeurs x86-64 sont pris en charge pour l'instant." ;;
    esac

    work_folder=$(mktemp -d)
    trap 'rm -rf "$work_folder"' EXIT INT TERM

    if [ -n "$requested_version" ]; then
        version="$requested_version"
    else
        download "$RELEASES_URL/latest/download/latest.json" "$work_folder/latest.json"
        version=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$work_folder/latest.json" | head -n 1)
        [ -n "$version" ] || fail "impossible de lire la dernière version."
    fi
    archive_name="Pistolame-$version-linux-x64.tar.gz"
    say "Téléchargement de Pistolame $version…"
    download "$RELEASES_URL/download/v$version/$archive_name" "$work_folder/$archive_name"
    download "$RELEASES_URL/download/v$version/$archive_name.sha256" "$work_folder/$archive_name.sha256"

    expected=$(cut -d ' ' -f 1 "$work_folder/$archive_name.sha256")
    actual=$(sha256_of "$work_folder/$archive_name")
    [ "$expected" = "$actual" ] || fail "l'archive ne correspond pas à son empreinte SHA-256 ; rien n'a été installé."

    tar -xzf "$work_folder/$archive_name" -C "$work_folder"
    unpacked="$work_folder/Pistolame-$version-linux-x64"
    [ -x "$unpacked/$EXECUTABLE_NAME" ] || fail "l'archive ne contient pas l'application."

    # Replace the previous copy in one move, so a failed download never leaves half an app.
    mkdir -p "$DATA_HOME"
    rm -rf "$INSTALL_FOLDER.new"
    mv "$unpacked" "$INSTALL_FOLDER.new"
    rm -rf "$INSTALL_FOLDER"
    mv "$INSTALL_FOLDER.new" "$INSTALL_FOLDER"

    mkdir -p "$COMMAND_FOLDER" "$(dirname "$DESKTOP_ENTRY")" "$(dirname "$ICON_FILE")"
    ln -sf "$INSTALL_FOLDER/$EXECUTABLE_NAME" "$COMMAND_LINK"
    cp "$INSTALL_FOLDER/pistolame.png" "$ICON_FILE"
    cat >"$DESKTOP_ENTRY" <<ENTRY
[Desktop Entry]
Type=Application
Name=Pistolame
Comment=Lecteur audio sans perte, bit-perfect
Exec="$INSTALL_FOLDER/$EXECUTABLE_NAME" %F
Icon=pistolame
Categories=AudioVideo;Audio;Player;
MimeType=audio/flac;audio/x-flac;audio/mp4;audio/x-m4a;audio/mpeg;audio/x-wav;audio/x-aiff;audio/ogg;
Terminal=false
ENTRY
    refresh_menus

    say "Pistolame $version est installé : lancez-le depuis le menu des applications ou avec la commande « pistolame »."
    case ":$PATH:" in
        *":$COMMAND_FOLDER:"*) ;;
        *) say "Pour la commande « pistolame », ajoutez ~/.local/bin à votre PATH (puis rouvrez le terminal)." ;;
    esac
    check_libraries
}

main() {
    requested_version=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --uninstall) uninstall; return 0 ;;
            --version)
                [ $# -ge 2 ] || fail "--version attend un numéro, par exemple 0.6.0."
                requested_version="${2#v}"
                shift 2
                ;;
            -h | --help)
                sed -n '2,15p' "$0" 2>/dev/null || true
                return 0
                ;;
            *) fail "option inconnue : $1" ;;
        esac
    done
    install_release "$requested_version"
}

main "$@"
