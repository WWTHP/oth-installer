# oth-installer

**WWTHP Mod Installer**: a small desktop app (Windows, macOS, Linux) that installs, updates and
removes the WWTHP [Over the Hill](https://store.steampowered.com/app/2929250/) mods, plus
MelonLoader 0.7.3, which the mods need.

## Use it

1. Download the installer for your OS from this repo's
   [Releases](https://github.com/WWTHP/oth-installer/releases) and run it.
   - Windows: `oth-installer-windows-x64.exe`.
   - macOS: the binary is unsigned. Unpack it, then right-click → Open (or `xattr -d com.apple.quarantine oth-installer`).
   - Linux / Steam Deck: see below.
2. Sign in to GitHub (the mods are private). Any one of:
   - **Sign in with GitHub**: the app shows a code and a QR code; open github.com/login/device
     on any phone or computer and enter the code. Nothing to install.
   - **Paste a token**: a classic personal access token with the `repo` scope.
   - The GitHub CLI: if `gh auth login` was done on this machine, the app uses it automatically.
3. Quit the game. In the app: check the game folder, install MelonLoader if needed, then
   Install / Update / Remove each mod.

The saved sign-in lives in the OS config folder (`oth-installer/github-token`, readable only by you);
**Sign out** deletes it.

### Steam Deck

1. Switch to **Desktop Mode** (Steam button → Power → Switch to Desktop).
2. Download `oth-installer-linux-x64.tar.gz` (in a browser signed in to GitHub, since the repo is
   private, or copy it over from a PC on a USB stick or with KDE Connect), open the
   Downloads folder in Dolphin, right-click the file → Extract → Extract archive here, then double-click
   `oth-installer`.
3. **Sign in with GitHub** and enter the code on your phone. The app finds the game (internal
   storage or SD card) by itself.
4. Install MelonLoader and the mods. Then, in Steam, open Over the Hill → Properties → General →
   Launch Options and paste `WINEDLLOVERRIDES="version=n,b" %command%` (the app has a Copy
   button). Without it Proton doesn't load MelonLoader.
5. Back to Gaming Mode and play.

The app enlarges its UI and opens maximized on a Deck; touch works. It can also run in Gaming
Mode (add it as a non-Steam game). There's no folder picker there, so type the path if detection fails.

## How it works

| Step | Detail |
| --- | --- |
| Find the game | Steam roots for the OS (registry on Windows; native, Flatpak and Snap Steam on Linux; Steam plus CrossOver/Whisky bottles on macOS) → `libraryfolders.vdf` → `appmanifest_2929250.acf`. If that fails, the app asks for the folder. The choice is saved in the OS config dir (`oth-installer/config.json`). |
| Validate | The folder must be the Windows IL2CPP build (`GameAssembly.dll` + `*_Data/`), the build MelonLoader mods run on. |
| Mod list | Every non-archived `oth-*` repo in the WWTHP org with a latest release. The release's `.zip` (laid out like the game folder) wins over a bare `.dll`. |
| Token | `GH_TOKEN` / `GITHUB_TOKEN`, then the saved sign-in, then `gh auth token`. Sent only to `api.github.com`, never to the storage redirect that serves the download. |
| Integrity | SHA-256 checked against GitHub's asset digest or the hash in the release notes. A mismatch installs nothing. MelonLoader comes from LavaGang's public release, checked against a pinned SHA-256. |
| Install | Zip entries must stay under `Mods/`, `UserData/`, `UserLibs/` or `Plugins/`; the whole archive is checked before any file is written. Files are written atomically. |
| Track | `<game>/UserData/WWTHP-Installer.json` records each mod's version and files. Updates delete files the new version dropped; Remove deletes exactly what was installed (or a hand-copied `Mods/<Mod>.dll`). Settings in `MelonPreferences.cfg` are kept. |
| Safety | Install/remove refuse while the game runs. |

## Sign-in setup (once, by an org owner)

"Sign in with GitHub" needs a GitHub App owned by the WWTHP org. Its client id is not a secret.

1. github.com/organizations/WWTHP/settings/apps → **New GitHub App**.
   - Name: `WWTHP Mod Installer`. Homepage: this repo. Callback URL: leave empty.
   - **Enable Device Flow**: on. Webhook: off (uncheck Active).
   - Repository permissions: **Contents: Read-only** (Metadata: read-only is automatic). Nothing else.
   - Where can it be installed: only this account.
   - Optional features → opt out of **user-to-server token expiration** (otherwise people sign in again every 8 hours).
2. **Install App** → WWTHP → All repositories.
3. Copy the app's **Client ID** into a repo variable: Settings → Secrets and variables → Actions →
   Variables → `OTH_GITHUB_CLIENT_ID`. The release workflow builds it in.

A signed-in token can only read the repos the app is installed on and the user can already see.
Without the variable, builds still work, but offer only token paste and gh.

## Develop

```bash
cargo run                         # the toolchain is pinned in rust-toolchain.toml
OTH_GITHUB_CLIENT_ID=<id> cargo run   # with device sign-in
OTH_INSTALLER_NO_GH=1 cargo run   # ignore gh, to see the sign-in screen
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
cargo test -- --ignored           # live: lists, downloads, verifies and installs every mod into a temp dir (needs gh login)
```

Release: bump `version` in `Cargo.toml`, merge, then `git tag v<version> && git push origin v<version>`.
The `release` workflow builds Windows x64, Linux x64 and macOS universal binaries and publishes them.

New mod repos need no change here: name the repo `oth-*` and publish a release with the DLL or zip
(the mod template's `tools/release.py` does this).
