#!/usr/bin/env bash
# Run a command inside a private, headless KWin session. Nothing appears on your real
# screen, and screenshots taken inside it can't capture your desktop.
#
# The command runs with DISPLAY set to the session's XWayland and WAYLAND_DISPLAY unset,
# so GPUI uses X11: drive it with `DISPLAY=<display> xdotool ...` and capture a window
# with `import -display <display> -window <id> out.png`.
#
# Usage: scripts/virtual-display.sh [-s WIDTHxHEIGHT] [-o DISPLAY_FILE] -- command [args...]
#   -s  virtual screen size (default 1600x1000)
#   -o  write the X display name (e.g. ":3") to this file once the session is up
set -euo pipefail

size=1600x1000
display_file=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -s) size=$2; shift 2 ;;
        -o) display_file=$2; shift 2 ;;
        --) shift; break ;;
        *) break ;;
    esac
done
if [[ $# -eq 0 ]]; then
    echo "usage: $0 [-s WxH] [-o display-file] -- command [args...]" >&2
    exit 2
fi
command -v kwin_wayland >/dev/null || { echo "kwin_wayland not found" >&2; exit 1; }

inner=$(mktemp --suffix=.sh)
trap 'rm -f "$inner"' EXIT
{
    echo '#!/usr/bin/env bash'
    echo 'unset WAYLAND_DISPLAY'
    if [[ -n $display_file ]]; then printf 'echo "$DISPLAY" > %q\n' "$display_file"; fi
    printf 'exec'; printf ' %q' "$@"; echo
} > "$inner"
chmod +x "$inner"

# A unique socket per run avoids clashing with stale lock files from killed sessions.
kwin_wayland --virtual --xwayland --socket "sentinel-vd-$$" \
    --width "${size%x*}" --height "${size#*x}" \
    --exit-with-session "$inner" 2> >(grep -v -E '^(kwin_|qt\.|KCrash)' >&2)
