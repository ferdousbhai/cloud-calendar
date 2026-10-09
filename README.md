# Cloud Calendar

Your iCloud, Google and HEY calendars in one place on Linux: a fast native GTK app themed from
Omarchy, a CLI that people and AI agents can both drive, and event notifications through Omarchy's
own notification system. It is the calendar sibling of [cloud-mail](https://github.com/ferdousbhai/cloud-mail).

- **One merged view.** Every linked account's events in one week or agenda, each tagged with its
  account and calendar. If one account is signed out or offline, the others still load, and one line
  says what's wrong.
- **Read and write.** Add, change and delete events in any calendar you can write to.
- **Repeating events** show every occurrence. Changing or deleting one occurrence changes the whole
  series; moving its time is left to the calendar it came from, so nothing moves by accident.
- **Notifications** a few minutes before each timed event, through `omarchy-notification-send`
  (or `notify-send` elsewhere). On Omarchy, clicking one opens the app.
- **No passwords in files.** iCloud's app-specific password lives in your keyring; Google and HEY
  sign-ins are kept by their own CLIs.

| Path | What |
|---|---|
| `crates/cloud-calendar-api` | Providers (`caldav.rs` iCloud, `google.rs`, `hey.rs`), the merged view (`unified.rs`), iCalendar (`ics.rs`), notifications (`notify.rs`), config and keyring |
| `crates/cloud-calendar` | `cloud-calendar` CLI |
| `crates/cloud-calendar-gtk` | `cloud-calendar-gtk` desktop app (GTK4) |
| `packaging/` | PKGBUILD (x86_64 and aarch64) and the notification timer's systemd user units |

## Install

On Omarchy or Arch Linux (x86_64 or aarch64), from a checkout:

```sh
cd packaging/aur/cloud-calendar && makepkg -si
```

The PKGBUILD builds a release tag. Until there is one, `cargo build --release` in the checkout builds
`target/release/cloud-calendar` and `target/release/cloud-calendar-gtk`.

## Link your calendars

Each account gets a short name (its provider's, unless you pick another with `--name`), which
starts the IDs of its calendars and events: `icloud:…`, `google:…`, `hey:…`.

### iCloud

iCloud calendars use CalDAV, signed in with an **app-specific password**: make one at
[account.apple.com](https://account.apple.com) under Sign-In and Security → App-Specific Passwords.

```sh
cloud-calendar account add icloud --username you@icloud.com   # asks for the app-specific password
```

The password is checked with Apple before anything is saved, then stored in your keyring (the Secret
Service: GNOME Keyring, KWallet, KeePassXC…) through `secret-tool`. The config file holds only your
email. Advanced Data Protection doesn't cover calendars, so this works with it on. Your Reminders
lists are left out; only event calendars show.

### Google

Google Calendar goes through Google's own [Workspace CLI `gws`](https://github.com/googleworkspace/cli),
the way cloud-mail reaches Gmail. `gws` keeps the sign-in in Cloud Calendar's own directory
(`~/.config/cloud-calendar/gws/<name>`), separate from any `gws` you use yourself.

```sh
npm install -g @googleworkspace/cli
cloud-calendar account add google --client-id <id> --client-secret <secret>
```

The sign-in needs a Google OAuth client: a "Desktop app" client in a Google Cloud project with the
Google Calendar API enabled. Cloud Calendar doesn't include one yet. Pass it with `--client-id` and
`--client-secret`, or set `CLOUD_CALENDAR_GOOGLE_CLIENT_ID` and `CLOUD_CALENDAR_GOOGLE_CLIENT_SECRET`.
The sign-in asks for your events and your calendar list only. Calendars you've hidden in Google
Calendar stay hidden here.

### HEY

HEY Calendar goes through HEY's official [`hey` CLI](https://github.com/basecamp/hey-cli) (1.7 or
newer), which signs in with one browser login. Cloud Calendar never sees a HEY token.

```sh
cloud-calendar account add hey     # runs `hey auth login` if you aren't signed in
```

HEY reads by week, over the calendars switched on in HEY. Moving a HEY event needs both its new
start and its new end, because the CLI can't read one event back to keep its length.

### Accounts

```sh
cloud-calendar account list            # which accounts are linked and working
cloud-calendar account login icloud    # sign in again (a new app-specific password, or the browser)
cloud-calendar account remove google --yes
```

## Use it

Open **Cloud Calendar** from the app launcher. `h`/`l` (or ←/→) move a week, `t` goes to today,
`a` switches between the week and a two-week agenda, `n` adds an event, `r` reloads. Click an event
to change or delete it, or a day's heading to add an event on that day.

To open it with `SUPER + SHIFT + C`, which Omarchy binds to HEY's web calendar by default, add this
to `~/.config/hypr/bindings.lua`:

```lua
hl.unbind("SUPER + SHIFT + C")
o.bind("SUPER + SHIFT + C", "Calendar", "cloud-calendar-gtk")
```

From the terminal:

```sh
cloud-calendar today
cloud-calendar week                    # this week, Monday to Sunday; or `week 2026-10-20`
cloud-calendar agenda --from tomorrow --days 14
cloud-calendar calendars               # calendar IDs, and which are read-only

cloud-calendar event add --calendar icloud:/123/calendars/home/ --title "Dentist" \
  --start "2026-10-14 09:00" --length 45m --location "Main St"
cloud-calendar event add --calendar hey:11 --title "Away" --start 2026-10-20 --end 2026-10-23   # all-day, 20th to 22nd
cloud-calendar event edit <id> --start "tomorrow 15:00"      # keeps the length
cloud-calendar event edit <id> --location ""                 # clears it
cloud-calendar event delete <id> --yes
```

A date alone (`2026-10-20`) means all-day, and an all-day `--end` is exclusive: `--start 2026-10-20
--end 2026-10-23` covers the 20th, 21st and 22nd. Times are local: `2026-10-14 09:00`,
`tomorrow 15:00`, `14:00` (today), or RFC 3339.

### For agents

Piped output is a JSON envelope:

```json
{"ok": true, "data": [...], "summary": "6 events in the week of 2026-10-12", "meta": {"warnings": []}}
```

`--quiet` prints only `data`, `--ids-only` one ID per line, `--count` the number of results, and
`--styled` forces text. Each event has `id`, `account`, `calendar_id`, `calendar`, `title`, `start`,
`end` (local RFC 3339, or `YYYY-MM-DD` for all-day with an exclusive end), `all_day`, `location`,
`notes` and `recurring`. An account that failed shows up in `meta.warnings` (`account`, `code`,
`message`) and doesn't fail the command. Errors are `{"ok": false, "error": {"code", "message"}}`
with exit codes 2 (invalid request), 3 (not signed in or not configured), 4 (not found) and
5 (an account is unreachable).

## Notifications

```sh
systemctl --user enable --now cloud-calendar-notify.timer
```

Every minute the timer runs `cloud-calendar notify`, which sends a notification for each timed event
starting within your lead time (10 minutes by default), once. All-day events aren't notified. The
next 24 hours of events are cached for 10 minutes, so an event added in the last few minutes may
not notify until the cache refreshes. Choose the lead times in the config:

```toml
# ~/.config/cloud-calendar/config.toml
notify_minutes = [10, 1]
```

## Configuration

`~/.config/cloud-calendar/config.toml`, written by `account add`:

```toml
notify_minutes = [10]

[accounts.icloud]
username = "you@icloud.com"

[accounts.google]
client_id = "…apps.googleusercontent.com"
client_secret = "…"

[accounts.hey]
# account = "<hey linked-account id>"   # default: all
```

A second account of the same kind is `account add google --name work`.

## Development

```sh
cargo test --workspace && cargo clippy --workspace --all-targets
```

Tests never touch a real account, keyring or desktop. The CLI's end-to-end tests run the real binary
against an in-process mock CalDAV server and fake commands in `crates/cloud-calendar/tests`:
`fake-hey`, `fake-gws`, `fake-secret-tool` and `fake-notify`, selected with
`CLOUD_CALENDAR_HEY_COMMAND`, `CLOUD_CALENDAR_GWS_COMMAND`, `CLOUD_CALENDAR_SECRET_TOOL` and
`CLOUD_CALENDAR_NOTIFY_COMMAND`. A new provider implements `Provider` in `provider.rs`, prefixes its
IDs with its account name, and is added to `provider::open`.

## License

MIT
