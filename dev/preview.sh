#!/usr/bin/env bash
set -Eeuo pipefail

# The app in a nested Hyprland with a private HOME, XDG directories and D-Bus, on the tests' fake `hey` and `gws`, so
# nothing of the desktop it is run from is read or shown on: not its accounts, its theme, or its windows.
#
#   dev/preview.sh <scene> <screenshot.png> [theme-dir]
#
# `scene` is what the picture shows, reached with the app's own keys:
#   week      the week the fakes' events are in (2026-10-12)
#   agenda    the two weeks around them
#   new       the editor for a new event
#   calendars the same, with its list of calendars open
#   series    the editor for a repeating HEY event, which can only be deleted: every field is off
#   accounts  the Accounts window
#   setup     the window before any account is linked
# `theme-dir` holds a `colors.toml` (an Omarchy theme, such as /usr/share/omarchy/themes/catppuccin-latte) and is
# copied in as the scratch desktop's current theme; without one the app shows its fallback.
#
#   CLOUD_CALENDAR_PREVIEW_BINARY  the app to run (default target/debug/cloud-calendar-gtk)
#   CLOUD_CALENDAR_PREVIEW_SETTLE  seconds to wait before the picture (default 3)
if (( $# < 2 || $# > 3 )); then
  echo "usage: $0 <week|agenda|new|calendars|series|accounts|setup> <screenshot.png> [theme-dir]" >&2
  exit 2
fi
scene=$1
screenshot=$(realpath -m -- "$2")
theme=${3:-}

here=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repo=$(realpath -e -- "$here/..")
binary=${CLOUD_CALENDAR_PREVIEW_BINARY:-$repo/target/debug/cloud-calendar-gtk}
[[ -x $binary ]] || { echo "preview: build the app first (cargo build -p cloud-calendar-gtk)" >&2; exit 1; }
for command_name in Hyprland hyprctl grim jq dbus-daemon setsid wtype; do
  command -v "$command_name" >/dev/null || { echo "preview: $command_name is required" >&2; exit 1; }
done
[[ -z $theme || -f $theme/colors.toml ]] || { echo "preview: $theme has no colors.toml" >&2; exit 1; }

# The fakes' events are in the week of 2026-10-12. `h` goes back a week, or two weeks in the agenda.
days=$(( ( $(date -u +%s) - $(date -u -d 2026-10-12 +%s) ) / 86400 ))
(( days >= 0 )) || { echo "preview: the clock is before the fakes' week" >&2; exit 1; }
back() { local n; for (( n = 0; n < $1; n++ )); do printf 'h '; done; }
# Tab starts on the toolbar's first button: five more reach Accounts, and twelve the fourth button of the week, which
# is the repeating HEY event (after Monday's heading and two events, and Tuesday's heading).
tabs() { local n; for (( n = 0; n < $1; n++ )); do printf 'Tab '; done; }
case $scene in
  week) keys=$(back $(( days / 7 ))) ;;
  agenda) keys="$(back $(( (days + 6) / 7 )))a" ;;
  new) keys=n ;;
  calendars) keys="n shift+ISO_Left_Tab space" ;;
  series) keys="$(back $(( days / 7 )))$(tabs 12)Return" ;;
  accounts) keys="$(tabs 5)Return" ;;
  setup) keys="" ;;
  *) echo "preview: no scene named $scene" >&2; exit 2 ;;
esac

parent_runtime=${XDG_RUNTIME_DIR:?preview: XDG_RUNTIME_DIR is required}
parent_display=${WAYLAND_DISPLAY:?preview: run this from a Wayland desktop}
parent_socket=$parent_runtime/$parent_display
settle=${CLOUD_CALENDAR_PREVIEW_SETTLE:-3}

root=""
runtime=""
hypr_pid=""
app_pid=""
dbus_pid=""
cleanup() {
  local status=$?
  trap - EXIT INT TERM
  local pid
  # Only the PIDs this script started.
  for pid in "$app_pid" "$hypr_pid" "$dbus_pid"; do
    [[ -z $pid ]] || kill -TERM "$pid" 2>/dev/null || true
  done
  for _ in {1..30}; do
    local alive=0
    for pid in "$app_pid" "$hypr_pid" "$dbus_pid"; do
      [[ -z $pid ]] || ! kill -0 "$pid" 2>/dev/null || alive=1
    done
    (( alive == 0 )) && break
    sleep 0.1
  done
  for pid in "$app_pid" "$hypr_pid" "$dbus_pid"; do
    [[ -z $pid ]] || kill -KILL "$pid" 2>/dev/null || true
  done
  [[ -z $root || $root != /tmp/cloud-calendar-preview.* ]] || rm -rf -- "$root"
  [[ -z $runtime || $runtime != /tmp/ccp.* ]] || rm -rf -- "$runtime"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Wayland and Hyprland IPC use AF_UNIX paths capped at 108 bytes.
root=$(mktemp -d /tmp/cloud-calendar-preview.XXXXXX)
runtime=$(mktemp -d /tmp/ccp.XXXXXX)
chmod 700 "$root" "$runtime"
export HOME=$root/home
# The XDG directories where they are by default: Omarchy writes its current theme under ~/.local/state.
export XDG_CONFIG_HOME=$HOME/.config XDG_STATE_HOME=$HOME/.local/state XDG_DATA_HOME=$HOME/.local/share
export XDG_CACHE_HOME=$HOME/.cache
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
export XDG_RUNTIME_DIR=$runtime
export LC_ALL=C TZ=UTC
unset GTK_THEME

if [[ -n $theme ]]; then
  mkdir -p "$XDG_STATE_HOME/omarchy/current/theme"
  cp -- "$theme/colors.toml" "$XDG_STATE_HOME/omarchy/current/theme/colors.toml"
  basename -- "$(realpath -- "$theme")" > "$XDG_STATE_HOME/omarchy/current/theme.name"
fi

# HEY and Google answer from the fakes. iCloud has no icloud-session on this bus, so it is the account that fails.
fakes=$repo/crates/cloud-calendar/tests
mkdir -p "$XDG_CONFIG_HOME/cloud-calendar/gws/google"
[[ $scene == setup ]] || printf '[accounts.%s]\n\n' google hey icloud > "$XDG_CONFIG_HOME/cloud-calendar/config.toml"
: > "$XDG_CONFIG_HOME/cloud-calendar/gws/google/credentials.enc"
export CLOUD_CALENDAR_HEY_COMMAND=$fakes/fake-hey CLOUD_CALENDAR_GWS_COMMAND=$fakes/fake-gws
export CLOUD_CALENDAR_NOTIFY_COMMAND=$fakes/fake-notify CLOUD_CALENDAR_BROWSER=true
export FAKE_GWS_SECRET=preview CLOUD_CALENDAR_GOOGLE_CLIENT_ID=preview CLOUD_CALENDAR_GOOGLE_CLIENT_SECRET=preview

# A session bus with no service directories: nothing can be dbus-activated on it.
printf '%s\n' \
  '<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"' \
  ' "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">' \
  '<busconfig>' \
  '  <type>session</type>' \
  "  <listen>unix:abstract=cloud-calendar-preview-$BASHPID</listen>" \
  '  <keep_umask/>' \
  '  <policy context="default">' \
  '    <allow send_destination="*" eavesdrop="true"/>' \
  '    <allow eavesdrop="true"/>' \
  '    <allow own="*"/>' \
  '  </policy>' \
  '</busconfig>' > "$root/dbus.conf"
mapfile -t dbus_details < <(/usr/bin/dbus-daemon --config-file="$root/dbus.conf" --fork --print-address=1 --print-pid=1)
(( ${#dbus_details[@]} == 2 )) || { echo "preview: could not start the isolated session bus" >&2; exit 1; }
export DBUS_SESSION_BUS_ADDRESS=${dbus_details[0]}
dbus_pid=${dbus_details[1]}

printf '%s\n' \
  'hl.monitor({ output = "", mode = "1280x820@60", position = "auto", scale = 1 })' \
  'hl.config({' \
  '  general = { gaps_in = 0, gaps_out = 0, border_size = 0 },' \
  '  decoration = { rounding = 0, shadow = { enabled = false }, blur = { enabled = false } },' \
  '  animations = { enabled = false },' \
  '  misc = { disable_hyprland_logo = true, disable_splash_rendering = true },' \
  '})' \
  'hl.window_rule({ match = { class = ".*" }, monitor = "preview" })' > "$root/hyprland.lua"
unset HYPRLAND_INSTANCE_SIGNATURE
/usr/bin/setsid /usr/bin/env WAYLAND_DISPLAY="$parent_socket" /usr/bin/Hyprland --config "$root/hyprland.lua" > "$root/hyprland.log" 2>&1 &
hypr_pid=$!
signature=""
display=""
for _ in {1..120}; do
  kill -0 "$hypr_pid" 2>/dev/null || { echo "preview: nested Hyprland exited during startup" >&2; sed -n '1,80p' "$root/hyprland.log" >&2; exit 1; }
  for instance in "$XDG_RUNTIME_DIR"/hypr/*; do
    [[ -S $instance/.socket.sock && -f $instance/hyprland.lock ]] || continue
    candidate=$(sed -n '2p' "$instance/hyprland.lock")
    [[ -n $candidate && -S $XDG_RUNTIME_DIR/$candidate ]] || continue
    signature=$(basename "$instance")
    display=$candidate
    break 2
  done
  sleep 0.1
done
[[ -n $signature ]] || { echo "preview: nested Hyprland did not publish its sockets" >&2; exit 1; }
export HYPRLAND_INSTANCE_SIGNATURE=$signature WAYLAND_DISPLAY=$display

# The nested compositor's own output is a window on the desktop this runs from, as large as that desktop tiles it.
# The app goes on a headless output instead, whose size is the monitor rule's; the window rule sends it there.
hyprctl output create headless preview >/dev/null

GDK_BACKEND=wayland "$binary" > "$root/app.log" 2>&1 &
app_pid=$!
on_output() {
  local output
  output=$(hyprctl monitors -j | jq 'first(.[] | select(.name == "preview") | .id)') &&
    hyprctl clients -j | jq -e --argjson output "${output:-null}" 'any(.[]; .mapped and .monitor == $output)' >/dev/null
}
for _ in {1..100}; do
  on_output && break
  kill -0 "$app_pid" 2>/dev/null || break
  sleep 0.1
done

# Once the window is drawn and its first week has loaded. The nested compositor's keyboard is the app's: it is the
# only client there.
sleep 2
for key in $keys; do
  case $key in
    ?) wtype "$key" ;;
    *+*) wtype -M "${key%%+*}" -k "${key#*+}" -m "${key%%+*}" ;;
    *) wtype -k "$key" ;;
  esac
  # A week's events are read again after each step back.
  sleep 0.6
done
sleep "$settle"
kill -0 "$app_pid" 2>/dev/null || { echo "preview: the app exited" >&2; sed -n '1,40p' "$root/app.log" >&2; exit 1; }
# A picture of an output the window is not on would be blank, and look like a result.
on_output || { echo "preview: the app's window is not on the output being captured" >&2; hyprctl clients >&2; exit 1; }
mkdir -p "$(dirname "$screenshot")"
# Hyprland's own notices (it says it was started without its wrapper) would cover the toolbar.
hyprctl dismissnotify >/dev/null
/usr/bin/grim -o preview "$screenshot"
echo "preview: $screenshot"
[[ ! -s $root/app.log ]] || sed -n '1,20p' "$root/app.log" >&2
