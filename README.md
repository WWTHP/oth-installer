# oth-installer

**WWTHP Mod Installer**: a small desktop app (Windows, macOS, Linux, Steam Deck) that installs,
updates and removes the WWTHP [Over the Hill](https://store.steampowered.com/app/2929250/) mods,
plus MelonLoader 0.7.3, which the mods need. No account or sign-in needed.

## Download

| System | File |
| --- | --- |
| Windows | [oth-installer-windows-x64.exe](https://github.com/WWTHP/oth-installer/releases/latest/download/oth-installer-windows-x64.exe) |
| Linux / Steam Deck | [oth-installer-linux-x64.tar.gz](https://github.com/WWTHP/oth-installer/releases/latest/download/oth-installer-linux-x64.tar.gz) |
| macOS (Intel + Apple silicon) | [oth-installer-macos-universal.tar.gz](https://github.com/WWTHP/oth-installer/releases/latest/download/oth-installer-macos-universal.tar.gz) |

- Windows may show a SmartScreen warning (the exe isn't signed): More info → Run anyway.
- macOS: the binary is unsigned. Unpack it, then right-click → Open (or `xattr -d com.apple.quarantine oth-installer`).

## Use it

1. Quit the game and start the installer. It finds the game in your Steam libraries; if not,
   Browse… to the game folder or type its path.
2. Install MelonLoader if the app says it's missing.
3. Install / Update / Remove each mod. **Update all** appears when updates are out.

### Steam Deck

1. Switch to **Desktop Mode** (Steam button → Power → Switch to Desktop).
2. Download the Linux file above, open Downloads in Dolphin, right-click it → Extract → Extract
   archive here, then double-click `oth-installer`. It finds the game on internal storage or the SD card.
3. Install MelonLoader and the mods. Then, in Steam, open Over the Hill → Properties → General →
   Launch Options and paste `WINEDLLOVERRIDES="version=n,b" %command%` (the app has a Copy
   button). Without it Proton doesn't load MelonLoader.
4. Back to Gaming Mode and play.

On a Deck the app opens maximized with a larger UI; touch works. It also runs in Gaming Mode
(add it as a non-Steam game). There's no folder picker there, so type the path if detection fails.

## How it works

| Step | Detail |
| --- | --- |
| Find the game | Steam roots for the OS (registry on Windows; native, Flatpak and Snap Steam on Linux; Steam plus CrossOver/Whisky bottles on macOS) → `libraryfolders.vdf` → `appmanifest_2929250.acf`. Otherwise you pick or type the folder. The choice is saved in the OS config dir (`oth-installer/config.json`). |
| Validate | The folder must be the Windows IL2CPP build (`GameAssembly.dll` + `*_Data/`), the build MelonLoader mods run on. |
| Mod list | Every public, non-archived `oth-*` repo in the WWTHP org with a latest release, read anonymously from the GitHub API (about 7 requests per refresh; GitHub allows 60 an hour per network). The release's `.zip` (laid out like the game folder) wins over a bare `.dll`. |
| Integrity | SHA-256 checked against GitHub's asset digest or the hash in the release notes. A mismatch installs nothing. MelonLoader comes from LavaGang's release, checked against a pinned SHA-256. |
| Install | Zip entries must stay under `Mods/`, `UserData/`, `UserLibs/` or `Plugins/`; the whole archive is checked before any file is written. Files are written atomically. |
| Track | `<game>/UserData/WWTHP-Installer.json` records each mod's version and files. Updates delete files the new version dropped; Remove deletes exactly what was installed (or a hand-copied `Mods/<Mod>.dll`). Settings in `MelonPreferences.cfg` are kept. |
| Safety | Install/remove refuse while the game runs. |

## Develop

```bash
cargo run                         # the toolchain is pinned in rust-toolchain.toml
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
cargo test -- --ignored           # live: lists, downloads, verifies and installs every mod into a temp dir
```

Release: bump `version` in `Cargo.toml`, merge, then `git tag v<version> && git push origin v<version>`.
The `release` workflow builds Windows x64, Linux x64 and macOS universal binaries and publishes them.

New mod repos need no change here: name the repo `oth-*`, make it public and publish a release
with the DLL or zip (the mod template's `tools/release.py` does this).

Licence: MIT ([LICENSE](LICENSE)).
