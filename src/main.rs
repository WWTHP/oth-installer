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

const MELON_TAG: &str = "v0.7.3";
const MELON_ASSET: &str = "MelonLoader.x64.zip"; // the game is the Windows x64 build everywhere

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
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (tx, rx) = channel();
        let mut app = Self {
            game: None,
            game_error: None,
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
                app.game_error = Some("Couldn't find Over the Hill. Choose the game folder.".into())
            }
        }
        app.run(Job::Refresh, "Loading the mod list from GitHub…");
        app
    }

    fn set_game(&mut self, p: PathBuf) {
        match game::validate(&p) {
            Ok(()) => {
                config::save(&config::Config {
                    game_dir: Some(p.clone()),
                });
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
        if let Some(p) = dlg.pick_folder() {
            self.set_game(p);
        }
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
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let done = match job {
                Job::Refresh => Done::Mods(github::token().and_then(|t| github::list_mods(&t))),
                Job::Install(m) => Done::Op(do_install(&game.unwrap(), &m)),
                Job::Remove(name) => Done::Op(
                    install::uninstall(&game.unwrap(), &name).map(|()| format!("Removed {name}.")),
                ),
                Job::InstallMelon => Done::Op(do_install_melon(&game.unwrap())),
            };
            let _ = tx.send(done);
            ctx.request_repaint();
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
        ui.horizontal(|ui| {
            match &self.game {
                Some(g) => ui.monospace(g.display().to_string()),
                None => ui.colored_label(ui.visuals().warn_fg_color, "Not set"),
            };
        });
        if let Some(e) = &self.game_error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
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
                            Some("Not found in any Steam library. Use Browse… to choose it.".into())
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
                game::MelonStatus::Installed(Some(v)) => {
                    (format!("Installed (v{v})"), "Reinstall 0.7.3")
                }
                game::MelonStatus::Installed(None) => ("Installed".to_string(), "Reinstall 0.7.3"),
                game::MelonStatus::Broken => ("Incomplete install".to_string(), "Repair (0.7.3)"),
                game::MelonStatus::Missing => (
                    "Not installed: the mods need it".to_string(),
                    "Install MelonLoader 0.7.3",
                ),
            };
            let ok = matches!(self.melon, game::MelonStatus::Installed(_));
            let color = if ok {
                ui.visuals().text_color()
            } else {
                ui.visuals().warn_fg_color
            };
            ui.colored_label(color, text);
            if ui.add_enabled(idle, egui::Button::new(button)).clicked() {
                self.run(Job::InstallMelon, "Installing MelonLoader…");
            }
        });
        if game::runs_under_proton(&game) {
            ui.label("Proton: set the game's Steam launch options to");
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
                self.run(Job::Refresh, "Loading the mod list from GitHub…");
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
        let melon_ok = matches!(self.melon, game::MelonStatus::Installed(_));
        if !melon_ok {
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
            .filter(|(m, s)| match s {
                LocalState::Installed(v) => v != &m.version,
                _ => false,
            })
            .map(|(m, _)| m.clone())
            .collect();

        egui::ScrollArea::vertical()
            .auto_shrink([false, true])
            .show(ui, |ui| {
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
                                    LocalState::NotInstalled => (Some("Install"), false),
                                    LocalState::Installed(v) if v != &m.version => {
                                        (Some("Update"), true)
                                    }
                                    LocalState::Installed(_) => (Some("Reinstall"), true),
                                    LocalState::Manual => (Some("Replace"), true),
                                };
                                if let Some(p) = primary {
                                    if ui.add_enabled(idle, egui::Button::new(p)).clicked() {
                                        action = Some(Job::Install(m.clone()));
                                    }
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
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let msgs: Vec<String> = mods
                .iter()
                .map(|m| do_install(&game, m).unwrap_or_else(|e| format!("{}: {e}", m.name)))
                .collect();
            let _ = tx.send(Done::Op(Ok(msgs.join(" "))));
            ctx.request_repaint();
        });
    }
}

fn do_install(game: &std::path::Path, m: &RemoteMod) -> Result<String, String> {
    let token = github::token()?;
    let bytes = github::download_asset(&token, github::ORG, &m.repo, m.asset.id)?;
    github::verify(&bytes, m.sha256.as_deref(), &m.asset.name)?;
    install::install(game, &m.name, &m.version, &m.repo, &m.asset.name, &bytes)?;
    Ok(format!("Installed {} {}.", m.name, m.version))
}

fn do_install_melon(game: &std::path::Path) -> Result<String, String> {
    let token = github::token()?;
    let asset = github::release_asset(&token, "LavaGang", "MelonLoader", MELON_TAG, MELON_ASSET)?;
    let bytes = github::download_asset(&token, "LavaGang", "MelonLoader", asset.id)?;
    let digest = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"));
    github::verify(&bytes, digest, MELON_ASSET)?;
    install::install_melonloader(game, &bytes)?;
    Ok(format!(
        "Installed MelonLoader {MELON_TAG}. Start the game once to let it generate its files."
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
            self.game_section(ui);
            ui.separator();
            self.melon_section(ui);
            if self.game.is_some() {
                ui.separator();
            }
            self.mods_section(ui);
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("WWTHP Mod Installer")
            .with_inner_size([760.0, 560.0])
            .with_min_inner_size([520.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "WWTHP Mod Installer",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
