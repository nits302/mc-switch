# MC Switch

Switch between your **Claude Code**, **Codex**, **Cursor**, **Antigravity** and **OpenCode**
accounts in one click — without signing out — and see how much each account has left.

A tray / menu bar app (Windows, macOS) plus an `mc-switch` command line.

- **Switch accounts instantly** from the tray menu; open Claude Code sessions follow.
- **See every account's limits**: 5-hour, weekly or monthly, per model, reset times
  and pace, plus an **All accounts together** row per tool.
- **Switch automatically** to the account with the most room left, and **warm up**
  an account's next 5-hour window (Claude Code and Codex).
- **See what you used and what it would cost** at API prices, from the logs on your
  own computer.
- **Private**: no server, no telemetry, no auto-updater. Parked logins are
  AES-256-GCM encrypted in `~/.mc-switch/vault/`, with the key in the OS keystore
  (Windows Credential Manager / macOS Keychain, entry `mc-switch`). Each login only
  ever goes to the service that issued it.

## How it works

| Tool | Active credential location | Switch behavior |
|---|---|---|
| Claude Code (Windows/Linux) | `~/.claude/.credentials.json` + `oauthAccount` in `~/.claude.json` | immediate, open sessions follow |
| Claude Code (macOS) | Keychain item `Claude Code-credentials` + `oauthAccount` in `~/.claude.json` | same |
| Codex CLI | `~/.codex/auth.json` | a running Codex keeps the old account until restarted |
| Cursor | `cursorAuth/*` rows of `<app data>/Cursor/User/globalStorage/state.vscdb` | MC Switch asks, quits Cursor, switches and reopens it |
| Antigravity IDE | `antigravityUnifiedStateSync.*` rows of `<app data>/Antigravity IDE/User/globalStorage/state.vscdb` | same as Cursor |
| OpenCode | `~/.local/share/opencode/auth.json` (all its services at once) | restart a running OpenCode; add accounts by saving the login it holds; no usage limits shown |

`<app data>` is `%APPDATA%` on Windows and `~/Library/Application Support` on macOS.

A switch parks the live login under the account it belongs to, then puts the chosen
account's parked login where the tool reads it. Every write is atomic, and a failed
switch rolls back.

**Add accounts with Add Account** (or `mc-switch enroll --sign-in`), which runs the
tool's own sign-in in a private, empty directory. Don't use Claude Code's `/login`
or `codex login` to add accounts, and don't log out in the tools: both replace or
end the login MC Switch parked.

Usage limits come from each service's own usage endpoint, using the account's
current token; tokens are never refreshed just to read usage. Cost estimates price
the local session logs with [LiteLLM's public price table](https://github.com/BerriAI/litellm/blob/main/model_prices_and_context_window.json).

## Build from source

Needs [Rust](https://rustup.rs) (stable); on Windows, the MSVC build tools.

```sh
cargo build --release -p pitstop-app   # the app: target/release/pitstop-app
cargo build --release -p pitstop-cli   # command line: target/release/mc-switch
```

On macOS: `crates/pitstop-app/macos/bundle.sh target/release/pitstop-app` wraps it
into `MC Switch.app`.

Set `MC_SWITCH_HOME=/some/dir` to run the app against a sandbox home instead of your
real accounts (the command line has `--home`).

## Command line

```sh
mc-switch enroll work --provider claude-code                 # park the login the tool is using now
mc-switch enroll personal --provider claude-code --sign-in   # sign in to another account
mc-switch                    # list accounts, * marks active, usage under each
mc-switch list --offline     # same, without asking for usage
mc-switch use work           # switch
mc-switch rename work job
mc-switch remove job
```

## License

MIT with the Commons Clause — see [LICENSE](LICENSE). Free to use, modify and share;
**not** to sell. MC Switch is a modified version of
[Piaz Switch](https://github.com/Benpiaz/piaz-switch); the original copyright notice
is kept as the license requires. Third-party marks: [THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES.md).

Not affiliated with Anthropic, OpenAI, Anysphere or Google.
