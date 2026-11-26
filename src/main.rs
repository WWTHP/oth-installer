#![cfg_attr(windows, windows_subsystem = "windows")]
//! WWTHP mod installer for Over the Hill: finds the game, installs/removes the mods and MelonLoader.

mod config;
mod game;
mod github;
mod install;
mod vdf;

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

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
    Refresh,
    Install(RemoteMod),
    Remove(String),
    InstallMelon,
}

enum Done {
    Mods(Result<Vec<RemoteMod>, String>),
    Op(Result<String, String>),
}

struct App {
    game: Option<PathBuf>,
    game_error: Option<String>,
    path_input: String,
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
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        self.run(Job::Refresh, "Loading the mod list from GitHub…");
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
        let needs_game = !matches!(job, Job::Refresh);
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
            Job::Refresh => Done::Mods(github::list_mods()),
            Job::Install(m) => Done::Op(do_install(&game.unwrap(), &m)),
            Job::Remove(name) => Done::Op(
                install::uninstall(&game.unwrap(), &name).map(|()| format!("Removed {name}.")),
            ),
            Job::InstallMelon => Done::Op(do_install_melon(&game.unwrap())),
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
            self.busy = None;
            match done {
                Done::Mods(Ok(m)) => {
                    self.mods = m;
                    self.mods_error = None;
                }
                Done::Mods(Err(e)) => self.mods_error = Some(e),
                Done::Op(r) => {
                    let ok = r.is_ok();
                    self.log(ok, r.unwrap_or_else(|e| e));
                    self.reload_local();
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
                self.refresh();
            }
        });
        if let Some(e) = &self.mods_error {
            ui.colored_label(ui.visuals().error_fg_color, e);
            return;
        }
        if self.game.is_none() {
            ui.label("Choose the game folder first.");
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
            ui.label("No released mods found.");
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

fn do_install(game: &std::path::Path, m: &RemoteMod) -> Result<String, String> {
    let bytes = github::download(&m.asset.browser_download_url)?;
    github::verify(&bytes, m.sha256.as_deref(), &m.asset.name)?;
    install::install(game, &m.name, &m.version, &m.repo, &m.asset.name, &bytes)?;
    Ok(format!("Installed {} {}.", m.name, m.version))
}

fn do_install_melon(game: &std::path::Path) -> Result<String, String> {
    let bytes = github::download(MELON_URL)?;
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
