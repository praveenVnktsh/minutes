#!/bin/bash
# Install or update Minutes on an Apple Silicon Mac.
#
#   curl -fsSL https://praveenvnktsh.github.io/minutes/install.sh | bash
#
# Downloads the latest release from GitHub, copies minutes.app into
# /Applications (or ~/Applications when /Applications isn't writable) and
# opens it. Files fetched with curl are not quarantined, so macOS opens the
# app without the "could not verify" prompt a browser download gets.
#
# Environment:
#   MINUTES_INSTALL_DIR  install somewhere else
#   MINUTES_NO_OPEN=1    don't open the app afterwards
#   MINUTES_DMG_URL      install from another .dmg

set -euo pipefail

# Everything runs inside main, so a download cut off halfway runs nothing.
main() {
  local repo="praveenvnktsh/minutes"
  local dmg_url="${MINUTES_DMG_URL:-https://github.com/$repo/releases/latest/download/minutes-macos-arm64.dmg}"
  local app_name="minutes.app"

  bold() { printf '\033[1m%s\033[0m\n' "$*"; }
  info() { printf '  %s\n' "$*"; }
  fail() { printf '\033[31mError:\033[0m %s\n' "$*" >&2; exit 1; }

  [ "$(uname -s)" = "Darwin" ] || fail "Minutes runs on macOS only."
  if [ "$(uname -m)" != "arm64" ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" != "1" ]; then
    fail "Minutes needs an Apple Silicon Mac (M1 or later)."
  fi
  local macos major
  macos="$(sw_vers -productVersion)"
  major="${macos%%.*}"
  [ "$major" -ge 13 ] || fail "Minutes needs macOS 13 or later (this Mac has $macos)."

  local dest="${MINUTES_INSTALL_DIR:-}"
  if [ -z "$dest" ]; then
    if [ -w /Applications ]; then dest=/Applications; else dest="$HOME/Applications"; fi
  fi
  mkdir -p "$dest"
  local target="$dest/$app_name"

  bold "Installing Minutes into $dest"

  # Globals: the EXIT trap runs after main has returned.
  MINUTES_TMP="$(mktemp -d)"
  local tmp="$MINUTES_TMP" mnt="$MINUTES_TMP/mnt"
  trap 'rc=$?; hdiutil detach -quiet "$MINUTES_TMP/mnt" 2>/dev/null || true; rm -rf "$MINUTES_TMP"; exit $rc' EXIT

  info "Downloading the latest release…"
  curl -fL --progress-bar -o "$tmp/minutes.dmg" "$dmg_url" || fail "Download failed: $dmg_url"

  hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$mnt" "$tmp/minutes.dmg" >/dev/null \
    || fail "Could not open the downloaded disk image."
  [ -d "$mnt/$app_name" ] || fail "The disk image has no $app_name."
  local version
  version="$(defaults read "$mnt/$app_name/Contents/Info.plist" CFBundleShortVersionString 2>/dev/null || echo "?")"

  # Quit this copy of Minutes if it is running, so it isn't replaced underneath itself.
  local was_running=0
  if pgrep -f "$target/Contents/MacOS/" >/dev/null 2>&1; then
    was_running=1
    info "Quitting the running Minutes…"
    osascript -e "tell application \"$target\" to quit" >/dev/null 2>&1 || true
    for _ in $(seq 1 20); do
      pgrep -f "$target/Contents/MacOS/" >/dev/null 2>&1 || break
      sleep 0.5
    done
    pgrep -f "$target/Contents/MacOS/" >/dev/null 2>&1 \
      && fail "Minutes is still running (maybe recording). Quit it and run this again."
  fi

  info "Copying Minutes ${version}…"
  # Copy beside the old app, then swap, so a failed copy leaves the old one intact.
  local staging="$dest/.minutes-install-$$.app"
  rm -rf "$staging"
  ditto "$mnt/$app_name" "$staging"
  xattr -dr com.apple.quarantine "$staging" 2>/dev/null || true
  rm -rf "$target"
  mv "$staging" "$target"

  bold "Minutes $version is installed at $target"
  if [ "$major" -lt 15 ]; then
    info "Note: transcription needs macOS 15 or later; this Mac has $macos."
  fi
  info "Updates install automatically from inside the app."

  if [ "${MINUTES_NO_OPEN:-0}" != "1" ]; then
    open "$target"
    if [ "$was_running" = 1 ]; then info "Reopened Minutes."; fi
  fi
}

main "$@"
