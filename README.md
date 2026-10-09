# Cloud Calendar

Your iCloud, Google and HEY calendars in one place on Omarchy: a fast native GTK app themed from
Omarchy, a CLI that people and AI agents can both drive, and event notifications through Omarchy's
own notification system. It is the calendar sibling of [cloud-mail](https://github.com/ferdousbhai/cloud-mail).

- **One merged view.** Every linked account's events in one week or agenda, each tagged with its
  account and calendar. If one account is signed out or offline, the others still load, and one line
  says what's wrong.
- **Read and write.** Add, change and delete events in any calendar you can write to.
- **No separate iCloud sign-in.** iCloud uses the sign-in [icloud-session](https://github.com/ferdousbhai/icloud-for-omarchy)
  already keeps for your other iCloud apps.
- **No passwords of its own.** Every sign-in belongs to icloud-session, `gws` or `hey`; see
  [Where sign-ins live](#where-sign-ins-live).
- **Notifications** a few minutes before each timed event, through `omarchy-notification-send`;
  clicking one opens the app.

| Path | What |
|---|---|
| `crates/cloud-calendar-api` | Providers (`icloud.rs`, `google.rs`, `hey.rs`), the merged view (`unified.rs`), accounts (`accounts.rs`), notifications (`notify.rs`), config |
| `crates/cloud-calendar` | `cloud-calendar` CLI |
| `crates/cloud-calendar-gtk` | `cloud-calendar-gtk` desktop app (GTK4) |
| `packaging/` | PKGBUILD (x86_64 and aarch64) and the notification timer's systemd user units |

## Install

On Omarchy or Arch Linux (x86_64 and aarch64), from the signed package repository once a release
is published:

```sh
curl -fsSL https://github.com/ferdousbhai/cloud-calendar/releases/latest/download/install.sh | sudo bash
systemctl --user enable --now cloud-calendar-notify.timer   # event notifications
```

The installer trusts the package-signing key (pinned by fingerprint, the same key cloud-mail and
icloud-for-omarchy use), adds the `[cloud-calendar]` repository (`[cloud-calendar-aarch64]` on ARM)
and installs the package; updates then arrive with `omarchy update`. Before the first release,
`cargo build --release` in a checkout builds `target/release/cloud-calendar` and
`target/release/cloud-calendar-gtk`. iCloud calendars need icloud-session (from
icloud-for-omarchy) installed; adding iCloud without it says so.

## Link your calendars

In the app, open **Accounts** and choose iCloud, Google or HEY; sign in again or remove an account
there too. From a terminal, `cloud-calendar account add icloud|google|hey` does the same.

Each account gets a short name (its provider's, unless you pick another with `--name`), which
starts the IDs of its calendars and events: `icloud:…`, `google:…`, `hey:…`.

### iCloud

```sh
cloud-calendar account add icloud
```

iCloud calendars come from icloud.com's own calendar service, signed in through icloud-session.
If you're already signed in to iCloud for Notes, Photos or Find My, linking is instant; if not,
icloud-session opens its iCloud sign-in window. Removing the account leaves icloud-session signed in,
since your other apps share it. There is one iCloud account: the one icloud-session holds.

Repeating iCloud events are shown but can't be changed or deleted here yet: how icloud.com changes a
whole series isn't established well enough to rely on. Use the Calendar app or icloud.com for those.

### Google

Google Calendar goes through Google's own [Workspace CLI `gws`](https://github.com/googleworkspace/cli),
the way cloud-mail reaches Gmail, with one browser sign-in and no Google Cloud setup of your own:
Cloud Calendar brings its own Google sign-in. (Not yet in this build: until the built-in client is
filled in, adding Google says sign-in isn't configured; see [Google sign-in](#google-sign-in-maintainers).)

```sh
npm install -g @googleworkspace/cli
cloud-calendar account add google      # opens Google's sign-in in your browser, once
```

The sign-in asks for your events and your calendar list only. Calendars you've hidden in Google
Calendar stay hidden here. `gws` keeps the sign-in in Cloud Calendar's own directory
(`~/.config/cloud-calendar/gws/<name>`), apart from any `gws` you use yourself.

### HEY

HEY Calendar goes through HEY's official [`hey` CLI](https://github.com/basecamp/hey-cli) (1.7 or
newer), which signs in with one browser login and keeps the sign-in in your keyring.

```sh
cloud-calendar account add hey
```

HEY reads by week, over the calendars switched on in HEY. Moving a HEY event needs both its new
start and its new end, because the CLI can't read one event back to keep its length. Events can't
be added to HEY's personal calendar (HEY refuses them there), so it shows as read-only.

Editing a HEY event, even just its title, removes its countdown, flattens its notes' formatting and
detaches an attached email you can't read: HEY's CLI resends the whole event, and HEY serves notes
back as plain text and countdowns and such emails not at all. The editor says so before you save, and `cloud-calendar event edit` adds an
`edit_side_effects` warning to its output.

### Where sign-ins live

| Account | Sign-in | Kept in |
|---|---|---|
| iCloud | icloud-session's | icloud-session's keyring items |
| Google | `gws`'s | `~/.config/cloud-calendar/gws/<name>/credentials.enc`, encrypted with a key in a file beside it: gws can't keep that key only in the keyring on Linux |
| HEY | `hey`'s | the keyring; linking refuses a `hey` that fell back to its plain file |

### Accounts

```sh
cloud-calendar account list            # which accounts are linked and working
cloud-calendar account login icloud    # sign in again
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
cloud-calendar agenda --from tomorrow --days 14   # 1 to 366 days
cloud-calendar calendars               # calendar IDs, and which are read-only

cloud-calendar event add --calendar icloud:home --title "Dentist" \
  --start "2026-10-14 09:00" --length 45m --location "Main St"
cloud-calendar event add --calendar hey:11 --title "Away" --start 2026-10-20 --end 2026-10-23   # all-day, 20th to 22nd
cloud-calendar event edit <id> --start "tomorrow 15:00"      # keeps the length
cloud-calendar event edit <id> --location ""                 # clears it
cloud-calendar event delete <id> --yes
```

A date alone (`2026-10-20`) means all-day, and an all-day `--end` is exclusive: `--start 2026-10-20
--end 2026-10-23` covers the 20th, 21st and 22nd. Times are local: `2026-10-14 09:00`,
`tomorrow 15:00`, `14:00` (today), or RFC 3339. For a Google repeating event, changing or deleting
one occurrence changes the whole series; its time can only change in Google. A HEY repeating event
can be deleted (the whole series) but not changed here: change it in HEY.

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
with exit codes 2 (invalid request), 3 (not signed in, not configured, or no keyring:
`keyring_unavailable`), 4 (not found) and 5 (an account is unreachable).

## Notifications

```sh
systemctl --user enable --now cloud-calendar-notify.timer
```

Every minute the timer runs `cloud-calendar notify`, which sends a notification through
`omarchy-notification-send` for each timed event starting within your lead time (10 minutes by
default), once. All-day events aren't notified. The next 24 hours of events are cached for 10
minutes, so an event added in the last few minutes may not notify until the cache refreshes. Choose
the lead times in the config:

```toml
# ~/.config/cloud-calendar/config.toml
notify_minutes = [10, 1]
```

## Configuration

`~/.config/cloud-calendar/config.toml`, written when you link an account. Nothing secret is in it:

```toml
notify_minutes = [10]

[accounts.icloud]

[accounts.google]

[accounts.hey]
# account = "<hey linked-account id>"   # default: all
```

A second Google or HEY account is `account add google --name work`.

## Development

```sh
cargo test --workspace && cargo clippy --workspace --all-targets
```

Tests never touch a real account, keyring, bus or desktop. The CLI's end-to-end tests
(`crates/cloud-calendar/tests`) run the real binary on a private `dbus-daemon` carrying a fake
icloud-session and a fake Secret Service, against a fake icloud.com calendar service, with fake
`hey`, `gws` and notification commands selected by `CLOUD_CALENDAR_HEY_COMMAND`,
`CLOUD_CALENDAR_GWS_COMMAND` and `CLOUD_CALENDAR_NOTIFY_COMMAND`. They need `dbus-daemon`. A new
provider implements `Provider` in `provider.rs`, prefixes its IDs with its account name, and is
added to `provider::open`.

### Google sign-in (maintainers)

Cloud Calendar's Google sign-in is one OAuth client, `GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET` in
`crates/cloud-calendar-api/src/google.rs` (a desktop client's secret isn't secret). It is meant to
be cloud-mail's "Desktop app" client, in cloud-mail's Google Cloud project, which has:

- the **Google Calendar API** enabled (done);
- an OAuth consent screen published "In production" (test-mode sign-ins expire after 7 days).

The calendar scopes (`https://www.googleapis.com/auth/calendar.events` and
`https://www.googleapis.com/auth/calendar.calendarlist.readonly`) aren't listed on the consent
screen. Sign-in works without that, with the same "Google hasn't verified this app" warning as
Gmail's; listing them only matters for Google's app verification.

The constants are empty in this repository, so adding Google currently says sign-in isn't
configured. Fill them with cloud-mail's client (`crates/cloudmail-api/src/gmail.rs`) to turn it on.
A build can also use its own client with `CLOUD_CALENDAR_GOOGLE_CLIENT_ID` /
`CLOUD_CALENDAR_GOOGLE_CLIENT_SECRET`.

### Releasing (maintainers)

Native CI (`.github/workflows/packages.yml`) builds and tests the package on x86_64 and aarch64
runners for every push and pull request. To release version X.Y.Z, bump `version` in `Cargo.toml`
and `pkgver` in `packaging/aur/cloud-calendar/PKGBUILD`, commit, then:

```sh
git tag -a vX.Y.Z -m "Cloud Calendar X.Y.Z" && git push origin main vX.Y.Z
gh release create vX.Y.Z --draft --verify-tag --generate-notes
bin/release X.Y.Z
```

`bin/release` waits for the tag's CI run, checks the artifacts' source commit and metadata, signs
both packages and repository databases with the package-signing key (which must be in the local
keyring; it never enters CI) and publishes the release. `verify-release.yml` then installs it from
the one-liner on both architectures.

## License

MIT
