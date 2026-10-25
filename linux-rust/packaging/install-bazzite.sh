#!/usr/bin/env bash
# BazzitePods installer for Bazzite (and any Flatpak-based Linux desktop).
#
#   Install / update the latest release:
#     curl -fsSL https://raw.githubusercontent.com/DaBarkle/bazzitepods/main/linux-rust/packaging/install-bazzite.sh | bash
#   Install a bundle you already downloaded:
#     ./install-bazzite.sh ~/Downloads/BazzitePods.flatpak
#   Uninstall (app, its data and the autostart entry):
#     ./install-bazzite.sh --uninstall
#
# Per-user install only: no sudo, no rpm-ostree layering. Safe to run repeatedly.

set -euo pipefail

readonly APP_ID="io.github.dabarkle.BazzitePods"
readonly REPO="DaBarkle/bazzitepods"
readonly API_URL="https://api.github.com/repos/${REPO}/releases/latest"
readonly ONE_LINER="curl -fsSL https://raw.githubusercontent.com/DaBarkle/bazzitepods/main/linux-rust/packaging/install-bazzite.sh | bash -s --"
readonly FLATHUB_URL="https://dl.flathub.org/repo/flathub.flatpakrepo"
readonly ASSET_NAME="BazzitePods.flatpak"
readonly AUTOSTART_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
readonly AUTOSTART_FILE="${AUTOSTART_DIR}/${APP_ID}.desktop"
# The app's id before it was renamed to BazzitePods; removed on install.
readonly OLD_APP_ID="io.github.dabarkle.LibrePods"

if [[ -t 1 ]]; then
    BOLD=$'\e[1m'; GREEN=$'\e[32m'; YELLOW=$'\e[33m'; RED=$'\e[31m'; RESET=$'\e[0m'
else
    BOLD=""; GREEN=""; YELLOW=""; RED=""; RESET=""
fi

step() { printf '%s==>%s %s\n' "$BOLD" "$RESET" "$*"; }
ok()   { printf '%s  ✓%s %s\n' "$GREEN" "$RESET" "$*"; }
warn() { printf '%s  !%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
die()  { printf '%serror:%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS] [BUNDLE.flatpak]

Installs or updates BazzitePods as a per-user Flatpak.

  BUNDLE.flatpak   install this local bundle instead of downloading the latest release
  --uninstall      remove BazzitePods, its data and its autostart entry
  --no-launch      do not start BazzitePods after installing
  -h, --help       show this help
EOF
}

TMPDIR_CREATED=""
cleanup() {
    if [[ -n "$TMPDIR_CREATED" ]]; then
        rm -rf -- "$TMPDIR_CREATED"
    fi
}
trap cleanup EXIT

require() {
    command -v "$1" >/dev/null 2>&1 || die "'$1' is required but not installed."
}

ensure_flathub() {
    step "Checking the Flathub remote (provides the runtime)"
    flatpak remote-add --user --if-not-exists flathub "$FLATHUB_URL"
    ok "Flathub remote available"
}

download_latest() {
    local dest_dir=$1 json url
    require curl
    step "Looking up the latest release of ${REPO}" >&2
    if ! json=$(curl -fsSL -H "Accept: application/vnd.github+json" "$API_URL"); then
        die "could not reach the GitHub API. Download ${ASSET_NAME} manually from https://github.com/${REPO}/releases and run: ${ONE_LINER} /path/to/${ASSET_NAME}"
    fi
    # Pick the browser_download_url of the first *.flatpak asset (no jq on a stock system).
    url=$(printf '%s\n' "$json" \
        | grep -o '"browser_download_url"[[:space:]]*:[[:space:]]*"[^"]*\.flatpak"' \
        | head -n1 \
        | sed -E 's/.*"(https:[^"]*)"$/\1/') || true
    [[ -n "$url" ]] || die "the latest release has no .flatpak asset."
    local tag
    tag=$(printf '%s\n' "$json" | grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' \
        | head -n1 | sed -E 's/.*"([^"]*)"$/\1/') || true
    step "Downloading ${tag:-latest}: ${url##*/}" >&2
    curl -fL --progress-bar -o "${dest_dir}/${ASSET_NAME}" "$url" \
        || die "download failed: $url"
    printf '%s\n' "${dest_dir}/${ASSET_NAME}"
}

remove_old_id() {
    if ! flatpak info --user "$OLD_APP_ID" >/dev/null 2>&1; then
        return
    fi
    step "Replacing the pre-rename build (${OLD_APP_ID})"
    flatpak kill "$OLD_APP_ID" >/dev/null 2>&1 || true
    flatpak uninstall --user --noninteractive -y --delete-data "$OLD_APP_ID" >/dev/null
    flatpak override --user --reset "$OLD_APP_ID" >/dev/null 2>&1 || true
    flatpak permission-remove background background "$OLD_APP_ID" >/dev/null 2>&1 || true
    rm -f -- "${AUTOSTART_DIR}/${OLD_APP_ID}.desktop"
    ok "Old build removed"
}

do_install() {
    local bundle=$1 launch=$2
    require flatpak

    if [[ -z "$bundle" ]]; then
        TMPDIR_CREATED=$(mktemp -d)
        bundle=$(download_latest "$TMPDIR_CREATED")
    fi
    [[ -f "$bundle" ]] || die "no such file: $bundle"
    ok "Bundle: $bundle"

    ensure_flathub

    # Stop a running copy so the new version is what starts afterwards.
    flatpak kill "$APP_ID" >/dev/null 2>&1 || true

    remove_old_id

    step "Installing ${APP_ID} (per user)"
    # --reinstall makes repeated runs (and updates from a bundle) idempotent.
    flatpak install --user --noninteractive -y --reinstall "$bundle"
    ok "BazzitePods installed"

    if [[ "$launch" == "yes" ]]; then
        step "Starting BazzitePods once so it can register to start at login"
        if flatpak ps --columns=application 2>/dev/null | grep -qx "$APP_ID"; then
            ok "BazzitePods is already running"
        else
            nohup flatpak run "$APP_ID" --start-minimized >/dev/null 2>&1 &
            disown || true
            ok "BazzitePods is running in the system tray"
        fi
    fi

    printf '\n%sDone.%s BazzitePods starts automatically at login and lives in the system tray.\n' "$BOLD" "$RESET"
    printf 'To remove it later run:\n  %s --uninstall\n' "$ONE_LINER"
}

do_uninstall() {
    require flatpak
    step "Stopping BazzitePods"
    flatpak kill "$APP_ID" >/dev/null 2>&1 || true

    step "Removing ${APP_ID}"
    if flatpak info --user "$APP_ID" >/dev/null 2>&1; then
        flatpak uninstall --user --noninteractive -y --delete-data "$APP_ID"
        ok "App and its data removed"
    else
        ok "App was not installed for this user"
    fi

    # Written by the Background portal (or by the app itself outside Flatpak).
    if [[ -e "$AUTOSTART_FILE" ]]; then
        rm -f -- "$AUTOSTART_FILE"
        ok "Removed autostart entry $AUTOSTART_FILE"
    else
        ok "No autostart entry"
    fi
    # Forget the Background-portal autostart grant.
    flatpak permission-remove background background "$APP_ID" >/dev/null 2>&1 || true

    printf '\n%sBazzitePods has been uninstalled.%s\n' "$BOLD" "$RESET"
}

main() {
    local mode=install launch=yes bundle=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --uninstall) mode=uninstall ;;
            --no-launch) launch=no ;;
            -h|--help) usage; exit 0 ;;
            -*) usage >&2; die "unknown option: $1" ;;
            *)
                [[ -z "$bundle" ]] || die "only one bundle may be given"
                bundle=$1
                ;;
        esac
        shift
    done

    if [[ $EUID -eq 0 ]]; then
        die "run this as your normal user, not root (it installs for your user only)."
    fi

    case "$mode" in
        install) do_install "$bundle" "$launch" ;;
        uninstall) do_uninstall ;;
    esac
}

main "$@"
