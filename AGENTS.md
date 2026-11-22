# cloud-calendar

iCloud, Google and HEY calendars merged on Omarchy/Arch: a GTK4 app, an agent-friendly CLI and
event notifications through `omarchy-notification-send`. Calendar sibling of cloud-mail.

## Code index

- `crates/cloud-calendar-api` — the library both front ends share. One `Provider` trait
  (`provider.rs`, opened by `provider::open`) with `icloud.rs` (icloud.com's calendar service via
  the shared `icloud-session` client), `google.rs` (shells out to `gws`; holds the OAuth client
  ID/secret) and `hey.rs` (shells out to `hey`); `unified.rs` merges accounts, so one failing
  account becomes a warning, not an error; `accounts.rs` links/removes accounts; `notify.rs` is
  the notification cache and sender; `config.rs` reads `~/.config/cloud-calendar/config.toml`;
  `time.rs` parses dates (`tomorrow 15:00`, `+N`, RFC 3339); `error.rs` gives each error kind its
  stable `code`.
- `crates/cloud-calendar` — `cloud-calendar` CLI (clap, `src/cli.rs`; output modes in
  `src/main.rs`, text rendering in `src/render.rs`). `tests/cli.rs` runs the real binary against
  fakes (`tests/fakes/`, `tests/fake-{gws,hey,notify}`).
- `crates/cloud-calendar-gtk` — GTK4 app (`ui.rs` week/agenda view, the shortcut overlay and the keys, `editor.rs`,
  `accounts.rs`; every job that writes goes through `work.rs`, on `omui::background`, which holds the application
  until it is done). Every visible string goes through omui's `tr`/`tr!`, is listed in `src/i18n/en.json` (the
  catalog test checks it), and follows a language switch live (`i18n::bind*`, `ui::Live`, `Ui::redraw`). Its one
  name is `cloud-calendar`: omui sets the Wayland app id to it (`omui::application`'s short name, and
  `omui::name_window` for the editor and Accounts, which are kept out of the application); the GApplication id,
  which needs a dot, stays `com.ferdousbhai.CloudCalendar`; `cloud-calendar open` execs the binary, which the package installs as
  `/usr/lib/cloud-calendar/cloud-calendar-gtk`. The
  look, the Omarchy theme, the icons and the shared widgets are omui's (git dependency pinned by
  `rev` in its `Cargo.toml`); `style.css` holds only the calendar's own rules, on omui's `@om_*`
  colours and `--om-radius*`. `data/` holds `cloud-calendar.desktop` and `cloud-calendar.svg`.
- Notification text (`notify.rs`) is translated by the library's own small reader of the same catalogs
  (`cloud-calendar-api/src/i18n.rs`, `src/i18n/en.json`), since the CLI links no GTK; the app's tests run omui's
  catalog check over it.
- `packaging/aur/cloud-calendar/PKGBUILD` — the package (the CLI, the window under `/usr/lib/cloud-calendar/`,
  systemd user units, and `packaging/compat/cloud-calendar-gtk`, the old command kept for bindings and older
  notifications).
  `packaging/systemd/` — `cloud-calendar-notify.{service,timer}` (runs `cloud-calendar notify`
  every minute). `packaging/repo/` — the signed-repo `install.sh` and its tests.
- `bin/build-package <arch>` — native, unsigned package build; `bin/arch-run` wraps it in a disposable Arch chroot for CI;
  `bin/release <version>` — maintainer-only signing and publishing; `bin/verify-release`.

## Commands

```sh
bin/check                        # rustfmt, clippy -D warnings, tests, app QA, installer tests; .githooks/pre-push and CI (check.yml) run it
CHECK_NO_GTK=1 bin/check         # without gtk4: the CLI and the library alone (otherwise missing gtk4 fails it)
git config core.hooksPath .githooks   # once per clone (NO_CHECK=1 skips the hook)
cargo build --release            # target/release/cloud-calendar and cloud-calendar-gtk (what `cloud-calendar open` runs)
dev/preview.sh week /tmp/week.png  # the app on the fakes in a nested Hyprland, as a screenshot
cargo build -p cloud-calendar-gtk --features qa && dev/qa.sh target/debug/cloud-calendar-gtk dev/qa/*.jsonl  # in bin/check
bash packaging/repo/test.sh && python3 packaging/repo/test_arch.py   # installer tests (in bin/check)
bin/build-package "$(uname -m)"  # full package build as a regular Arch user
sudo bin/arch-run bin/build-package "$(uname -m)" /output  # disposable native Arch chroot
```

CLI tests need `dbus-daemon`; they never touch a real account, keyring, bus or desktop. The fakes
are selected with `CLOUD_CALENDAR_{HEY,GWS,NOTIFY}_COMMAND`. Look at the app only through
`dev/preview.sh` (its header lists the scenes and themes): it has a private HOME and D-Bus, so no
window opens on the desktop and no real account is read. `dev/qa/*.jsonl` drive the app on the same
fakes through omui's scripted QA (feature `qa`, which release builds and the PKGBUILD leave off):
`dev/qa.sh` runs them on a private broadway display with no session bus, and any GTK warning fails a
run. Widgets are found by accessible name, so two that look alike get their own
(`Property::Label`). A script line `@gws <count> <pattern>` checks what reached the stand-in Google. README.md's
picture is `docs/screenshot.png`, taken by `dev/qa/accounts.jsonl`. `dev/qa/language.jsonl` switches the language with omui's QA
`language` command into `dev/qa/da.json` (a few Danish strings only QA builds carry, registered by `main.rs`) and opens the shortcut overlay. Shots of the main window go to `CLOUD_CALENDAR_QA_SHOTS`; the editor and
Accounts are windows of their own, which `shot` does not take, so their pictures are preview.sh's.

## Conventions and gotchas

- The CLI prints a JSON envelope `{ok, data, summary, meta: {warnings}}` when piped (or `--json`);
  `--quiet`, `--ids-only`, `--count`, `--styled` change that. Exit 0 on success and 1 on any failure
  (Omarchy's convention); a failure goes to stderr only, as `{ok: false, error: {code, message}}`
  unless output is text, and its `code` (`ErrorKind::code`, or `usage`) is what tells failures apart.
  Keep new commands on this contract; destructive commands take `--yes`.
- IDs are prefixed with the account name (`icloud:…`, `google:…`, `hey:…`); a new provider must
  do the same and be added to `provider::open`.
- No secrets of its own: iCloud uses icloud-session (git dependency pinned by `rev` in the root
  `Cargo.toml`, from icloud-for-omarchy), Google uses `gws` with state in
  `~/.config/cloud-calendar/gws/<name>`, HEY uses `hey` and the keyring. The config file holds
  nothing secret.
- Provider limits are deliberate (repeating iCloud events read-only, HEY edits drop countdowns and
  need both start and end); keep the CLI/editor warnings (`edit_side_effects`) in sync with them.
- The app's binary holds omui's Strata-derived code and Lucide icons: a change of the omui `rev`
  keeps `THIRD_PARTY_NOTICES.md` (installed by the PKGBUILD) true.
- Release: bump `version` in the root `Cargo.toml` and `pkgver` in the PKGBUILD together, tag
  `vX.Y.Z`, draft the GitHub release, then `bin/release X.Y.Z` (signing key lives only in the
  maintainer's keyring, never CI). Do not run release scripts unless asked.
