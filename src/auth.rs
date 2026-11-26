//! GitHub login. The mod repos are private, so every request needs a token. Sources, in order:
//! `GH_TOKEN` / `GITHUB_TOKEN`, the token this app saved (device sign-in or pasted), `gh auth token`.
//!
//! Device sign-in (no gh needed, e.g. on a Steam Deck) uses GitHub's device flow with the client
//! id baked in at build time (`OTH_GITHUB_CLIENT_ID`; see README §Sign-in setup).

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

/// Client id for device sign-in, baked in at build time; None when the build has none.
pub fn client_id() -> Option<&'static str> {
    option_env!("OTH_GITHUB_CLIENT_ID")
        .map(str::trim)
        .filter(|s| !s.is_empty())
}
/// Error text when no token is found (the UI then shows sign-in).
pub const NOT_SIGNED_IN: &str = "Not signed in to GitHub.";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Source {
    Env,
    Saved,
    GhCli,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Env => "GH_TOKEN environment variable",
            Source::Saved => "saved sign-in",
            Source::GhCli => "GitHub CLI (gh)",
        }
    }
}

pub fn find() -> Option<(String, Source)> {
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(t) = std::env::var(var) {
            if !t.trim().is_empty() {
                return Some((t.trim().to_string(), Source::Env));
            }
        }
    }
    if let Some(t) = saved() {
        return Some((t, Source::Saved));
    }
    gh_token().map(|t| (t, Source::GhCli))
}

pub fn token() -> Result<String, String> {
    find()
        .map(|(t, _)| t)
        .ok_or_else(|| NOT_SIGNED_IN.to_string())
}

fn token_path() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("oth-installer")
            .join("github-token"),
    )
}

fn saved() -> Option<String> {
    let t = std::fs::read_to_string(token_path()?).ok()?;
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Stores the token readable only by this user.
pub fn save(token: &str) -> Result<(), String> {
    let p = token_path().ok_or("no config folder on this system")?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    opts.open(&p)
        .and_then(|mut f| f.write_all(token.trim().as_bytes()))
        .map_err(|e| format!("Can't save the token to {}: {e}", p.display()))
}

pub fn forget() {
    if let Some(p) = token_path() {
        let _ = std::fs::remove_file(p);
    }
}

/// `OTH_INSTALLER_NO_GH=1` skips gh (to test the sign-in screen on a machine with gh).
fn gh_token() -> Option<String> {
    if std::env::var("OTH_INSTALLER_NO_GH").is_ok_and(|v| v == "1") {
        return None;
    }
    gh_candidates().into_iter().find_map(|gh| {
        let mut cmd = Command::new(gh);
        cmd.args(["auth", "token", "--hostname", "github.com"]);
        hide_console(&mut cmd);
        let out = cmd.output().ok().filter(|o| o.status.success())?;
        let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!t.is_empty()).then_some(t)
    })
}

fn gh_candidates() -> Vec<String> {
    let mut c = vec!["gh".to_string()];
    // Apps started from Finder/the desktop don't get the shell's PATH.
    if cfg!(target_os = "macos") {
        c.extend(["/opt/homebrew/bin/gh".into(), "/usr/local/bin/gh".into()]);
    } else if cfg!(windows) {
        c.push(r"C:\Program Files\GitHub CLI\gh.exe".into());
    } else {
        c.extend([
            "/usr/bin/gh".into(),
            "/usr/local/bin/gh".into(),
            "/home/linuxbrew/.linuxbrew/bin/gh".into(),
        ]);
    }
    c
}

#[cfg(windows)]
fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}
#[cfg(not(windows))]
fn hide_console(_: &mut Command) {}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, PartialEq)]
pub enum PollResult {
    Pending,
    SlowDown,
    Token(String),
    Failed(String),
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

fn post_form(url: &str, form: &[(&str, &str)]) -> Result<String, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .user_agent("oth-installer")
        .build()
        .into();
    let mut resp = agent
        .post(url)
        .header("Accept", "application/json")
        .send_form(form.iter().copied())
        .map_err(|e| format!("GitHub sign-in request failed: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(format!(
            "GitHub sign-in: HTTP {status}: {}",
            body.chars().take(200).collect::<String>()
        ));
    }
    Ok(body)
}

pub fn start_device_flow(client_id: &str) -> Result<DeviceCode, String> {
    let body = post_form(
        "https://github.com/login/device/code",
        // `repo` matters for an OAuth app; a GitHub App ignores scopes and uses its permissions.
        &[("client_id", client_id), ("scope", "repo")],
    )?;
    if let Ok(r) = serde_json::from_str::<TokenResponse>(&body) {
        if let Some(e) = r.error {
            return Err(device_error(&e, r.error_description.as_deref()));
        }
    }
    serde_json::from_str(&body).map_err(|e| format!("Unexpected sign-in response: {e}"))
}

pub fn poll_device_flow(client_id: &str, device_code: &str) -> PollResult {
    let body = match post_form(
        "https://github.com/login/oauth/access_token",
        &[
            ("client_id", client_id),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ],
    ) {
        Ok(b) => b,
        Err(e) => return PollResult::Failed(e),
    };
    parse_poll(&body)
}

fn parse_poll(body: &str) -> PollResult {
    match serde_json::from_str::<TokenResponse>(body) {
        Ok(TokenResponse {
            access_token: Some(t),
            ..
        }) if !t.is_empty() => PollResult::Token(t),
        Ok(TokenResponse {
            error: Some(e),
            error_description,
            ..
        }) => match e.as_str() {
            "authorization_pending" => PollResult::Pending,
            "slow_down" => PollResult::SlowDown,
            _ => PollResult::Failed(device_error(&e, error_description.as_deref())),
        },
        _ => PollResult::Failed("Unexpected sign-in response from GitHub.".into()),
    }
}

fn device_error(code: &str, desc: Option<&str>) -> String {
    match code {
        "expired_token" => "The sign-in code expired. Start again.".into(),
        "access_denied" => "Sign-in was cancelled on GitHub.".into(),
        "device_flow_disabled" => {
            "Device sign-in is disabled for this GitHub app (enable \"Device Flow\" in its settings).".into()
        }
        "incorrect_client_credentials" => "This build's GitHub client id is wrong.".into(),
        _ => format!("GitHub sign-in failed: {}", desc.unwrap_or(code)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_responses() {
        assert_eq!(
            parse_poll(r#"{"access_token":"gho_x","token_type":"bearer","scope":"repo"}"#),
            PollResult::Token("gho_x".into())
        );
        assert_eq!(
            parse_poll(r#"{"error":"authorization_pending"}"#),
            PollResult::Pending
        );
        assert_eq!(
            parse_poll(r#"{"error":"slow_down","interval":10}"#),
            PollResult::SlowDown
        );
        assert!(matches!(
            parse_poll(r#"{"error":"expired_token"}"#),
            PollResult::Failed(m) if m.contains("expired")
        ));
        assert!(matches!(parse_poll("garbage"), PollResult::Failed(_)));
        assert!(matches!(
            parse_poll(r#"{"access_token":""}"#),
            PollResult::Failed(_)
        ));
    }
}
