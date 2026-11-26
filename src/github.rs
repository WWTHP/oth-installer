//! GitHub API access: mod list, releases and asset downloads (token from `auth`).

use std::io::Read;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const ORG: &str = "WWTHP";
/// Repos in the org that aren't mods.
const NOT_MODS: &[&str] = &["oth-installer"];
/// Error prefix for a rejected token (the UI then offers sign-in).
pub const AUTH_REJECTED: &str = "GitHub rejected the login (expired or revoked).";
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
    pub id: u64,
    pub name: String,
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

fn agent(follow_redirects: bool) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .max_redirects(if follow_redirects { 10 } else { 0 })
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .user_agent("oth-installer")
        .build()
        .into()
}

fn api_get(token: &str, path: &str) -> Result<(u16, String), String> {
    let mut resp = agent(true)
        .get(format!("https://api.github.com{path}"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|e| format!("GitHub request failed: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .with_config()
        .limit(16 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| format!("GitHub response unreadable: {e}"))?;
    Ok((status, body))
}

fn api_json<T: for<'de> Deserialize<'de>>(token: &str, path: &str) -> Result<Option<T>, String> {
    let (status, body) = api_get(token, path)?;
    match status {
        200 => serde_json::from_str(&body)
            .map(Some)
            .map_err(|e| format!("bad JSON from {path}: {e}")),
        404 => Ok(None),
        401 => Err(format!("{AUTH_REJECTED} Sign in again.")),
        s => Err(format!(
            "GitHub {path}: HTTP {s}: {}",
            body.chars().take(200).collect::<String>()
        )),
    }
}

#[derive(Deserialize)]
struct User {
    login: String,
}

/// The login the token belongs to (also checks the token works).
pub fn whoami(token: &str) -> Result<String, String> {
    api_json::<User>(token, "/user")?
        .map(|u| u.login)
        .ok_or_else(|| "GitHub didn't return the signed-in user.".into())
}

/// Every mod repo in the org with a published release.
pub fn list_mods(token: &str) -> Result<Vec<RemoteMod>, String> {
    let repos: Vec<Repo> = api_json(token, &format!("/orgs/{ORG}/repos?per_page=100&type=all"))?
        .ok_or_else(|| {
            format!("Organization {ORG} not found or not visible to this GitHub login.")
        })?;
    let mut mods = Vec::new();
    for repo in repos {
        if repo.archived || !repo.name.starts_with("oth-") || NOT_MODS.contains(&repo.name.as_str())
        {
            continue;
        }
        let Some(rel): Option<Release> = api_json(
            token,
            &format!("/repos/{ORG}/{}/releases/latest", repo.name),
        )?
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

/// Downloads a release asset by id. The API answers with a redirect to a signed storage URL;
/// that hop is followed without the token so it never leaves GitHub's API.
pub fn download_asset(
    token: &str,
    owner: &str,
    repo: &str,
    asset_id: u64,
) -> Result<Vec<u8>, String> {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/releases/assets/{asset_id}");
    let mut resp = agent(false)
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/octet-stream")
        .call()
        .map_err(|e| format!("Download failed: {e}"))?;
    let status = resp.status().as_u16();
    if (300..400).contains(&status) {
        let loc = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or("Download redirect without a location")?
            .to_string();
        resp = agent(true)
            .get(&loc)
            .call()
            .map_err(|e| format!("Download failed: {e}"))?;
    }
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(format!(
            "Download of asset {asset_id} from {repo} failed: HTTP {status}"
        ));
    }
    let mut bytes = Vec::new();
    resp.into_body()
        .into_reader()
        .take(MAX_DOWNLOAD)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Download interrupted: {e}"))?;
    Ok(bytes)
}

/// Downloads a public file (no token), following redirects.
pub fn download_public(url: &str) -> Result<Vec<u8>, String> {
    let resp = agent(true)
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

    fn asset(id: u64, name: &str) -> Asset {
        Asset {
            id,
            name: name.into(),
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
            (m.name.as_str(), m.version.as_str(), m.asset.id),
            ("WombatGD", "0.6.0", 2)
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

/// Live check against GitHub (needs `gh auth login`): `cargo test -- --ignored`.
#[cfg(test)]
mod live {
    use super::*;

    #[test]
    #[ignore = "network + GitHub login"]
    fn lists_downloads_and_verifies_every_mod() {
        let t = crate::auth::token().unwrap();
        let mods = list_mods(&t).unwrap();
        assert!(!mods.is_empty());
        let game = tempfile::tempdir().unwrap();
        for m in &mods {
            let bytes = download_asset(&t, ORG, &m.repo, m.asset.id).unwrap();
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
        println!("signed in as {}", whoami(&t).unwrap());
        let z = download_public(crate::MELON_URL).unwrap();
        verify(&z, Some(crate::MELON_SHA256), "MelonLoader").unwrap();
        crate::install::install_melonloader(game.path(), &z).unwrap();
        assert!(game.path().join("version.dll").is_file());
        assert!(game
            .path()
            .join("MelonLoader/net6/MelonLoader.dll")
            .is_file());
    }
}
