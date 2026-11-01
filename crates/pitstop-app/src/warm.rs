//! A warm-up: Claude Code's own print mode, run once for an account in a
//! directory of its own, so the request starts that account's next five-hour
//! period. The account in use lends only its access token, so the login Claude
//! Code uses is never touched; a parked account lends its whole login, which
//! Claude Code renews as it runs when its token has lapsed, and the renewed
//! login goes back in the vault. What was tried is kept in
//! `~/.mc-switch/warmed.json`.
//!
//! Codex works the same way through `codex exec`: the account in use runs in
//! Codex's own home (Codex renews its login there itself, so no copy goes
//! stale), a parked one in a private `CODEX_HOME` whose renewed `auth.json`
//! goes back in the vault.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use pitstop_core::engine::vault_id;
use pitstop_core::provider::claude_code::ClaudeCode;
use pitstop_core::provider::codex::{jwt_claims, Codex};
use pitstop_core::provider::provider_for;
use pitstop_core::signin::{kill_tree, PrivateDir};
use pitstop_core::{vault, Host, ProviderId};
use serde_json::Value;

use crate::duty::Warmed;

const PROMPT: &str = "Reply with the single word OK.";
const TIMEOUT: Duration = Duration::from_secs(120);

/// Why a warm-up came to nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The limit is used up: nothing starts until it resets.
    LimitReached,
    /// The service refused the login.
    LoginRefused,
    /// The service was out of reach or overloaded.
    Unreachable,
    /// Anything else, in Claude Code's words.
    Failed(String),
}

impl Failure {
    /// A stable name for what it came to, kept in `warmed.json` and shown in Settings.
    pub fn code(&self) -> &'static str {
        match self {
            Failure::LimitReached => "limitReached",
            Failure::LoginRefused => "loginRefused",
            Failure::Unreachable => "unreachable",
            Failure::Failed(_) => "failed",
        }
    }
}

/// An account's access token, when it lapses (epoch seconds) and whether the
/// login holds what renews it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub access: String,
    pub expires_at: Option<i64>,
    pub renewable: bool,
}

impl Token {
    pub fn lapsed(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|until| until <= now)
    }
}

/// Where an account's warm-up setting and record are kept: Claude Code by its
/// bare name (as before Codex had warm-ups), any other tool as `tool:name`.
pub fn key(provider: ProviderId, name: &str) -> String {
    match provider {
        ProviderId::ClaudeCode => name.to_owned(),
        other => format!("{}:{name}", other.as_str()),
    }
}

/// The token in a Claude Code login: a vault blob, whose `credentials` hold
/// the `claudeAiOauth` Claude Code keeps on disk or in the keychain.
pub fn token_of(blob: &[u8]) -> Option<Token> {
    let blob: Value = serde_json::from_slice(blob).ok()?;
    let login: Value = serde_json::from_str(blob.get("credentials")?.as_str()?).ok()?;
    let oauth = login.get("claudeAiOauth")?;
    let access = oauth.get("accessToken")?.as_str()?.to_owned();
    let expires_at = oauth
        .get("expiresAt")
        .and_then(Value::as_i64)
        .map(|ms| ms / 1000);
    let renewable = oauth
        .get("refreshToken")
        .and_then(Value::as_str)
        .is_some_and(|r| !r.is_empty());
    Some(Token {
        access,
        expires_at,
        renewable,
    })
}

/// The token in a Codex login: `auth.json`, whose access token lapses at its
/// JWT's `exp` and is renewed with `tokens.refresh_token`.
pub fn codex_token_of(blob: &[u8]) -> Option<Token> {
    let auth: Value = serde_json::from_slice(blob).ok()?;
    let tokens = auth.get("tokens")?;
    let access = tokens.get("access_token")?.as_str()?.to_owned();
    let expires_at = jwt_claims(&access).and_then(|c| c.get("exp")?.as_i64());
    let renewable = tokens
        .get("refresh_token")
        .and_then(Value::as_str)
        .is_some_and(|r| !r.is_empty());
    Some(Token {
        access,
        expires_at,
        renewable,
    })
}

/// The token of `provider`'s account `name`: the login the tool holds when it
/// is the account in use, its parked copy otherwise.
pub fn token_for(host: &dyn Host, provider: ProviderId, name: &str, in_use: bool) -> Option<Token> {
    let blob = if in_use {
        provider_for(provider).read_active(host).ok()??.blob
    } else {
        vault::load(host, &vault_id(provider, name)).ok()?
    };
    match provider {
        ProviderId::Codex => codex_token_of(&blob),
        _ => token_of(&blob),
    }
}

/// Print mode on the smallest model, with no tools, MCP servers, slash
/// commands or kept session, answering in JSON. `dir` is its config home;
/// without `token` it signs in with the login lent to `dir`.
pub fn command(program: &Path, dir: &Path, token: Option<&str>, search: &[PathBuf]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args([
        "-p",
        PROMPT,
        "--model",
        "haiku",
        "--tools",
        "",
        "--no-session-persistence",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--output-format",
        "json",
    ])
    .current_dir(dir)
    .env("CLAUDE_CONFIG_DIR", dir)
    .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
    .env("DISABLE_AUTOUPDATER", "1")
    .env_remove("CLAUDE_SECURESTORAGE_CONFIG_DIR")
    .env_remove("ANTHROPIC_API_KEY")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    match token {
        Some(token) => cmd.env("CLAUDE_CODE_OAUTH_TOKEN", token),
        None => cmd.env_remove("CLAUDE_CODE_OAUTH_TOKEN"),
    };
    if let Ok(path) = std::env::join_paths(search) {
        cmd.env("PATH", path);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    // Its own process group, so a timeout reaches what an npm shim starts.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

/// `codex exec` answering one short prompt in `codex_home`, read-only, with no
/// user config (MCP servers, profiles), no git check, no kept session, events
/// as JSON lines. The model is Codex's default: naming one could name one the
/// account's plan does not have.
pub fn codex_command(program: &Path, codex_home: &Path, search: &[PathBuf]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args([
        "exec",
        "--skip-git-repo-check",
        "--ephemeral",
        "--ignore-user-config",
        "--sandbox",
        "read-only",
        "--json",
        PROMPT,
    ])
    .current_dir(codex_home)
    .env("CODEX_HOME", codex_home)
    .env_remove("OPENAI_API_KEY")
    .env_remove("CODEX_API_KEY")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    if let Ok(path) = std::env::join_paths(search) {
        cmd.env("PATH", path);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

/// What `codex exec --json` came to: a clean exit after `turn.completed` is a
/// warm-up; otherwise the words of its `turn.failed` or `error` event decide.
pub fn codex_verdict(exit: Option<i32>, stdout: &str) -> Result<(), Failure> {
    let events: Vec<Value> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .collect();
    let kind = |e: &Value| e.get("type").and_then(Value::as_str).map(str::to_owned);
    if exit == Some(0)
        && events
            .iter()
            .any(|e| kind(e).as_deref() == Some("turn.completed"))
    {
        return Ok(());
    }
    let said = events
        .iter()
        .rev()
        .find_map(|e| match kind(e).as_deref() {
            Some("turn.failed") => e.pointer("/error/message")?.as_str(),
            Some("error") => e.get("message")?.as_str(),
            _ => None,
        })
        .unwrap_or("Codex failed")
        .to_owned();
    let lower = said.to_lowercase();
    let has = |signs: &[&str]| signs.iter().any(|s| lower.contains(s));
    if has(&["usage limit", "usage_limit"]) {
        Err(Failure::LimitReached)
    } else if has(&[
        "401",
        "403",
        "unauthorized",
        "forbidden",
        "refresh token",
        "log in again",
        "sign in again",
    ]) {
        Err(Failure::LoginRefused)
    } else if has(&[
        "429",
        "500",
        "502",
        "503",
        "504",
        "stream disconnected",
        "overloaded",
    ]) || never_answered(&said)
    {
        Err(Failure::Unreachable)
    } else {
        Err(Failure::Failed(said))
    }
}

/// What print mode's JSON result came to: `is_error` false and a clean exit
/// is a warm-up; a 401/403 a refused login; 429 `usage_limit_reached` a used
/// up limit; another 429 or a 5xx a service out of reach.
pub fn verdict(exit: Option<i32>, stdout: &str) -> Result<(), Failure> {
    let Some(result) = stdout
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter(Value::is_object)
    else {
        return Err(Failure::Failed("Claude Code printed no result".into()));
    };
    if result.get("is_error").and_then(Value::as_bool) == Some(false) && exit == Some(0) {
        return Ok(());
    }
    let said = result
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or("Claude Code failed")
        .to_owned();
    match (
        result.get("api_error_status").and_then(Value::as_u64),
        result.get("api_error").and_then(Value::as_str),
    ) {
        (Some(429), Some("usage_limit_reached")) => Err(Failure::LimitReached),
        (Some(401 | 403), _) => Err(Failure::LoginRefused),
        (Some(429 | 500..=599), _) => Err(Failure::Unreachable),
        (None, _) if never_answered(&said) => Err(Failure::Unreachable),
        _ => Err(Failure::Failed(said)),
    }
}

/// Whether print mode's words say no answer came back at all: no connection,
/// or one that timed out.
fn never_answered(said: &str) -> bool {
    let said = said.to_lowercase();
    [
        "connection error",
        "timed out",
        "timeout",
        "econnreset",
        "econnrefused",
        "enotfound",
        "etimedout",
        "fetch failed",
        "network",
    ]
    .iter()
    .any(|sign| said.contains(sign))
}

/// A fresh directory only this user can read, removed when dropped with
/// whatever a run left outside it (Claude Code's keychain item on macOS). Its
/// path is resolved, so the keychain item Claude Code names after it is the
/// one [`ClaudeCode::lend`] made.
fn new_dir() -> Result<PrivateDir, Failure> {
    PrivateDir::create_for(ProviderId::ClaudeCode, "warm")
        .map_err(|e| Failure::Failed(e.to_string()))
}

/// Run one warm-up with `token`, in a directory made for it and removed after.
pub fn run(program: &Path, token: &str, search: &[PathBuf]) -> Result<(), Failure> {
    let dir = new_dir()?;
    run_in(command(program, dir.path(), Some(token), search), verdict)
}

/// Run one Codex warm-up of the account in use, in Codex's own home: it renews
/// that login in place, as it would for the user. The caller keeps switches
/// out while this runs, so a renewal never lands on a login switched in.
pub fn run_codex(host: &dyn Host, program: &Path, search: &[PathBuf]) -> Result<(), Failure> {
    let auth = Codex::auth_path(host);
    let home = auth
        .parent()
        .ok_or_else(|| Failure::Failed("no Codex home".into()))?;
    run_in(codex_command(program, home, search), codex_verdict)
}

/// Run one Codex warm-up of the parked account `name`, its `auth.json` copied
/// into a private `CODEX_HOME`; one Codex renewed meanwhile goes back in the
/// vault, whatever the warm-up came to. The caller keeps switches out.
pub fn run_codex_parked(
    host: &dyn Host,
    name: &str,
    program: &Path,
    search: &[PathBuf],
) -> Result<(), Failure> {
    let id = vault_id(ProviderId::Codex, name);
    let blob = vault::load(host, &id).map_err(|e| Failure::Failed(e.to_string()))?;
    let dir = PrivateDir::create_for(ProviderId::Codex, "warm")
        .map_err(|e| Failure::Failed(e.to_string()))?;
    let auth = dir.path().join("auth.json");
    let mut outcome = host
        .write_atomic(&auth, &blob)
        .map_err(|e| Failure::Failed(e.to_string()))
        .and_then(|()| run_in(codex_command(program, dir.path(), search), codex_verdict));
    if let Ok(Some(renewed)) = host.read(&auth) {
        if renewed != blob {
            if let Err(e) = vault::store(host, &id, &renewed) {
                outcome = Err(Failure::Failed(format!(
                    "couldn't keep the renewed login: {e}"
                )));
            }
        }
    }
    outcome
}

/// Run one warm-up of the parked account `name` with its whole login, lent to
/// a directory made for it; a login Claude Code renewed meanwhile goes back in
/// the vault, whatever the warm-up came to. The caller keeps switches out
/// while this runs: a switch to `name` would put in use a login this renews.
pub fn run_parked(
    host: &dyn Host,
    name: &str,
    program: &Path,
    search: &[PathBuf],
) -> Result<(), Failure> {
    let id = vault_id(ProviderId::ClaudeCode, name);
    let blob = vault::load(host, &id).map_err(|e| Failure::Failed(e.to_string()))?;
    let dir = new_dir()?;
    let claude = ClaudeCode::for_this_os();
    let mut outcome = claude
        .lend(host, dir.path(), &blob)
        .map_err(|e| Failure::Failed(e.to_string()))
        .and_then(|()| run_in(command(program, dir.path(), None, search), verdict));
    if let Ok(Some(renewed)) = claude.renewed(host, dir.path(), &blob) {
        if let Err(e) = vault::store(host, &id, &renewed) {
            outcome = Err(Failure::Failed(format!(
                "couldn't keep the renewed login: {e}"
            )));
        }
    }
    outcome
}

fn run_in(
    mut cmd: Command,
    verdict: fn(Option<i32>, &str) -> Result<(), Failure>,
) -> Result<(), Failure> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .spawn()
        .map_err(|e| Failure::Failed(format!("couldn't start {program}: {e}")))?;
    let mut stdout = child.stdout.take();
    let (said_tx, said_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(out) = stdout.as_mut() {
            let _ = out.read_to_string(&mut text);
        }
        let _ = said_tx.send(text);
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() > TIMEOUT => {
                kill_tree(&mut child);
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => {
                kill_tree(&mut child);
                return Err(Failure::Failed(e.to_string()));
            }
        }
    };
    // Something the tool started may still hold the pipe open: never wait on
    // it for long, or every duty after this one would wait too.
    let said = said_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    match status {
        // Print mode waits out a dead link retrying, so a run this long is
        // one that never reached the service.
        None => Err(Failure::Unreachable),
        Some(status) => verdict(status.code(), &said),
    }
}

// ---------- what was tried ----------

/// Every read-modify-write of `warmed.json` in this process holds this, so a
/// rename or a forget never interleaves with writing down a warm-up.
static LEDGER: Mutex<()> = Mutex::new(());

/// Every account's [`Warmed`], by name, in `~/.mc-switch/warmed.json`.
pub type Ledger = BTreeMap<String, Warmed>;

fn path(host: &dyn Host) -> PathBuf {
    host.home().join(".mc-switch").join("warmed.json")
}

/// The ledger, with failures counted afresh on a day after the one they were
/// counted for. A missing or unreadable file is an empty ledger.
pub fn load(host: &dyn Host, today: &str) -> Ledger {
    let mut ledger = read(host);
    for warmed in ledger.values_mut() {
        new_day(warmed, today);
    }
    ledger
}

fn read(host: &dyn Host) -> Ledger {
    host.read(&path(host))
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(host: &dyn Host, ledger: &Ledger) {
    let bytes = serde_json::to_vec_pretty(ledger).unwrap_or_default();
    let _ = host.write_atomic(&path(host), &bytes);
}

fn new_day(warmed: &mut Warmed, today: &str) {
    if warmed.date != today {
        warmed.date = today.to_owned();
        warmed.failures = 0;
        warmed.last_failure = None;
        warmed.last_error = None;
        warmed.unreachable = 0;
    }
}

/// Write down a warm-up of `name` at `now`: one that worked marks the start
/// of its period, one that failed counts against today's tries, unless the
/// service was out of reach or overloaded: a night offline must not use the
/// day up. Those count in a row instead, and each waits longer.
pub fn record(
    ledger: &mut Ledger,
    name: &str,
    today: &str,
    now: i64,
    outcome: Result<(), &Failure>,
) {
    let warmed = ledger.entry(name.to_owned()).or_default();
    new_day(warmed, today);
    match outcome {
        Ok(()) => {
            warmed.started = Some(now);
            warmed.last_error = None;
            warmed.unreachable = 0;
        }
        Err(failure) => {
            if *failure == Failure::Unreachable {
                warmed.unreachable += 1;
            } else {
                warmed.failures += 1;
                warmed.unreachable = 0;
            }
            warmed.last_failure = Some(now);
            warmed.last_error = Some(failure.code().to_owned());
        }
    }
}

/// An account renamed keeps what was tried for it.
pub fn rename_in(ledger: &mut Ledger, old: &str, new: &str) {
    if let Some(warmed) = ledger.remove(old) {
        ledger.insert(new.to_owned(), warmed);
    }
}

pub fn forget_in(ledger: &mut Ledger, name: &str) {
    ledger.remove(name);
}

/// Write down a warm-up of `name` that just ended, on the ledger as it is now
/// on disk, so what changed while it ran stays changed. Nothing is written
/// when `still_enrolled` says the account went (renamed or forgotten) in the
/// meantime; it is asked under the same lock a rename or forget takes.
pub fn update_if(
    host: &dyn Host,
    name: &str,
    today: &str,
    now: i64,
    outcome: Result<(), &Failure>,
    still_enrolled: impl FnOnce() -> bool,
) -> Warmed {
    let _held = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
    let mut ledger = load(host, today);
    if still_enrolled() {
        record(&mut ledger, name, today, now, outcome);
        save(host, &ledger);
    }
    ledger.get(name).cloned().unwrap_or_default()
}

/// Carry an account's record over a rename, in `warmed.json` itself.
pub fn rename(host: &dyn Host, old: &str, new: &str) {
    let _held = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
    let mut ledger = read(host);
    if ledger.contains_key(old) {
        rename_in(&mut ledger, old, new);
        save(host, &ledger);
    }
}

pub fn forget(host: &dyn Host, name: &str) {
    let _held = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
    let mut ledger = read(host);
    if ledger.contains_key(name) {
        forget_in(&mut ledger, name);
        save(host, &ledger);
    }
}

/// What an account's rename carries: its warm-up setting and its record both
/// move to the new key (see [`key`]).
pub fn follow_rename(host: &dyn Host, old: &str, new: &str) {
    let mut saved = crate::settings::load(host);
    saved.rename_account(old, new);
    let _ = crate::settings::save(host, &saved);
    rename(host, old, new);
}

/// What forgetting an account takes with it, by its [`key`].
pub fn follow_forget(host: &dyn Host, name: &str) {
    let mut saved = crate::settings::load(host);
    saved.forget_account(name);
    let _ = crate::settings::save(host, &saved);
    forget(host, name);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn the_command_runs_print_mode_with_the_token_in_its_own_home_and_nothing_else_of_ours() {
        let cmd = command(
            Path::new("claude"),
            Path::new("/tmp/w"),
            Some("tok-123"),
            &[],
        );
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args[0], OsStr::new("-p"));
        assert_eq!(args[1], OsStr::new(PROMPT));
        for flag in [
            "--no-session-persistence",
            "--strict-mcp-config",
            "--disable-slash-commands",
        ] {
            assert!(args.contains(&OsStr::new(flag)), "{flag}");
        }
        let at = args
            .iter()
            .position(|a| *a == OsStr::new("--tools"))
            .unwrap();
        assert_eq!(args[at + 1], OsStr::new(""));
        let env: Vec<_> = cmd.get_envs().collect();
        let value = |k: &str| {
            env.iter()
                .find(|(key, _)| *key == OsStr::new(k))
                .map(|(_, v)| *v)
        };
        assert_eq!(value("CLAUDE_CONFIG_DIR"), Some(Some(OsStr::new("/tmp/w"))));
        assert_eq!(
            value("CLAUDE_CODE_OAUTH_TOKEN"),
            Some(Some(OsStr::new("tok-123")))
        );
        assert_eq!(value("ANTHROPIC_API_KEY"), Some(None));
        assert_eq!(value("CLAUDE_SECURESTORAGE_CONFIG_DIR"), Some(None));
    }

    #[test]
    fn without_a_token_the_command_signs_in_with_the_lent_login_alone() {
        let cmd = command(Path::new("claude"), Path::new("/tmp/w"), None, &[]);
        let token = cmd
            .get_envs()
            .find(|(key, _)| *key == OsStr::new("CLAUDE_CODE_OAUTH_TOKEN"))
            .map(|(_, v)| v);
        assert_eq!(token, Some(None));
    }

    #[test]
    fn claude_keeps_bare_names_and_other_tools_are_prefixed() {
        assert_eq!(key(ProviderId::ClaudeCode, "work"), "work");
        assert_eq!(key(ProviderId::Codex, "work"), "codex:work");
    }

    #[test]
    fn the_codex_command_runs_exec_in_the_given_home_without_api_keys() {
        let cmd = codex_command(Path::new("codex"), Path::new("/tmp/c"), &[]);
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args[0], OsStr::new("exec"));
        for flag in [
            "--ephemeral",
            "--ignore-user-config",
            "--json",
            "--skip-git-repo-check",
        ] {
            assert!(args.contains(&OsStr::new(flag)), "{flag}");
        }
        assert_eq!(args.last(), Some(&OsStr::new(PROMPT)));
        let env: Vec<_> = cmd.get_envs().collect();
        let value = |k: &str| {
            env.iter()
                .find(|(key, _)| *key == OsStr::new(k))
                .map(|(_, v)| *v)
        };
        assert_eq!(value("CODEX_HOME"), Some(Some(OsStr::new("/tmp/c"))));
        assert_eq!(value("OPENAI_API_KEY"), Some(None));
    }

    #[test]
    fn each_codex_outcome_is_read_as_what_it_came_to() {
        let done = "{\"type\":\"thread.started\"}\n{\"type\":\"turn.completed\",\"usage\":{}}\n";
        let failed =
            |msg: &str| format!("{{\"type\":\"turn.failed\",\"error\":{{\"message\":\"{msg}\"}}}}");
        assert_eq!(codex_verdict(Some(0), done), Ok(()));
        assert_eq!(
            codex_verdict(Some(1), done),
            Err(Failure::Failed("Codex failed".into()))
        );
        assert_eq!(
            codex_verdict(
                Some(1),
                &failed("You've hit your usage limit. Try again later.")
            ),
            Err(Failure::LimitReached)
        );
        assert_eq!(
            codex_verdict(Some(1), &failed("unexpected status 401 Unauthorized")),
            Err(Failure::LoginRefused)
        );
        assert_eq!(
            codex_verdict(Some(1), &failed("stream disconnected before completion")),
            Err(Failure::Unreachable)
        );
        assert_eq!(
            codex_verdict(Some(1), "{\"type\":\"error\",\"message\":\"odd\"}"),
            Err(Failure::Failed("odd".into()))
        );
        assert_eq!(
            codex_verdict(Some(1), ""),
            Err(Failure::Failed("Codex failed".into()))
        );
    }

    #[test]
    fn a_codex_token_lapses_at_its_jwt_exp_and_renews_with_its_refresh_token() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        let payload = URL_SAFE_NO_PAD.encode(br#"{"exp":1000}"#);
        let blob =
            format!(r#"{{"tokens":{{"access_token":"h.{payload}.s","refresh_token":"r"}}}}"#);
        let token = codex_token_of(blob.as_bytes()).unwrap();
        assert_eq!(token.expires_at, Some(1000));
        assert!(token.renewable);
        assert!(token.lapsed(1000));
        assert!(codex_token_of(br#"{"OPENAI_API_KEY":"k"}"#).is_none());
    }

    #[test]
    fn each_result_print_mode_gives_is_read_as_what_it_came_to() {
        let cases = [
            (Some(0), r#"{"is_error":false,"result":"OK"}"#, Ok(())),
            (
                Some(0),
                "warning first\n{\"is_error\":false,\"result\":\"OK\"}\n",
                Ok(()),
            ),
            (
                Some(1),
                r#"{"is_error":true,"api_error_status":429,"api_error":"usage_limit_reached","result":"x"}"#,
                Err(Failure::LimitReached),
            ),
            (
                Some(1),
                r#"{"is_error":true,"api_error_status":429,"api_error":"rate_limit_error"}"#,
                Err(Failure::Unreachable),
            ),
            (
                Some(1),
                r#"{"is_error":true,"api_error_status":529}"#,
                Err(Failure::Unreachable),
            ),
            (
                Some(1),
                r#"{"is_error":true,"api_error_status":401,"result":"bad"}"#,
                Err(Failure::LoginRefused),
            ),
            (
                Some(1),
                r#"{"is_error":true,"api_error_status":403,"result":"forbidden"}"#,
                Err(Failure::LoginRefused),
            ),
            (
                Some(1),
                r#"{"is_error":true,"result":"something else"}"#,
                Err(Failure::Failed("something else".into())),
            ),
            (
                Some(1),
                r#"{"is_error":true,"result":"API Error: Connection error."}"#,
                Err(Failure::Unreachable),
            ),
            (
                Some(1),
                r#"{"is_error":true,"result":"API Error: Request timed out."}"#,
                Err(Failure::Unreachable),
            ),
            (
                Some(1),
                r#"{"is_error":false,"result":"OK"}"#,
                Err(Failure::Failed("OK".into())),
            ),
            (
                Some(0),
                "",
                Err(Failure::Failed("Claude Code printed no result".into())),
            ),
        ];
        for (exit, stdout, expected) in cases {
            assert_eq!(verdict(exit, stdout), expected, "{stdout}");
        }
    }

    #[test]
    fn a_vault_blob_gives_its_token_when_it_lapses_and_whether_it_renews() {
        let blob = |oauth: &str| {
            serde_json::to_vec(&serde_json::json!({
                "credentials": format!(r#"{{"claudeAiOauth":{oauth}}}"#),
                "oauth_account": {"emailAddress": "a@x.com"},
            }))
            .unwrap()
        };
        let token = token_of(&blob(
            r#"{"accessToken":"a-1","refreshToken":"r-1","expiresAt":1800000000123}"#,
        ))
        .unwrap();
        assert_eq!(
            token,
            Token {
                access: "a-1".into(),
                expires_at: Some(1_800_000_000),
                renewable: true,
            }
        );
        assert!(!token.lapsed(1_799_999_999));
        assert!(token.lapsed(1_800_000_000));
        assert!(
            !token_of(&blob(r#"{"accessToken":"a-1"}"#))
                .unwrap()
                .renewable
        );
        assert_eq!(token_of(br#"{"other":1}"#), None);
        // what Claude Code itself keeps is not a vault blob
        assert_eq!(
            token_of(br#"{"claudeAiOauth":{"accessToken":"a-1"}}"#),
            None
        );
    }

    #[test]
    fn failures_count_per_day_and_a_success_marks_the_start() {
        let mut ledger = Ledger::new();
        record(
            &mut ledger,
            "ben2",
            "2026-10-15",
            100,
            Err(&Failure::LoginRefused),
        );
        record(
            &mut ledger,
            "ben2",
            "2026-10-15",
            200,
            Err(&Failure::Failed("x".into())),
        );
        assert_eq!(ledger["ben2"].failures, 2);
        assert_eq!(ledger["ben2"].last_failure, Some(200));
        record(&mut ledger, "ben2", "2026-10-16", 300, Ok(()));
        assert_eq!(ledger["ben2"].failures, 0);
        assert_eq!(ledger["ben2"].last_failure, None);
        assert_eq!(ledger["ben2"].started, Some(300));
    }

    #[test]
    fn a_warm_up_that_never_reached_the_service_waits_but_uses_no_try() {
        let mut ledger = Ledger::new();
        for at in [100, 200, 300, 400] {
            record(
                &mut ledger,
                "ben1",
                "2026-10-18",
                at,
                Err(&Failure::Unreachable),
            );
        }
        assert_eq!(ledger["ben1"].failures, 0);
        assert_eq!(ledger["ben1"].unreachable, 4);
        assert_eq!(ledger["ben1"].last_failure, Some(400));
        assert_eq!(ledger["ben1"].last_error.as_deref(), Some("unreachable"));
    }

    #[test]
    fn a_failure_is_written_down_with_its_kind_and_a_success_clears_it() {
        let mut ledger = Ledger::new();
        record(
            &mut ledger,
            "ben2",
            "2026-10-15",
            100,
            Err(&Failure::LoginRefused),
        );
        assert_eq!(ledger["ben2"].last_error.as_deref(), Some("loginRefused"));
        record(&mut ledger, "ben2", "2026-10-15", 200, Ok(()));
        assert_eq!(ledger["ben2"].last_error, None);
    }

    #[test]
    fn a_renamed_account_keeps_its_record_and_a_forgotten_one_loses_it() {
        let mut ledger = Ledger::new();
        record(&mut ledger, "old", "2026-10-15", 100, Ok(()));
        rename_in(&mut ledger, "old", "new");
        assert!(!ledger.contains_key("old"));
        assert_eq!(ledger["new"].started, Some(100));
        forget_in(&mut ledger, "new");
        assert!(ledger.is_empty());
    }

    #[test]
    fn writing_down_a_warm_up_keeps_what_changed_on_disk_meanwhile() {
        let host = pitstop_core::testing::FakeHost::new();
        let mut ledger = Ledger::new();
        record(&mut ledger, "a", "2026-10-15", 1, Ok(()));
        record(&mut ledger, "b", "2026-10-15", 1, Ok(()));
        save(&host, &ledger);
        // While a's warm-up runs, b is forgotten.
        forget(&host, "b");
        let a = update_if(
            &host,
            "a",
            "2026-10-15",
            50,
            Err(&Failure::LoginRefused),
            || true,
        );
        assert_eq!(a.failures, 1);
        let on_disk = load(&host, "2026-10-15");
        assert!(!on_disk.contains_key("b"), "a forgotten account came back");
        assert_eq!(on_disk["a"].last_failure, Some(50));
    }

    #[test]
    fn a_rename_and_a_forget_carry_the_setting_and_the_record() {
        let host = pitstop_core::testing::FakeHost::new();
        let settings = crate::settings::Settings {
            warm_up: vec!["old".into(), "other".into()],
            ..Default::default()
        };
        crate::settings::save(&host, &settings).unwrap();
        let mut ledger = Ledger::new();
        record(&mut ledger, "old", "2026-10-15", 7, Ok(()));
        save(&host, &ledger);

        follow_rename(&host, "old", "new");
        assert_eq!(
            crate::settings::load(&host).warm_up,
            vec!["new".to_string(), "other".to_string()]
        );
        assert_eq!(load(&host, "2026-10-15")["new"].started, Some(7));

        follow_forget(&host, "new");
        assert_eq!(
            crate::settings::load(&host).warm_up,
            vec!["other".to_string()]
        );
        assert!(load(&host, "2026-10-15").is_empty());
    }

    #[test]
    fn a_warm_up_of_an_account_that_went_meanwhile_is_not_written_down() {
        let host = pitstop_core::testing::FakeHost::new();
        update_if(&host, "gone", "2026-10-15", 9, Ok(()), || false);
        assert!(load(&host, "2026-10-15").is_empty());
    }
}
