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
  `time.rs` parses dates (`tomorrow 15:00`, `+N`, RFC 3339); `error.rs` maps error kinds to exit
  codes.
- `crates/cloud-calendar` — `cloud-calendar` CLI (clap, `src/cli.rs`; output modes in
  `src/main.rs`, text rendering in `src/render.rs`). `tests/cli.rs` runs the real binary against
  fakes (`tests/fakes/`, `tests/fake-{gws,hey,notify}`).
- `crates/cloud-calendar-gtk` — GTK4 app (`ui.rs` week/agenda view, `editor.rs`, `accounts.rs`,
  `theme.rs` reads the Omarchy theme); `data/` holds the .desktop file and icon.
- `packaging/aur/cloud-calendar/PKGBUILD` — the package (both binaries + systemd user units).
  `packaging/systemd/` — `cloud-calendar-notify.{service,timer}` (runs `cloud-calendar notify`
  every minute). `packaging/repo/` — the signed-repo `install.sh` and its tests.
- `bin/build-package <arch>` — native, unsigned package build in an Arch container (CI uses it);
  `bin/release <version>` — maintainer-only signing and publishing; `bin/verify-release`.

## Commands

```sh
bin/check                        # rustfmt, clippy -D warnings, tests, installer tests; .githooks/pre-push runs it
git config core.hooksPath .githooks   # once per clone (NO_CHECK=1 skips the hook)
cargo build --release            # target/release/cloud-calendar and cloud-calendar-gtk
bash packaging/repo/test.sh && python3 packaging/repo/test_arch.py   # installer tests (in bin/check)
bin/build-package x86_64         # full package build in Docker, needs a native runner
```

CLI tests need `dbus-daemon`; they never touch a real account, keyring, bus or desktop. The fakes
are selected with `CLOUD_CALENDAR_{HEY,GWS,NOTIFY}_COMMAND`.

## Conventions and gotchas

- The CLI prints a JSON envelope `{ok, data, summary, meta: {warnings}}` when piped (or `--json`);
  `--quiet`, `--ids-only`, `--count`, `--styled` change that. Exit codes: 2 invalid request,
  3 not signed in / not configured / no keyring, 4 not found, 5 account unreachable. Keep new
  commands on this contract; destructive commands take `--yes`.
- IDs are prefixed with the account name (`icloud:…`, `google:…`, `hey:…`); a new provider must
  do the same and be added to `provider::open`.
- No secrets of its own: iCloud uses icloud-session (git dependency pinned by `rev` in the root
  `Cargo.toml`, from icloud-for-omarchy), Google uses `gws` with state in
  `~/.config/cloud-calendar/gws/<name>`, HEY uses `hey` and the keyring. The config file holds
  nothing secret.
- Provider limits are deliberate (repeating iCloud events read-only, HEY edits drop countdowns and
  need both start and end); keep the CLI/editor warnings (`edit_side_effects`) in sync with them.
- Release: bump `version` in the root `Cargo.toml` and `pkgver` in the PKGBUILD together, tag
  `vX.Y.Z`, draft the GitHub release, then `bin/release X.Y.Z` (signing key lives only in the
  maintainer's keyring, never CI). Do not run release scripts unless asked.
