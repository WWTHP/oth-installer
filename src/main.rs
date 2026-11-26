#![cfg_attr(windows, windows_subsystem = "windows")]
//! WWTHP mod installer for Over the Hill: finds the game, installs/removes the mods and MelonLoader.

mod auth;
mod config;
mod game;
mod github;
mod install;
mod vdf;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use github::RemoteMod;
use install::LocalState;

const MELON_VERSION: &str = "0.7.3";
/// The game is the Windows x64 build everywhere (Proton/CrossOver included).
const MELON_URL: &str =
    "https://github.com/LavaGang/MelonLoader/releases/download/v0.7.3/MelonLoader.x64.zip";
/// Pinned: GitHub's digest for that asset.
const MELON_SHA256: &str = "5b2b2f3d1cd42b59ec886c5bdc2663edae87a0097a4f4a8f58c0965a99dda416";

enum Job {
    /// Load the mod list; `Some(token)` = check and save a newly entered token first.
    Refresh(Option<String>),
    Install(RemoteMod),
    Remove(String),
    InstallMelon,
}

struct Account {
    login: String,
    source: auth::Source,
}

enum Done {
    Mods(Result<(Account, Vec<RemoteMod>), String>),
    Op(Result<String, String>),
    DeviceCode(Result<auth::DeviceCode, String>),
    SignedIn(Result<String, String>),
}

struct SignIn {
    code: Option<auth::DeviceCode>,
    cancel: Arc<AtomicBool>,
}

struct App {
    game: Option<PathBuf>,
    game_error: Option<String>,
    path_input: String,
    account: Option<Account>,
    needs_login: bool,
    token_input: String,
    signin: Option<SignIn>,
    signin_error: Option<String>,
    mods: Vec<RemoteMod>,
    mods_error: Option<String>,
    manifest: install::Manifest,
    melon: game::MelonStatus,
    busy: Option<String>,
    log: Vec<(bool, String)>,
    tx: Sender<Done>,
    rx: Receiver<Done>,
    ctx: egui::Context,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, deck: bool) -> Self {
        if deck {
            cc.egui_ctx.set_zoom_factor(1.4);
        }
        let (tx, rx) = channel();
        let mut app = Self {
            game: None,
            game_error: None,
            path_input: String::new(),
            account: None,
            needs_login: false,
            token_input: String::new(),
            signin: None,
            signin_error: None,
            mods: Vec::new(),
            mods_error: None,
            manifest: install::Manifest::default(),
            melon: game::MelonStatus::Missing,
            busy: None,
            log: Vec::new(),
            tx,
            rx,
            ctx: cc.egui_ctx.clone(),
        };
        let saved = config::load()
            .game_dir
            .filter(|p| game::validate(p).is_ok());
        match saved.or_else(game::detect) {
            Some(p) => app.set_game(p),
            None => {
                app.game_error = Some(
                    "Couldn't find Over the Hill. Choose the game folder, or type its path.".into(),
                )
            }
        }
        app.refresh(None);
        app
    }

    fn refresh(&mut self, new_token: Option<String>) {
        self.run(Job::Refresh(new_token), "Loading the mod list from GitHub…");
    }

    fn set_game(&mut self, p: PathBuf) {
        match game::validate(&p) {
            Ok(()) => {
                config::save(&config::Config {
                    game_dir: Some(p.clone()),
                });
                self.path_input = p.display().to_string();
                self.game = Some(p);
                self.game_error = None;
                self.reload_local();
            }
            Err(e) => self.game_error = Some(e),
        }
    }

    fn reload_local(&mut self) {
        if let Some(g) = &self.game {
            self.manifest = install::load_manifest(g);
            self.melon = game::melon_status(g);
        }
    }

    fn browse(&mut self) {
        let mut dlg = rfd::FileDialog::new().set_title("Select the Over the Hill game folder");
        if let Some(g) = &self.game {
            dlg = dlg.set_directory(g);
        }
        // No portal (e.g. Steam Deck Game Mode) = no dialog; the path box still works.
        if let Some(p) = dlg.pick_folder() {
            self.set_game(p);
        }
    }

    fn spawn(&self, f: impl FnOnce() -> Done + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
    }

    fn run(&mut self, job: Job, label: &str) {
        if self.busy.is_some() {
            return;
        }
        let needs_game = !matches!(job, Job::Refresh(_));
        if needs_game && game::is_running() {
            self.log(
                false,
                "Over the Hill is running. Quit the game first.".into(),
            );
            return;
        }
        let game = self.game.clone();
        if needs_game && game.is_none() {
            return;
        }
        self.busy = Some(label.to_string());
        self.spawn(move || match job {
            Job::Refresh(t) => Done::Mods(load_mods(t)),
            Job::Install(m) => Done::Op(do_install(&game.unwrap(), &m)),
            Job::Remove(name) => Done::Op(
                install::uninstall(&game.unwrap(), &name).map(|()| format!("Removed {name}.")),
            ),
            Job::InstallMelon => Done::Op(do_install_melon(&game.unwrap())),
        });
    }

    fn start_signin(&mut self, client_id: &'static str) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.signin = Some(SignIn {
            code: None,
            cancel: cancel.clone(),
        });
        self.signin_error = None;
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let code = match auth::start_device_flow(client_id) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Done::DeviceCode(Err(e)));
                    ctx.request_repaint();
                    return;
                }
            };
            let _ = tx.send(Done::DeviceCode(Ok(code.clone())));
            ctx.request_repaint();
            let deadline = Instant::now() + Duration::from_secs(code.expires_in);
            let mut interval = code.interval.max(5);
            let result = loop {
                // Sleep in short steps so Cancel is prompt.
                let wake = Instant::now() + Duration::from_secs(interval);
                while Instant::now() < wake {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                if Instant::now() > deadline {
                    break Err("The sign-in code expired. Start again.".to_string());
                }
                match auth::poll_device_flow(client_id, &code.device_code) {
                    auth::PollResult::Pending => {}
                    auth::PollResult::SlowDown => interval += 5,
                    auth::PollResult::Token(t) => break Ok(t),
                    auth::PollResult::Failed(e) => break Err(e),
                }
            };
            if !cancel.load(Ordering::Relaxed) {
                let _ = tx.send(Done::SignedIn(result));
                ctx.request_repaint();
            }
        });
    }

    fn log(&mut self, ok: bool, msg: String) {
        self.log.push((ok, msg));
        if self.log.len() > 50 {
            self.log.remove(0);
        }
    }

    fn poll(&mut self) {
        while let Ok(done) = self.rx.try_recv() {
            match done {
                Done::Mods(r) => {
                    self.busy = None;
                    match r {
                        Ok((acct, m)) => {
                            self.account = Some(acct);
                            self.needs_login = false;
                            self.token_input.clear();
                            self.mods = m;
                            self.mods_error = None;
                        }
                        Err(e) => {
                            let auth_problem = e.starts_with(github::AUTH_REJECTED)
                                || e.starts_with(auth::NOT_SIGNED_IN);
                            if auth_problem {
                                self.account = None;
                                self.needs_login = true;
                                self.mods.clear();
                            }
                            self.mods_error = Some(e);
                        }
                    }
                }
                Done::Op(r) => {
                    self.busy = None;
                    let ok = r.is_ok();
                    self.log(ok, r.unwrap_or_else(|e| e));
                    self.reload_local();
                }
                Done::DeviceCode(Ok(code)) => {
                    if let Some(s) = &mut self.signin {
                        s.code = Some(code);
                    }
                }
                Done::DeviceCode(Err(e)) | Done::SignedIn(Err(e)) => {
                    self.signin = None;
                    self.signin_error = Some(e);
                }
                Done::SignedIn(Ok(token)) => {
                    self.signin = None;
                    self.refresh(Some(token));
                }
            }
        }
    }

    fn state_of(&self, name: &str) -> LocalState {
        match &self.game {
            Some(g) => install::state(g, &self.manifest, name),
            None => LocalState::NotInstalled,
        }
    }

    fn account_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("GitHub account");
        if let Some(a) = &self.account {
            let text = format!("Signed in as {} (via {})", a.login, a.source.label());
            let can_sign_out = a.source == auth::Source::Saved;
            let mut sign_out = false;
            ui.horizontal(|ui| {
                ui.label(text);
                sign_out = can_sign_out && ui.button("Sign out").clicked();
            });
            if sign_out {
                auth::forget();
                self.account = None;
                self.mods.clear();
                self.needs_login = true;
                self.mods_error = None;
            }
            return;
        }
        if !self.needs_login {
            ui.weak("Checking…");
            return;
        }
        ui.label("The WWTHP mods are private: sign in with a GitHub account that has access.");
        if let Some(e) = &self.signin_error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }

        if let Some(cid) = auth::client_id() {
            match self.signin.as_ref().map(|s| s.code.clone()) {
                None => {
                    if ui
                        .add_enabled(
                            self.busy.is_none(),
                            egui::Button::new("Sign in with GitHub"),
                        )
                        .clicked()
                    {
                        self.start_signin(cid);
                    }
                }
                Some(None) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Asking GitHub for a code…");
                    });
                }
                Some(Some(code)) => self.device_code_ui(ui, &code),
            }
            ui.add_space(6.0);
        }

        egui::CollapsingHeader::new("Or paste a token")
            .default_open(auth::client_id().is_none())
            .show(ui, |ui| {
                ui.label(
                    "Create a classic token with the \"repo\" scope at github.com → Settings → \
                     Developer settings → Personal access tokens, then paste it here.",
                );
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.token_input)
                            .password(true)
                            .hint_text("ghp_…")
                            .desired_width(280.0),
                    );
                    let ok = !self.token_input.trim().is_empty() && self.busy.is_none();
                    if ui.add_enabled(ok, egui::Button::new("Use token")).clicked() {
                        let t = self.token_input.trim().to_string();
                        self.refresh(Some(t));
                    }
                });
                ui.weak("With the GitHub CLI installed, `gh auth login` also works; then press Refresh.");
            });
    }

    fn device_code_ui(&mut self, ui: &mut egui::Ui, code: &auth::DeviceCode) {
        ui.horizontal(|ui| {
            qr_code(ui, &code.verification_uri, 150.0);
            ui.vertical(|ui| {
                ui.label("On any phone or computer, open");
                ui.horizontal(|ui| {
                    ui.hyperlink(&code.verification_uri);
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(code.verification_uri.clone());
                    }
                });
                ui.label("and enter this code:");
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(&code.user_code)
                            .monospace()
                            .size(28.0)
                            .strong(),
                    );
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(code.user_code.clone());
                    }
                });
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Waiting for you to approve…");
                    if ui.button("Cancel").clicked() {
                        if let Some(s) = self.signin.take() {
                            s.cancel.store(true, Ordering::Relaxed);
                        }
                    }
                });
            });
        });
    }

    fn game_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Game folder");
        if let Some(e) = &self.game_error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.path_input)
                    .hint_text("Path to the \"over the hill\" folder")
                    .desired_width((ui.available_width() - 90.0).max(200.0)),
            );
            let entered = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let changed = self.game.as_ref().map(|g| g.display().to_string())
                != Some(self.path_input.trim().to_string());
            if (ui.add_enabled(changed, egui::Button::new("Use")).clicked() || entered) && changed {
                self.set_game(PathBuf::from(self.path_input.trim()));
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Browse…").clicked() {
                self.browse();
            }
            if ui
                .button("Detect")
                .on_hover_text("Search the Steam libraries again")
                .clicked()
            {
                match game::detect() {
                    Some(p) => self.set_game(p),
                    None => {
                        self.game_error =
                            Some("Not found in any Steam library. Browse… or type the path.".into())
                    }
                }
            }
        });
    }

    fn melon_section(&mut self, ui: &mut egui::Ui) {
        let Some(game) = self.game.clone() else {
            return;
        };
        ui.heading("MelonLoader");
        let idle = self.busy.is_none();
        ui.horizontal(|ui| {
            let (text, button) = match &self.melon {
                game::MelonStatus::Installed(Some(v)) => (format!("Installed (v{v})"), "Reinstall"),
                game::MelonStatus::Installed(None) => ("Installed".to_string(), "Reinstall"),
                game::MelonStatus::Broken => ("Incomplete install".to_string(), "Repair"),
                game::MelonStatus::Missing => {
                    ("Not installed: the mods need it".to_string(), "Install")
                }
            };
            let ok = matches!(self.melon, game::MelonStatus::Installed(_));
            let color = if ok {
                ui.visuals().text_color()
            } else {
                ui.visuals().warn_fg_color
            };
            ui.colored_label(color, text);
            let label = format!("{button} MelonLoader {MELON_VERSION}");
            if ui.add_enabled(idle, egui::Button::new(label)).clicked() {
                self.run(Job::InstallMelon, "Installing MelonLoader…");
            }
        });
        if game::runs_under_proton(&game) {
            ui.label(
                "Proton (Linux / Steam Deck): in Steam, open the game's Properties → General → \
                 Launch Options and enter",
            );
            ui.horizontal(|ui| {
                ui.code(game::PROTON_LAUNCH_OPTION);
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(game::PROTON_LAUNCH_OPTION.into());
                }
            });
        }
    }

    fn mods_section(&mut self, ui: &mut egui::Ui) {
        let idle = self.busy.is_none();
        ui.horizontal(|ui| {
            ui.heading("WWTHP mods");
            if ui.add_enabled(idle, egui::Button::new("Refresh")).clicked() {
                self.reload_local();
                self.refresh(None);
            }
        });
        if let Some(e) = &self.mods_error {
            if !self.needs_login {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            return;
        }
        if self.game.is_none() {
            ui.label("Choose the game folder first.");
            return;
        }
        if self.account.is_none() {
            return;
        }
        if !matches!(self.melon, game::MelonStatus::Installed(_)) {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Install MelonLoader too, or the mods won't load.",
            );
        }

        let mut action: Option<Job> = None;
        let rows: Vec<(RemoteMod, LocalState)> = self
            .mods
            .iter()
            .map(|m| (m.clone(), self.state_of(&m.name)))
            .collect();
        let pending: Vec<RemoteMod> = rows
            .iter()
            .filter(|(m, s)| matches!(s, LocalState::Installed(v) if v != &m.version))
            .map(|(m, _)| m.clone())
            .collect();

        egui::Grid::new("mods")
            .num_columns(4)
            .striped(true)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                ui.strong("Mod");
                ui.strong("Installed");
                ui.strong("Latest");
                ui.strong("");
                ui.end_row();
                for (m, st) in &rows {
                    ui.vertical(|ui| {
                        ui.set_max_width(320.0);
                        ui.hyperlink_to(&m.name, &m.release_url);
                        if !m.description.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&m.description).small().weak(),
                                )
                                .wrap(),
                            );
                        }
                    });
                    match st {
                        LocalState::NotInstalled => ui.weak("—"),
                        LocalState::Installed(v) => ui.label(v),
                        LocalState::Manual => ui.label("yes (manual)"),
                    };
                    ui.label(&m.version);
                    ui.horizontal(|ui| {
                        let (primary, show_remove) = match st {
                            LocalState::NotInstalled => ("Install", false),
                            LocalState::Installed(v) if v != &m.version => ("Update", true),
                            LocalState::Installed(_) => ("Reinstall", true),
                            LocalState::Manual => ("Replace", true),
                        };
                        if ui.add_enabled(idle, egui::Button::new(primary)).clicked() {
                            action = Some(Job::Install(m.clone()));
                        }
                        if show_remove
                            && ui.add_enabled(idle, egui::Button::new("Remove")).clicked()
                        {
                            action = Some(Job::Remove(m.name.clone()));
                        }
                    });
                    ui.end_row();
                }
            });
        if self.mods.is_empty() && idle {
            ui.label("No mods visible to this GitHub account. Ask for access to the WWTHP repos.");
        }
        if !pending.is_empty() {
            ui.add_space(6.0);
            let label = format!("Update all ({})", pending.len());
            if ui.add_enabled(idle, egui::Button::new(label)).clicked() {
                self.run_many(pending);
            }
        }
        if let Some(job) = action {
            let label = match &job {
                Job::Install(m) => format!("Installing {} {}…", m.name, m.version),
                Job::Remove(n) => format!("Removing {n}…"),
                _ => String::new(),
            };
            self.run(job, &label);
        }
    }

    fn run_many(&mut self, mods: Vec<RemoteMod>) {
        if game::is_running() {
            self.log(
                false,
                "Over the Hill is running. Quit the game first.".into(),
            );
            return;
        }
        let Some(game) = self.game.clone() else {
            return;
        };
        self.busy = Some(format!("Updating {} mods…", mods.len()));
        self.spawn(move || {
            let msgs: Vec<String> = mods
                .iter()
                .map(|m| do_install(&game, m).unwrap_or_else(|e| format!("{}: {e}", m.name)))
                .collect();
            Done::Op(Ok(msgs.join(" ")))
        });
    }
}

/// Draws a QR code (the sign-in URL, so a phone camera can open it).
fn qr_code(ui: &mut egui::Ui, data: &str, size: f32) {
    let Ok(code) = qrcode::QrCode::new(data.as_bytes()) else {
        return;
    };
    let w = code.width();
    let quiet = 2;
    let cells = (w + 2 * quiet) as f32;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 4.0, egui::Color32::WHITE);
    let cell = size / cells;
    for (i, c) in code.to_colors().iter().enumerate() {
        if *c == qrcode::Color::Dark {
            let (x, y) = ((i % w + quiet) as f32, (i / w + quiet) as f32);
            let min = rect.min + egui::vec2(x * cell, y * cell);
            p.rect_filled(
                egui::Rect::from_min_size(min, egui::vec2(cell + 0.5, cell + 0.5)),
                0.0,
                egui::Color32::BLACK,
            );
        }
    }
}

fn load_mods(new_token: Option<String>) -> Result<(Account, Vec<RemoteMod>), String> {
    let (token, source) = match new_token {
        Some(t) => (t, auth::Source::Saved),
        None => auth::find().ok_or_else(|| auth::NOT_SIGNED_IN.to_string())?,
    };
    let login = github::whoami(&token)?;
    if source == auth::Source::Saved {
        auth::save(&token)?;
    }
    let mods = github::list_mods(&token)?;
    Ok((Account { login, source }, mods))
}

fn do_install(game: &std::path::Path, m: &RemoteMod) -> Result<String, String> {
    let token = auth::token()?;
    let bytes = github::download_asset(&token, github::ORG, &m.repo, m.asset.id)?;
    github::verify(&bytes, m.sha256.as_deref(), &m.asset.name)?;
    install::install(game, &m.name, &m.version, &m.repo, &m.asset.name, &bytes)?;
    Ok(format!("Installed {} {}.", m.name, m.version))
}

fn do_install_melon(game: &std::path::Path) -> Result<String, String> {
    let bytes = github::download_public(MELON_URL)?;
    github::verify(&bytes, Some(MELON_SHA256), "MelonLoader")?;
    install::install_melonloader(game, &bytes)?;
    Ok(format!(
        "Installed MelonLoader {MELON_VERSION}. Start the game once to let it generate its files."
    ))
}

impl eframe::App for App {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(4.0);
            if let Some(b) = &self.busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(b);
                });
            }
            for (ok, msg) in self.log.iter().rev().take(3) {
                let c = if *ok {
                    ui.visuals().text_color()
                } else {
                    ui.visuals().error_fg_color
                };
                ui.colored_label(c, msg);
            }
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.account_section(ui);
                    ui.separator();
                    self.game_section(ui);
                    if self.game.is_some() {
                        ui.separator();
                        self.melon_section(ui);
                    }
                    ui.separator();
                    self.mods_section(ui);
                });
        });
    }
}

fn main() -> eframe::Result {
    let deck = game::is_steam_deck();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("WWTHP Mod Installer")
        .with_inner_size([800.0, 640.0])
        .with_min_inner_size([520.0, 400.0]);
    if deck {
        viewport = viewport.with_maximized(true);
    }
    eframe::run_native(
        "WWTHP Mod Installer",
        eframe::NativeOptions {
            viewport,
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(App::new(cc, deck)))),
    )
}
