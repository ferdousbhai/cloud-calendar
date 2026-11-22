#!/usr/bin/env bash
set -Eeuo pipefail

# Runs the app, built with feature `qa`, through omui's scripted QA: each script's JSON lines go to it on stdin, and
# its answers come back on descriptor 3. It runs on a broadway display of its own with a private HOME and XDG
# directories and no session bus, on the stand-in accounts dev/preview.sh uses (the tests' fake `hey` and `gws`, and an
# iCloud account that fails): no window opens anywhere visible, and no real account is read. The protocol is in
# omui's README. bin/check runs every script under dev/qa.
#
#   dev/qa.sh <app-binary> <script.jsonl>...
#   CLOUD_CALENDAR_QA_SHOTS=<dir>  where a script's `shot` paths are written (default: a scratch directory, removed)
#   OMUI_QA_TIMEOUT=120            seconds before one script's run is stopped
#
# The fakes' events are in the week of 2026-10-12, and the app opens on today. A script line that is `@to-fakes` goes
# back that many weeks with the app's `h` key. A line `@gws <count> <pattern>` is checked once the app has ended: the
# stand-in `gws` must have been run that many times with arguments matching the pattern (an extended regular
# expression over its tab-separated arguments), which is how a script tells what was sent to the account from what
# the window shows.
(( $# >= 2 )) || { echo "usage: dev/qa.sh <app-binary> <script.jsonl>..." >&2; exit 2; }
app=$(realpath -- "$1")
shift
command -v gtk4-broadwayd >/dev/null || { echo "qa: gtk4-broadwayd is required" >&2; exit 1; }
fakes=$(realpath -- "$(dirname -- "${BASH_SOURCE[0]}")/../crates/cloud-calendar/tests")

days=$(( ( $(date -u +%s) - $(date -u -d 2026-10-12 +%s) ) / 86400 ))
(( days >= 0 )) || { echo "qa: the clock is before the fakes' week" >&2; exit 1; }

root=$(mktemp -d /tmp/cloud-calendar-qa.XXXXXX)
broadway_pid=""
app_pid=""
cleanup() {
  local status=$?
  # Only the app and the display this script started.
  [[ -z $app_pid ]] || kill -TERM "$app_pid" 2>/dev/null || true
  [[ -z $app_pid ]] || wait "$app_pid" 2>/dev/null || true
  [[ -z $broadway_pid ]] || kill -TERM "$broadway_pid" 2>/dev/null || true
  [[ -z $broadway_pid ]] || wait "$broadway_pid" 2>/dev/null || true
  rm -rf -- "$root"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
shots=${CLOUD_CALENDAR_QA_SHOTS:-$root/shots}
mkdir -p "$root/runtime" "$shots"
shots=$(realpath -- "$shots")
chmod 700 "$root/runtime"

# A display number and input port from this script's PID, so that two runs at once do not share one. broadwayd
# listens on that TCP port on 127.0.0.1 alone, where the app sends key presses (`key`, `type`) as broadway's page would.
display=$(( 100 + $$ % 800 ))
port=$(( 8080 + display ))
XDG_RUNTIME_DIR=$root/runtime gtk4-broadwayd --address 127.0.0.1 --port "$port" ":$display" \
  >"$root/broadwayd.log" 2>&1 &
broadway_pid=$!
for _ in {1..50}; do
  [[ -S $root/runtime/broadway$((display + 1)).socket ]] && break
  kill -0 "$broadway_pid" 2>/dev/null || { cat "$root/broadwayd.log" >&2; exit 1; }
  sleep 0.1
done

failed=0
for script in "$@"; do
  name=$(basename -- "$script" .jsonl)
  run=$root/$name
  # Each script starts from the same accounts, in a home of its own.
  rm -rf -- "$run"
  mkdir -p "$run"/{home,config/cloud-calendar/gws/google,state,data,cache}
  printf '[accounts.%s]\n\n' google hey icloud > "$run/config/cloud-calendar/config.toml"
  : > "$run/config/cloud-calendar/gws/google/credentials.enc"
  while IFS= read -r line; do
    if [[ $line == '@to-fakes' ]]; then
      for (( n = 0; n < days / 7; n++ )); do printf '%s\n' '{"do":"key","keys":"h"}'; done
    elif [[ $line == '@gws '* ]]; then
      printf '%s\n' "${line#@gws }" >> "$run/gws.expected"
    else
      printf '%s\n' "$line"
    fi
  done < "$script" > "$run/script.jsonl"

  echo "==> qa: $name"
  # In the background, waited for: bash runs a trap only once the foreground command ends, and `timeout` puts the app
  # in a process group of its own, which Ctrl-C does not reach. Killing `timeout` stops the app with it.
  status=0
  (
    cd "$shots"
    exec env -u WAYLAND_DISPLAY -u DISPLAY -u GTK_THEME -u HYPRLAND_INSTANCE_SIGNATURE \
      DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent \
      HOME="$run/home" XDG_CONFIG_HOME="$run/config" XDG_STATE_HOME="$run/state" XDG_DATA_HOME="$run/data" \
      XDG_CACHE_HOME="$run/cache" XDG_RUNTIME_DIR="$root/runtime" LC_ALL=C TZ=UTC \
      CLOUD_CALENDAR_HEY_COMMAND="$fakes/fake-hey" CLOUD_CALENDAR_GWS_COMMAND="$fakes/fake-gws" \
      CLOUD_CALENDAR_NOTIFY_COMMAND="$fakes/fake-notify" CLOUD_CALENDAR_BROWSER=true FAKE_GWS_SECRET=qa \
      FAKE_GWS_LOG="$run/gws.log" \
      CLOUD_CALENDAR_GOOGLE_CLIENT_ID=qa CLOUD_CALENDAR_GOOGLE_CLIENT_SECRET=qa \
      GDK_BACKEND=broadway BROADWAY_DISPLAY=":$display" OMUI_QA=1 OMUI_QA_INPUT="127.0.0.1:$port" OMUI_QA_FD=3 \
      timeout "${OMUI_QA_TIMEOUT:-120}" "$app" <"$run/script.jsonl" 3>"$run/answers"
  ) &
  app_pid=$!
  wait "$app_pid" || status=$?
  app_pid=""

  # Each command beside its answer; `quit`'s answer, or the failure that ended the run, comes last.
  grep -Ev '^[[:space:]]*(#|$)' "$run/script.jsonl" | awk -v answers="$run/answers" '
    { if ((getline answer < answers) > 0) { print "> " $0; print "< " answer } else { print "> " $0 " (not run)" } }
    END { while ((getline answer < answers) > 0) print "< " answer }'
  if [[ -f $run/gws.expected ]]; then
    while read -r count pattern; do
      sent=$(grep -Ec -- "$pattern" "$run/gws.log" 2>/dev/null || true)
      if (( sent != count )); then
        echo "qa: $name: gws ran $sent times matching '$pattern', not $count"
        status=1
      fi
    done < "$run/gws.expected"
  fi
  if (( status == 0 )); then
    echo "qa: $name passed"
  else
    echo "qa: $name failed with status $status"
    failed=1
  fi
done
exit "$failed"
