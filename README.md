# oth-installer

**WWTHP Mod Installer**: a small desktop app (Windows, macOS, Linux) that installs, updates and
removes the WWTHP [Over the Hill](https://store.steampowered.com/app/2929250/) mods, plus
MelonLoader 0.7.3, which the mods need.

## Use it

1. Install the [GitHub CLI](https://cli.github.com) and run `gh auth login` with an account that can
   see the WWTHP repos (they are private; the installer borrows gh's token).
2. Download the installer for your OS from this repo's
   [Releases](https://github.com/WWTHP/oth-installer/releases) and run it.
   - macOS: the binary is unsigned. Unpack it, then right-click → Open (or `xattr -d com.apple.quarantine oth-installer`).
   - Linux: `tar xzf oth-installer-linux-x64.tar.gz && ./oth-installer`.
3. Quit the game. In the app: check the game folder, install MelonLoader if needed, then
   Install / Update / Remove each mod.

Linux (Proton): MelonLoader needs the Steam launch option `WINEDLLOVERRIDES="version=n,b" %command%`.
The app shows it with a Copy button.

## How it works

| Step | Detail |
| --- | --- |
| Find the game | Steam roots for the OS (registry on Windows; native, Flatpak and Snap Steam on Linux; Steam plus CrossOver/Whisky bottles on macOS) → `libraryfolders.vdf` → `appmanifest_2929250.acf`. If that fails, the app asks for the folder. The choice is saved in the OS config dir (`oth-installer/config.json`). |
| Validate | The folder must be the Windows IL2CPP build (`GameAssembly.dll` + `*_Data/`), the build MelonLoader mods run on. |
| Mod list | Every non-archived `oth-*` repo in the WWTHP org with a latest release. The release's `.zip` (laid out like the game folder) wins over a bare `.dll`. |
| Token | `GH_TOKEN` / `GITHUB_TOKEN`, else `gh auth token`. Asset downloads send it only to `api.github.com`, never to the storage redirect. |
| Integrity | SHA-256 checked against GitHub's asset digest or the hash in the release notes. A mismatch installs nothing. |
| Install | Zip entries must stay under `Mods/`, `UserData/`, `UserLibs/` or `Plugins/`; the whole archive is checked before any file is written. Files are written atomically. |
| Track | `<game>/UserData/WWTHP-Installer.json` records each mod's version and files. Updates delete files the new version dropped; Remove deletes exactly what was installed (or a hand-copied `Mods/<Mod>.dll`). Settings in `MelonPreferences.cfg` are kept. |
| Safety | Install/remove refuse while the game runs. |

## Develop

```bash
cargo run                         # the toolchain is pinned in rust-toolchain.toml
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
cargo test -- --ignored           # live: lists, downloads, verifies and installs every mod into a temp dir (needs gh login)
```

Release: bump `version` in `Cargo.toml`, merge, then `git tag v<version> && git push origin v<version>`.
The `release` workflow builds Windows x64, Linux x64 and macOS universal binaries and publishes them.

New mod repos need no change here: name the repo `oth-*` and publish a release with the DLL or zip
(the mod template's `tools/release.py` does this).
