//! Installing and removing mods in the game folder, tracked by a manifest so removal deletes
//! exactly the files an install wrote.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Top-level game folders a mod archive may write into.
const MOD_ROOTS: &[&str] = &["Mods", "UserData", "UserLibs", "Plugins"];
const MANIFEST: &str = "UserData/WWTHP-Installer.json";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub mods: BTreeMap<String, InstalledMod>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledMod {
    pub version: String,
    pub repo: String,
    /// Paths relative to the game folder, with `/` separators.
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LocalState {
    NotInstalled,
    /// Installed by this tool, at this version.
    Installed(String),
    /// `Mods/<Name>.dll` is there but this tool didn't put it there.
    Manual,
}

pub fn load_manifest(game: &Path) -> Manifest {
    fs::read_to_string(game.join(MANIFEST))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_manifest(game: &Path, m: &Manifest) -> Result<(), String> {
    let json = serde_json::to_string_pretty(m).map_err(|e| e.to_string())?;
    write_atomic(&game.join(MANIFEST), json.as_bytes())
}

pub fn state(game: &Path, manifest: &Manifest, name: &str) -> LocalState {
    if let Some(m) = manifest.mods.get(name) {
        if m.files.iter().any(|f| game.join(f).is_file()) {
            return LocalState::Installed(m.version.clone());
        }
    }
    if game.join("Mods").join(format!("{name}.dll")).is_file() {
        LocalState::Manual
    } else {
        LocalState::NotInstalled
    }
}

/// Turns an archive entry name into a safe path under the game folder, or rejects it.
/// `roots` limits the first component (None = anywhere inside the game folder).
pub fn safe_relative(name: &str, roots: Option<&[&str]>) -> Result<PathBuf, String> {
    let normalized = name.replace('\\', "/");
    let mut out = PathBuf::new();
    for c in Path::new(&normalized).components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => return Err(format!("unsafe path in archive: {name}")),
        }
    }
    let first = out
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .ok_or_else(|| format!("empty path in archive: {name}"))?
        .to_string();
    if let Some(roots) = roots {
        if !roots.iter().any(|r| r.eq_ignore_ascii_case(&first)) {
            return Err(format!("archive entry outside {roots:?}: {name}"));
        }
    }
    Ok(out)
}

fn rel_string(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("oth-installer-tmp");
    fs::write(&tmp, bytes).map_err(|e| format!("Can't write {}: {e}", path.display()))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!(
            "Can't replace {} (is the game running?): {e}",
            path.display()
        )
    })
}

/// Files (relative path → bytes) a download expands to. Validates everything before any write.
pub fn unpack(
    asset_name: &str,
    bytes: &[u8],
    roots: Option<&[&str]>,
) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    if asset_name.ends_with(".dll") {
        let file = Path::new(asset_name).file_name().ok_or("bad asset name")?;
        return Ok(vec![(Path::new("Mods").join(file), bytes.to_vec())]);
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| format!("{asset_name} is not a valid zip: {e}"))?;
    let mut files = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("{asset_name}: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let entry_name = entry
            .name()
            .map_err(|e| format!("{asset_name}: {e}"))?
            .into_owned();
        let rel = safe_relative(&entry_name, roots)?;
        let mut data = Vec::with_capacity(entry.size() as usize);
        std::io::copy(&mut entry, &mut data).map_err(|e| format!("{asset_name}: {e}"))?;
        files.push((rel, data));
    }
    if files.is_empty() {
        return Err(format!("{asset_name} is empty"));
    }
    Ok(files)
}

/// Installs (or updates) a mod from its downloaded release asset.
pub fn install(
    game: &Path,
    name: &str,
    version: &str,
    repo: &str,
    asset_name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let files = unpack(asset_name, bytes, Some(MOD_ROOTS))?;
    let mut manifest = load_manifest(game);
    let mut written = BTreeSet::new();
    for (rel, data) in &files {
        write_atomic(&game.join(rel), data)?;
        written.insert(rel_string(rel));
    }
    // Drop files the previous version had and this one doesn't.
    if let Some(old) = manifest.mods.get(name) {
        for f in old.files.iter().filter(|f| !written.contains(*f)) {
            remove_file_and_empty_parents(game, f);
        }
    }
    // A manually copied DLL under a different name than the release's would load twice.
    let manual = format!("Mods/{name}.dll");
    if !written.contains(&manual)
        && files
            .iter()
            .any(|(p, _)| rel_string(p).starts_with("Mods/"))
    {
        remove_file_and_empty_parents(game, &manual);
    }
    manifest.mods.insert(
        name.to_string(),
        InstalledMod {
            version: version.to_string(),
            repo: repo.to_string(),
            files: written.into_iter().collect(),
        },
    );
    save_manifest(game, &manifest)
}

/// Removes a mod's files. Settings in `UserData/MelonPreferences.cfg` are left alone.
pub fn uninstall(game: &Path, name: &str) -> Result<(), String> {
    let mut manifest = load_manifest(game);
    let mut files = manifest
        .mods
        .remove(name)
        .map(|m| m.files)
        .unwrap_or_default();
    files.push(format!("Mods/{name}.dll"));
    let mut errors = Vec::new();
    for f in &files {
        let p = game.join(f);
        if p.is_file() {
            if let Err(e) = fs::remove_file(&p) {
                errors.push(format!("{}: {e}", p.display()));
                continue;
            }
        }
        prune_empty_parents(game, f);
    }
    save_manifest(game, &manifest)?;
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Couldn't remove (is the game running?): {}",
            errors.join("; ")
        ))
    }
}

fn remove_file_and_empty_parents(game: &Path, rel: &str) {
    let _ = fs::remove_file(game.join(rel));
    prune_empty_parents(game, rel);
}

/// Removes now-empty folders below the top-level root (never `Mods/` or `UserData/` themselves).
fn prune_empty_parents(game: &Path, rel: &str) {
    let mut p = Path::new(rel).parent();
    while let Some(dir) = p {
        if dir.components().count() < 2 {
            break;
        }
        if fs::remove_dir(game.join(dir)).is_err() {
            break; // not empty (or gone)
        }
        p = dir.parent();
    }
}

/// Extracts MelonLoader's zip over the game folder.
pub fn install_melonloader(game: &Path, zip_bytes: &[u8]) -> Result<(), String> {
    for (rel, data) in unpack("MelonLoader.zip", zip_bytes, None)? {
        write_atomic(&game.join(rel), &data)?;
    }
    for d in ["Mods", "Plugins", "UserData", "UserLibs"] {
        let _ = fs::create_dir_all(game.join(d));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            for (name, data) in entries {
                if name.ends_with('/') {
                    z.add_directory(*name, opts).unwrap();
                } else {
                    z.start_file(*name, opts).unwrap();
                    z.write_all(data).unwrap();
                }
            }
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn safe_paths() {
        let roots = Some(MOD_ROOTS);
        assert_eq!(
            safe_relative("Mods/A.dll", roots).unwrap(),
            Path::new("Mods/A.dll")
        );
        assert_eq!(
            safe_relative("./UserData\\A\\b.wav", roots).unwrap(),
            Path::new("UserData/A/b.wav")
        );
        assert!(safe_relative("../evil.dll", roots).is_err());
        assert!(safe_relative("Mods/../../evil", roots).is_err());
        assert!(safe_relative("/etc/passwd", roots).is_err());
        assert!(safe_relative("C:/Windows/x", roots).is_err());
        assert!(safe_relative("version.dll", roots).is_err());
        assert!(safe_relative("", roots).is_err());
        assert!(safe_relative("version.dll", None).is_ok());
    }

    #[test]
    fn dll_install_and_remove() {
        let t = tempfile::tempdir().unwrap();
        let g = t.path();
        assert_eq!(
            state(g, &load_manifest(g), "TireDamage"),
            LocalState::NotInstalled
        );
        install(
            g,
            "TireDamage",
            "0.8.0",
            "oth-tire-damage",
            "TireDamage.dll",
            b"dll",
        )
        .unwrap();
        assert_eq!(fs::read(g.join("Mods/TireDamage.dll")).unwrap(), b"dll");
        assert_eq!(
            state(g, &load_manifest(g), "TireDamage"),
            LocalState::Installed("0.8.0".into())
        );
        uninstall(g, "TireDamage").unwrap();
        assert!(!g.join("Mods/TireDamage.dll").exists());
        assert!(g.join("Mods").is_dir(), "Mods/ itself stays");
        assert_eq!(
            state(g, &load_manifest(g), "TireDamage"),
            LocalState::NotInstalled
        );
    }

    #[test]
    fn zip_update_drops_stale_files_and_remove_cleans_up() {
        let t = tempfile::tempdir().unwrap();
        let g = t.path();
        fs::create_dir_all(g.join("UserData")).unwrap();
        fs::write(g.join("UserData/MelonPreferences.cfg"), b"keep").unwrap();
        let v1 = zip_of(&[
            ("Mods/", b""),
            ("Mods/WombatGD.dll", b"v1"),
            ("UserData/WombatGD/Sounds/old.wav", b"o"),
            ("UserData/WombatGD/Sounds/idle.wav", b"i"),
        ]);
        install(
            g,
            "WombatGD",
            "0.5.0",
            "oth-wombatgd",
            "WombatGD-0.5.0.zip",
            &v1,
        )
        .unwrap();
        let v2 = zip_of(&[
            ("Mods/WombatGD.dll", b"v2"),
            ("UserData/WombatGD/Sounds/idle.wav", b"i2"),
        ]);
        install(
            g,
            "WombatGD",
            "0.6.0",
            "oth-wombatgd",
            "WombatGD-0.6.0.zip",
            &v2,
        )
        .unwrap();
        assert!(!g.join("UserData/WombatGD/Sounds/old.wav").exists());
        assert_eq!(fs::read(g.join("Mods/WombatGD.dll")).unwrap(), b"v2");
        assert_eq!(
            state(g, &load_manifest(g), "WombatGD"),
            LocalState::Installed("0.6.0".into())
        );

        uninstall(g, "WombatGD").unwrap();
        assert!(!g.join("UserData/WombatGD").exists());
        assert_eq!(
            fs::read(g.join("UserData/MelonPreferences.cfg")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn malicious_zip_writes_nothing() {
        let t = tempfile::tempdir().unwrap();
        let g = t.path().join("game");
        fs::create_dir_all(&g).unwrap();
        let bad = zip_of(&[("Mods/ok.dll", b"x"), ("../outside.txt", b"x")]);
        assert!(install(&g, "Bad", "1", "oth-bad", "Bad.zip", &bad).is_err());
        assert!(!g.join("Mods/ok.dll").exists());
        assert!(!t.path().join("outside.txt").exists());
        assert!(install(&g, "Bad", "1", "oth-bad", "Bad.zip", b"not a zip").is_err());
    }

    #[test]
    fn manual_install_detected_and_removed() {
        let t = tempfile::tempdir().unwrap();
        let g = t.path();
        fs::create_dir_all(g.join("Mods")).unwrap();
        fs::write(g.join("Mods/FuelManagement.dll"), b"x").unwrap();
        assert_eq!(
            state(g, &load_manifest(g), "FuelManagement"),
            LocalState::Manual
        );
        uninstall(g, "FuelManagement").unwrap();
        assert!(!g.join("Mods/FuelManagement.dll").exists());
    }

    #[test]
    fn melonloader_extracts_to_root() {
        let t = tempfile::tempdir().unwrap();
        let g = t.path();
        let z = zip_of(&[
            ("version.dll", b"p"),
            ("MelonLoader/net6/MelonLoader.dll", b"m"),
        ]);
        install_melonloader(g, &z).unwrap();
        assert!(g.join("version.dll").is_file());
        assert!(g.join("MelonLoader/net6/MelonLoader.dll").is_file());
        assert!(g.join("Mods").is_dir());
    }
}
