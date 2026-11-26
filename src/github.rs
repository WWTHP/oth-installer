//! GitHub access: mod list, releases and downloads. The repos are public, so no login.

use std::io::Read;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const ORG: &str = "WWTHP";
/// Repos in the org that aren't mods.
const NOT_MODS: &[&str] = &["oth-installer"];
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct RemoteMod {
    pub repo: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub asset: Asset,
    pub release_url: String,
    /// SHA-256 (hex) from GitHub's asset digest or the release notes, when known.
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub digest: Option<String>,
}

#[derive(Deserialize)]
struct Repo {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    archived: bool,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    assets: Vec<Asset>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .http_status_as_error(false)
        .user_agent("oth-installer")
        .build()
        .into()
}

fn api_json<T: for<'de> Deserialize<'de>>(path: &str) -> Result<Option<T>, String> {
    let mut resp = agent()
        .get(format!("https://api.github.com{path}"))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|e| format!("Can't reach GitHub (offline?): {e}"))?;
    let status = resp.status().as_u16();
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    };
    let (remaining, reset) = (header("x-ratelimit-remaining"), header("x-ratelimit-reset"));
    let body = resp
        .body_mut()
        .with_config()
        .limit(16 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| format!("GitHub response unreadable: {e}"))?;
    match status {
        200 => serde_json::from_str(&body)
            .map(Some)
            .map_err(|e| format!("bad JSON from {path}: {e}")),
        404 => Ok(None),
        403 | 429 if remaining.as_deref() == Some("0") => Err(rate_limited(reset.as_deref())),
        s => Err(format!(
            "GitHub {path}: HTTP {s}: {}",
            body.chars().take(200).collect::<String>()
        )),
    }
}

/// GitHub allows 60 anonymous API requests an hour per IP; a refresh uses about 7.
fn rate_limited(reset: Option<&str>) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match reset.and_then(|r| r.parse::<u64>().ok()) {
        Some(at) if at > now => format!(
            "GitHub's hourly limit for this network is used up. Try again in {} min.",
            (at - now).div_ceil(60)
        ),
        _ => "GitHub's hourly limit for this network is used up. Try again later.".into(),
    }
}

/// Every mod repo in the org with a published release.
pub fn list_mods() -> Result<Vec<RemoteMod>, String> {
    let repos: Vec<Repo> = api_json(&format!("/orgs/{ORG}/repos?per_page=100&type=public"))?
        .ok_or_else(|| format!("GitHub organization {ORG} not found."))?;
    let mut mods = Vec::new();
    for repo in repos {
        if repo.archived || !repo.name.starts_with("oth-") || NOT_MODS.contains(&repo.name.as_str())
        {
            continue;
        }
        let Some(rel): Option<Release> =
            api_json(&format!("/repos/{ORG}/{}/releases/latest", repo.name))?
        else {
            continue; // no release yet
        };
        if let Some(m) = to_remote_mod(&repo.name, repo.description.unwrap_or_default(), rel) {
            mods.push(m);
        }
    }
    mods.sort_by_key(|m| m.name.to_lowercase());
    Ok(mods)
}

fn to_remote_mod(repo: &str, description: String, rel: Release) -> Option<RemoteMod> {
    // Prefer the zip (it carries UserData files); a bare DLL otherwise.
    let asset = rel
        .assets
        .iter()
        .find(|a| a.name.ends_with(".zip"))
        .or_else(|| rel.assets.iter().find(|a| a.name.ends_with(".dll")))?
        .clone();
    let version = rel.tag_name.trim_start_matches('v').to_string();
    // Release names are "<Mod> <version>"; the DLL name is the mod name too.
    let name = if let Some(stem) = asset.name.strip_suffix(".dll") {
        stem.to_string()
    } else {
        rel.name
            .as_deref()
            .and_then(|n| n.split_whitespace().next())
            .map(String::from)
            .unwrap_or_else(|| repo.to_string())
    };
    let sha256 = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .map(str::to_ascii_lowercase)
        .or_else(|| sha_from_notes(rel.body.as_deref().unwrap_or(""), &asset.name));
    Some(RemoteMod {
        repo: repo.to_string(),
        name,
        description,
        version,
        asset,
        release_url: rel.html_url,
        sha256,
    })
}

/// Release notes contain "`<file>` SHA-256: `<hex>`".
fn sha_from_notes(body: &str, file: &str) -> Option<String> {
    body.lines()
        .filter(|l| l.contains(file) && l.contains("SHA-256"))
        .find_map(|l| {
            let after = &l[l.find("SHA-256")?..];
            after
                .split(|c: char| !c.is_ascii_hexdigit())
                .find(|w| w.len() == 64)
                .map(str::to_ascii_lowercase)
        })
}

/// Downloads a release asset or other public file, following redirects.
pub fn download(url: &str) -> Result<Vec<u8>, String> {
    let resp = agent()
        .get(url)
        .call()
        .map_err(|e| format!("Download failed: {e}"))?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(format!("Download of {url} failed: HTTP {status}"));
    }
    let mut bytes = Vec::new();
    resp.into_body()
        .into_reader()
        .take(MAX_DOWNLOAD)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Download interrupted: {e}"))?;
    Ok(bytes)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Fails when an expected hash is known and doesn't match.
pub fn verify(bytes: &[u8], expected: Option<&str>, what: &str) -> Result<(), String> {
    match expected {
        Some(want) if sha256_hex(bytes) != want => Err(format!(
            "{what}: SHA-256 mismatch (download corrupted or tampered). Not installed."
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(_id: u64, name: &str) -> Asset {
        Asset {
            name: name.into(),
            browser_download_url: format!("https://example.invalid/{name}"),
            digest: None,
        }
    }

    #[test]
    fn mod_from_zip_release() {
        let hex = "3619ccf00aa336e3666bd13cfd79eb344b8e263c8237eb3cc76ccb3b0c49fadb";
        let rel = Release {
            tag_name: "v0.6.0".into(),
            name: Some("WombatGD 0.6.0".into()),
            body: Some(format!("intro\n`WombatGD-0.6.0.zip` SHA-256: `{hex}`\n")),
            html_url: "u".into(),
            assets: vec![asset(1, "notes.txt"), asset(2, "WombatGD-0.6.0.zip")],
        };
        let m = to_remote_mod("oth-wombatgd", "d".into(), rel).unwrap();
        assert_eq!(
            (m.name.as_str(), m.version.as_str(), m.asset.name.as_str()),
            ("WombatGD", "0.6.0", "WombatGD-0.6.0.zip")
        );
        assert_eq!(m.sha256.as_deref(), Some(hex));
    }

    #[test]
    fn mod_from_dll_release_prefers_digest() {
        let mut a = asset(7, "TireDamage.dll");
        a.digest = Some("sha256:ABCDEF".into());
        let rel = Release {
            tag_name: "v0.8.0".into(),
            name: None,
            body: None,
            html_url: "u".into(),
            assets: vec![a],
        };
        let m = to_remote_mod("oth-tire-damage", String::new(), rel).unwrap();
        assert_eq!(m.name, "TireDamage");
        assert_eq!(m.sha256.as_deref(), Some("abcdef"));
    }

    #[test]
    fn release_without_installable_asset_is_skipped() {
        let rel = Release {
            tag_name: "v1".into(),
            name: None,
            body: None,
            html_url: "u".into(),
            assets: vec![asset(1, "source.tar.gz")],
        };
        assert!(to_remote_mod("oth-x", String::new(), rel).is_none());
    }

    #[test]
    fn verify_hash() {
        let h = sha256_hex(b"abc");
        assert_eq!(
            h,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(verify(b"abc", Some(&h), "x").is_ok());
        assert!(verify(b"abd", Some(&h), "x").is_err());
        assert!(verify(b"abd", None, "x").is_ok());
    }
}

/// Live check against GitHub: `cargo test -- --ignored`.
#[cfg(test)]
mod live {
    use super::*;

    #[test]
    #[ignore = "network"]
    fn lists_downloads_and_verifies_every_mod() {
        let mods = list_mods().unwrap();
        assert!(!mods.is_empty());
        let game = tempfile::tempdir().unwrap();
        for m in &mods {
            let bytes = download(&m.asset.browser_download_url).unwrap();
            verify(&bytes, m.sha256.as_deref(), &m.asset.name).unwrap();
            crate::install::install(
                game.path(),
                &m.name,
                &m.version,
                &m.repo,
                &m.asset.name,
                &bytes,
            )
            .unwrap();
            println!(
                "{} {} {} sha={:?} bytes={}",
                m.repo,
                m.name,
                m.version,
                m.sha256.is_some(),
                bytes.len()
            );
        }
        for m in &mods {
            crate::install::uninstall(game.path(), &m.name).unwrap();
        }
        let z = download(crate::MELON_URL).unwrap();
        verify(&z, Some(crate::MELON_SHA256), "MelonLoader").unwrap();
        crate::install::install_melonloader(game.path(), &z).unwrap();
        assert!(game.path().join("version.dll").is_file());
        assert!(game
            .path()
            .join("MelonLoader/net6/MelonLoader.dll")
            .is_file());
    }
}
