//! Finding and validating the Over the Hill install.

use std::path::{Path, PathBuf};

use crate::vdf;

pub const APP_ID: &str = "2929250";
pub const DEFAULT_FOLDER: &str = "over the hill";
/// Steam launch option Proton needs so Wine loads MelonLoader's `version.dll` proxy.
pub const PROTON_LAUNCH_OPTION: &str = r#"WINEDLLOVERRIDES="version=n,b" %command%"#;

/// Steam install roots worth looking in on this OS (existing or not; callers check).
pub fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let home = dirs::home_dir().unwrap_or_default();

    #[cfg(windows)]
    {
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        use winreg::RegKey;
        if let Ok(k) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam") {
            if let Ok(p) = k.get_value::<String, _>("SteamPath") {
                roots.push(PathBuf::from(p));
            }
        }
        for key in [r"SOFTWARE\WOW6432Node\Valve\Steam", r"SOFTWARE\Valve\Steam"] {
            if let Ok(k) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(key) {
                if let Ok(p) = k.get_value::<String, _>("InstallPath") {
                    roots.push(PathBuf::from(p));
                }
            }
        }
        roots.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));
        roots.push(PathBuf::from(r"C:\Program Files\Steam"));
    }

    #[cfg(target_os = "macos")]
    {
        roots.push(home.join("Library/Application Support/Steam"));
        // Windows Steam inside CrossOver / Whisky bottles (the mods need the Windows build).
        for bottles in [
            home.join("Library/Application Support/CrossOver/Bottles"),
            home.join("Library/Containers/com.isaacmarovitz.Whisky/Bottles"),
        ] {
            if let Ok(entries) = std::fs::read_dir(bottles) {
                for e in entries.flatten() {
                    roots.push(e.path().join("drive_c/Program Files (x86)/Steam"));
                }
            }
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        roots.push(home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
        roots.push(home.join(".local/share/Steam"));
        roots.push(home.join(".steam/steam"));
        roots.push(home.join(".steam/root"));
        roots.push(home.join("snap/steam/common/.local/share/Steam"));
    }

    let _ = &home;
    dedupe(roots)
}

fn dedupe(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = Vec::<PathBuf>::new();
    for p in paths {
        let key = p.canonicalize().unwrap_or_else(|_| p.clone());
        if !seen
            .iter()
            .any(|s| s.canonicalize().unwrap_or_else(|_| s.clone()) == key)
        {
            seen.push(p);
        }
    }
    seen
}

/// Library folders listed by a Steam root (the root itself is always one).
pub fn library_dirs(root: &Path) -> Vec<PathBuf> {
    let mut libs = vec![root.to_path_buf()];
    for file in [
        root.join("steamapps/libraryfolders.vdf"),
        root.join("config/libraryfolders.vdf"),
    ] {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Ok(doc) = vdf::parse(&text) else { continue };
        let Some(lf) = doc.get("libraryfolders") else {
            continue;
        };
        for (_, entry) in lf.children() {
            // Old format: "1" "D:\\Lib"; new format: "1" { "path" "D:\\Lib" }.
            let path = entry.as_str().or_else(|| entry.get("path")?.as_str());
            if let Some(p) = path {
                libs.push(PathBuf::from(p));
            }
        }
    }
    dedupe(libs)
}

/// The game folder inside one Steam library, if it's installed there.
pub fn find_in_library(lib: &Path) -> Option<PathBuf> {
    let steamapps = lib.join("steamapps");
    let manifest = steamapps.join(format!("appmanifest_{APP_ID}.acf"));
    if let Ok(text) = std::fs::read_to_string(manifest) {
        if let Some(dir) = vdf::parse(&text).ok().and_then(|d| {
            d.get("AppState")?
                .get("installdir")?
                .as_str()
                .map(String::from)
        }) {
            let p = steamapps.join("common").join(dir);
            if p.is_dir() {
                return Some(p);
            }
        }
    }
    let p = steamapps.join("common").join(DEFAULT_FOLDER);
    p.is_dir().then_some(p)
}

/// Searches every known Steam library for a valid install.
pub fn detect() -> Option<PathBuf> {
    steam_roots()
        .iter()
        .filter(|r| r.is_dir())
        .flat_map(|r| library_dirs(r))
        .filter_map(|lib| find_in_library(&lib))
        .find(|p| validate(p).is_ok())
}

/// Checks the folder is a Windows (IL2CPP) build of the game, the one MelonLoader mods run on.
pub fn validate(dir: &Path) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a folder.", dir.display()));
    }
    let has_data = std::fs::read_dir(dir)
        .map_err(|e| format!("Can't read {}: {e}", dir.display()))?
        .flatten()
        .any(|e| e.path().is_dir() && e.file_name().to_string_lossy().ends_with("_Data"));
    if dir.join("GameAssembly.dll").is_file() && has_data {
        return Ok(());
    }
    if dir.join("GameAssembly.so").is_file() || dir.extension().is_some_and(|e| e == "app") {
        return Err(
            "This is a native Linux/macOS build. The mods need the Windows build \
                    (run through Proton, CrossOver or Whisky)."
                .into(),
        );
    }
    Err(format!(
        "{} doesn't look like the Over the Hill folder (no GameAssembly.dll / *_Data).",
        dir.display()
    ))
}

#[derive(Debug, Clone, PartialEq)]
pub enum MelonStatus {
    Missing,
    /// Installed; version read from `MelonLoader/Latest.log` when available.
    Installed(Option<String>),
    /// The loader folder exists but the `version.dll` proxy is gone (or vice versa).
    Broken,
}

pub fn melon_status(game: &Path) -> MelonStatus {
    let core = game.join("MelonLoader/net6/MelonLoader.dll").is_file();
    let proxy = game.join("version.dll").is_file();
    match (core, proxy) {
        (false, false) => MelonStatus::Missing,
        (true, true) => MelonStatus::Installed(melon_version_from_log(game)),
        _ => MelonStatus::Broken,
    }
}

fn melon_version_from_log(game: &Path) -> Option<String> {
    let text = std::fs::read_to_string(game.join("MelonLoader/Latest.log")).ok()?;
    text.lines().take(40).find_map(|l| {
        let i = l.find("MelonLoader v")?;
        let rest = &l[i + "MelonLoader v".len()..];
        let v: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        (!v.is_empty()).then_some(v)
    })
}

/// On Linux the game runs under Proton when Steam made a Wine prefix for it.
pub fn runs_under_proton(game: &Path) -> bool {
    cfg!(all(unix, not(target_os = "macos")))
        && game
            .parent()
            .and_then(Path::parent)
            .is_some_and(|steamapps| steamapps.join("compatdata").join(APP_ID).is_dir())
}

/// Whether the game is running (installing over a running game fails or corrupts state).
pub fn is_running() -> bool {
    use sysinfo::{ProcessesToUpdate, System};
    let Ok(mut sys) = System::new() else {
        return false;
    };
    sys.refresh_processes(ProcessesToUpdate::All, true);
    sys.processes().values().any(|p| {
        let n = p.name().to_string_lossy().to_ascii_lowercase();
        n.starts_with("over the hill")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fake_game(dir: &Path) {
        fs::create_dir_all(dir.join("over the hill_Data")).unwrap();
        fs::write(dir.join("GameAssembly.dll"), b"").unwrap();
    }

    #[test]
    fn finds_game_through_library_folders_and_manifest() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("Steam");
        let lib = t.path().join("Lib2");
        fs::create_dir_all(root.join("steamapps")).unwrap();
        fs::write(
            root.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\" {{ \"0\" {{ \"path\" \"{}\" }} \"1\" {{ \"path\" \"{}\" }} }}",
                root.display().to_string().replace('\\', "\\\\"),
                lib.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let game = lib.join("steamapps/common/OTH");
        fake_game(&game);
        fs::write(
            lib.join(format!("steamapps/appmanifest_{APP_ID}.acf")),
            "\"AppState\" { \"appid\" \"2929250\" \"installdir\" \"OTH\" }",
        )
        .unwrap();

        let libs = library_dirs(&root);
        assert_eq!(libs.len(), 2);
        let found: Vec<_> = libs.iter().filter_map(|l| find_in_library(l)).collect();
        assert_eq!(found, vec![game.clone()]);
        assert!(validate(&game).is_ok());
    }

    #[test]
    fn falls_back_to_default_folder_name() {
        let t = tempfile::tempdir().unwrap();
        let game = t.path().join("steamapps/common").join(DEFAULT_FOLDER);
        fake_game(&game);
        assert_eq!(find_in_library(t.path()), Some(game));
    }

    #[test]
    fn rejects_non_game_folders() {
        let t = tempfile::tempdir().unwrap();
        assert!(validate(t.path()).is_err());
        assert!(validate(&t.path().join("nope")).is_err());
        fs::write(t.path().join("GameAssembly.so"), b"").unwrap();
        assert!(validate(t.path()).unwrap_err().contains("native"));
    }

    #[test]
    fn melonloader_status() {
        let t = tempfile::tempdir().unwrap();
        let g = t.path();
        assert_eq!(melon_status(g), MelonStatus::Missing);
        fs::create_dir_all(g.join("MelonLoader/net6")).unwrap();
        fs::write(g.join("MelonLoader/net6/MelonLoader.dll"), b"").unwrap();
        assert_eq!(melon_status(g), MelonStatus::Broken);
        fs::write(g.join("version.dll"), b"").unwrap();
        assert_eq!(melon_status(g), MelonStatus::Installed(None));
        fs::write(
            g.join("MelonLoader/Latest.log"),
            "[00:00:00.000] ------------------------------\n[00:00:00.001] MelonLoader v0.7.3 Open-Beta\n",
        )
        .unwrap();
        assert_eq!(
            melon_status(g),
            MelonStatus::Installed(Some("0.7.3".into()))
        );
    }
}
