# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with this repository.

## What this is

MC Switch (internal crate names are `pitstop-*`; a fork of Piaz Switch): a tray/menu-bar app plus `mc-switch` CLI that swaps between multiple Claude Code, Codex, Cursor and Antigravity logins and shows each account's usage limits. No server, no telemetry, no updater. Windows and macOS are the supported targets.

## Commands

CI (Windows + macOS) runs exactly these; run them before a PR:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p pitstop-core <name>            # a single test (substring filter)
cargo test -p pitstop-core real_claude -- --ignored   # real sign-in tests, opt-in only
```

```sh
cargo build -p pitstop-app
MC_SWITCH_HOME=/tmp/sandbox cargo run -p pitstop-app   # ALWAYS use a sandbox home; the default touches real logins
cargo run -p pitstop-cli -- --home /tmp/sandbox list # CLI equivalent (--home)
cargo run -p pitstop-core --example update-prices    # refresh the shipped crates/pitstop-core/src/prices.json
```

The window UI is plain JS in `crates/pitstop-app/ui/` (no bundler, no dev server: `custom-protocol` loads the bundled files). Only the CSS is built: edit `ui-src/app.css`, then `npm install && npm run build` in `crates/pitstop-app/ui-src`, and commit the built `ui/app.css`. Dark theme rules use `:root[data-theme="dark"]`.

## Architecture

Three crates: `pitstop-core` (all logic) ← `pitstop-cli` and `pitstop-app` (Tauri 2, thin shells over core).

**`Host` trait (`pitstop-core/src/host.rs`) is the seam.** Every touch of the machine (files, atomic writes, OS keychain, keystore, editor `state.vscdb`, "is the editor running") goes through it. `RealHost` is production; `testing::FakeHost` is an in-memory fake so the whole park/switch flow is tested without touching disk. New machine access must be added to the trait, not called directly.

**Engine + providers.** `Engine` (`engine.rs`) implements park/switch/enroll/rename/remove over a `Host`. Each tool is a `provider/*` module (`claude_code`, `codex`, `cursor`, `antigravity`) keyed by `ProviderId`, owning: where the live credential lives, how to read/write it, account identity (email + org/workspace, not just email), and its usage endpoint. A switch parks the live login under the account it belongs to, then installs the chosen one; writes are atomic and a failed switch rolls back. Parked logins are AES-256-GCM encrypted in `~/.mc-switch/vault/` (`vault.rs`), key in the OS keystore (`store.rs`, Windows only).

**Editors (Cursor/Antigravity)** keep logins in `state.vscdb` (SQLite, `vscdb.rs`), read only at editor startup, so a switch must quit the editor first and never writes while it's open (`editor.rs`). Sign-in flows: `cursor_login.rs` (browser deep flow), Antigravity runs its IDE in a temp data dir (`signin.rs`). Never touch the Antigravity 2 app / `agy` (keyring-based, unsupported).

**Usage.** `usage.rs` / `live.rs` fetch per-account limits; tokens are never refreshed for usage (rotating a refresh token would break the other holder). `ledger.rs` + `pricing.rs` read local session logs (`~/.claude/projects`, `~/.codex/sessions`) and price them with the LiteLLM table.

**App (`pitstop-app/src`).** `commands.rs` are the Tauri commands the UI calls; `model.rs` builds the `Snapshot` sent to `ui/`; `tray.rs` the menu; `duty.rs`/`warm.rs` auto-switch and warm-up; `signin.rs` sign-in jobs; `i18n.rs` + `ui/i18n.js` English/Vietnamese (add strings to both).

## Conventions

- Conventional Commits (`feat(app): …`, `fix(core): …`); user-visible changes need a line in `CHANGELOG.md`.
- Release: bump `version` in the root `Cargo.toml` (app takes its version from there), date the CHANGELOG section, push tag `vX.Y.Z`. There is deliberately no auto-updater.
- Never log or paste tokens, credential files, or anything from `~/.mc-switch/vault/`.
- Unix-only permission tests exist; macOS-specific behavior (Keychain item `Claude Code-credentials`) is checked via the checklist in `CONTRIBUTING.md`.
