//! The Studio application: splash, start screen, and the editor for the opened game.

use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui::{self, TextureHandle};

use crate::colour::demo::ColourDemo;
use crate::editor_ui::{EditorExit, EditorUi};
use crate::licences::Licences;
use crate::png_image::{RgbaImage, decode_png};
use crate::recent::RecentProjects;
use crate::screenshot::ScreenshotJob;
use crate::settings::{self, SettingsStore, ThemeChoice};
use crate::settings_ui;
use crate::splash::{self, Splash};
use crate::start_screen::{self, StartAction, StartScreen};
use crate::templates::{TemplateInfo, list_templates, templates_dir};
use crate::title_bar::{self, TitleBar, TitleBarInput};
use crate::{StudioError, theme, widgets};

/// The brand icon (a placeholder until the final brand files arrive).
const ICON_PNG: &[u8] = include_bytes!("../../../branding/placeholder-icon.png");
/// Window title.
pub const WINDOW_TITLE: &str = "Metallic Studio";
/// The folder shown in screenshots (never touched).
const DEMO_LOCATION: &str = r"C:\Games";

/// Which screen a capture should show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scene {
    /// The splash.
    Splash,
    /// The start screen with a few recent games (default).
    #[default]
    Start,
    /// The start screen on a first run: no recent games.
    FirstRun,
    /// The start screen after pressing Create on a non-empty folder, with an error toast.
    Error,
    /// The editor with the bundled Parkour template open (nothing is saved).
    Editor,
    /// The same editor with Play running (the game drawn in the viewport).
    Playing,
    /// The same editor with the Ctrl+K palette open on a query.
    Palette,
    /// The colour popover demo (stand-in until the editor's Properties panel exists).
    Colour,
    /// Every Studio widget in every state (design review).
    Components,
    /// The start screen with the Settings dialog open.
    Settings,
    /// The start screen with the Third-party licences screen open.
    Licences,
    /// The same, on the Rust libraries list.
    LicencesCrates,
}

/// Command-line options (all for design review; creators start Studio without any).
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Save one frame to this PNG and exit.
    pub screenshot: Option<PathBuf>,
    /// Which screen to capture.
    pub scene: Scene,
    /// Parent folder for new games (shown in the start screen).
    pub location: Option<PathBuf>,
    /// Window size in points (`--size 1440x900`), to compare captures with the mockups.
    pub size: Option<[f32; 2]>,
    /// Theme for a capture (`--theme graphite|titanium|light|system`); the saved setting is not read.
    pub theme: Option<ThemeChoice>,
}

impl Options {
    /// Parses `--screenshot <path>`,
    /// `--scene splash|start|firstrun|error|editor|playing|palette|colour|components|settings|licences|licences-crates`,
    /// `--theme graphite|titanium|light|system`, `--location <dir>` and `--size <width>x<height>`.
    /// Unknown arguments are reported in the returned message.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut opts = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
            match arg.as_str() {
                "--screenshot" => opts.screenshot = Some(PathBuf::from(value("--screenshot")?)),
                "--location" => opts.location = Some(PathBuf::from(value("--location")?)),
                "--size" => opts.size = Some(parse_size(&value("--size")?)?),
                "--theme" => {
                    let name = value("--theme")?;
                    opts.theme = Some(
                        ThemeChoice::parse(&name).ok_or_else(|| format!("unknown theme {name}"))?,
                    );
                }
                "--scene" => {
                    opts.scene = match value("--scene")?.as_str() {
                        "splash" => Scene::Splash,
                        "start" => Scene::Start,
                        "firstrun" => Scene::FirstRun,
                        "error" => Scene::Error,
                        "editor" => Scene::Editor,
                        "playing" => Scene::Playing,
                        "palette" => Scene::Palette,
                        "colour" | "color" => Scene::Colour,
                        "components" => Scene::Components,
                        "settings" => Scene::Settings,
                        "licences" | "licenses" => Scene::Licences,
                        "licences-crates" => Scene::LicencesCrates,
                        other => return Err(format!("unknown scene {other}")),
                    }
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        Ok(opts)
    }
}

/// Parses `1440x900` into a size in points.
fn parse_size(text: &str) -> Result<[f32; 2], String> {
    let bad = || format!("--size wants <width>x<height>, got {text}");
    let (w, h) = text.split_once(['x', 'X']).ok_or_else(bad)?;
    let w: f32 = w.trim().parse().map_err(|_| bad())?;
    let h: f32 = h.trim().parse().map_err(|_| bad())?;
    if w < 320.0 || h < 240.0 {
        return Err(bad());
    }
    Ok([w, h])
}

enum Phase {
    Splash(Splash),
    Start,
    Opened(Box<EditorUi>),
    Colour(Box<ColourDemo>),
    Components,
}

/// What the startup steps have produced so far.
#[derive(Default)]
struct Boot {
    recent: Option<RecentProjects>,
    templates: Option<Vec<TemplateInfo>>,
}

/// The eframe application.
pub struct StudioApp {
    phase: Phase,
    boot: Boot,
    start: Option<StartScreen>,
    options: Options,
    icon: Option<TextureHandle>,
    screenshot: Option<ScreenshotJob>,
    /// The window still has the splash's borderless, see-through look.
    splash_window: bool,
    /// The user agreed to quit from the unsaved-changes prompt; the next close is let through.
    quit_confirmed: bool,
    /// Theme, interface size and reduced motion.
    settings: SettingsStore,
    /// The Settings dialog is showing.
    settings_open: bool,
    /// The Third-party licences screen, while it is showing.
    licences: Option<Licences>,
    /// The window's own title bar (Windows; see [`title_bar`]).
    title_bar: TitleBar,
    /// The open game's name, for the title bar.
    opened_name: String,
}

/// Decodes the embedded brand icon.
pub fn icon_image() -> Result<RgbaImage, StudioError> {
    decode_png(ICON_PNG, "branding/placeholder-icon.png")
}

impl Options {
    /// True when Studio opens with the splash (a normal start, or the splash capture), so the
    /// first window takes the splash's size and look.
    pub fn shows_splash(&self) -> bool {
        self.screenshot.is_none() || self.scene == Scene::Splash
    }
}

impl StudioApp {
    /// Builds the app and applies the theme. On a normal start the rest of the startup work runs
    /// one step per frame behind the splash; captures run it all here.
    pub fn new(ctx: &egui::Context, options: Options) -> Self {
        // The splash is always Graphite at full size; the saved settings are read behind it and
        // take effect when the start screen appears (no theme flash).
        let shot = options.screenshot.is_some();
        let mut settings = SettingsStore::default();
        if shot {
            // A capture never reads or writes the real config folder.
            settings.settings.theme = options.theme.unwrap_or_default();
        }
        theme::apply(ctx, theme::ThemeId::Graphite);
        if !options.shows_splash() {
            settings::apply_settings(ctx, &settings.settings);
        }
        let icon = icon_image().ok().map(|img| {
            let color = egui::ColorImage::from_rgba_unmultiplied(
                [img.width as usize, img.height as usize],
                &img.rgba,
            );
            ctx.load_texture("brand-icon", color, egui::TextureOptions::LINEAR)
        });
        let phase = match options.scene {
            Scene::Colour => {
                let mut demo = ColourDemo::new(!shot);
                if shot {
                    demo.open_at(egui::pos2(420.0, 120.0));
                }
                Phase::Colour(Box::new(demo))
            }
            Scene::Components => Phase::Components,
            Scene::Editor if shot => EditorUi::demo(&templates_dir().join("parkour"))
                .map_or(Phase::Start, |e| Phase::Opened(Box::new(e))),
            Scene::Playing if shot => {
                EditorUi::demo(&templates_dir().join("parkour")).map_or(Phase::Start, |mut e| {
                    e.state.start_play("play.start");
                    Phase::Opened(Box::new(e))
                })
            }
            Scene::Palette if shot => {
                EditorUi::demo(&templates_dir().join("parkour")).map_or(Phase::Start, |mut e| {
                    e.open_command_palette_with("check");
                    Phase::Opened(Box::new(e))
                })
            }
            // The capture shows the second step running, like the mockup.
            Scene::Splash if shot => Phase::Splash(Splash::held_at(1)),
            _ if shot => Phase::Start,
            _ => Phase::Splash(Splash::new()),
        };
        let mut app = Self {
            phase,
            boot: Boot::default(),
            start: None,
            splash_window: options.shows_splash(),
            quit_confirmed: false,
            settings,
            settings_open: options.scene == Scene::Settings && shot,
            licences: match options.scene {
                Scene::Licences if shot => Some(Licences::default()),
                Scene::LicencesCrates if shot => Some(Licences::on_crates()),
                _ => None,
            },
            screenshot: options.screenshot.clone().map(ScreenshotJob::new),
            title_bar: TitleBar::new(),
            opened_name: if options.scene == Scene::Editor {
                "Parkour".to_owned()
            } else {
                String::new()
            },
            options,
            icon,
        };
        if !matches!(app.phase, Phase::Splash(_)) {
            for step in 0..splash::STEPS.len() {
                app.run_step(ctx, step);
            }
        }
        app
    }

    /// Does the work behind one splash step (`splash::STEPS[step]`).
    fn run_step(&mut self, ctx: &egui::Context, step: usize) {
        match step {
            // Fonts were installed in `new` and are built for the first frame; read settings.
            0 => {
                self.boot.recent = Some(recent_projects(self.options.screenshot.is_some()));
                if self.options.screenshot.is_none() {
                    self.settings = SettingsStore::load_default();
                    // The rest of the splash already honours reduced motion.
                    theme::set_reduce_motion(self.settings.settings.reduce_motion);
                }
            }
            // Nothing to call: this step ends once the first frame has been drawn and presented,
            // which is when it runs (one step per frame, after painting).
            1 => {}
            2 => self.boot.templates = Some(list_templates(&templates_dir())),
            _ => {
                let templates = self.boot.templates.take().unwrap_or_default();
                let recent = self.boot.recent.take().unwrap_or_default();
                let shot = self.options.screenshot.is_some();
                let scene = self.options.scene;
                let recent = if shot
                    && matches!(
                        scene,
                        Scene::Start | Scene::Error | Scene::Settings | Scene::Licences
                    ) {
                    start_screen::demo_recent(&templates_dir())
                } else {
                    recent
                };
                let mut start = StartScreen::new(templates, recent);
                if shot {
                    // Display only: nothing is written, and no real path or user name can end up
                    // in a picture.
                    start.set_location(Path::new(DEMO_LOCATION));
                    if scene != Scene::FirstRun {
                        start.select_id("parkour");
                    }
                    if scene == Scene::Error {
                        start.set_error(&StudioError::FolderNotEmpty(PathBuf::new()).to_string());
                        let gone = templates_dir().join("arena-prototype");
                        start.report_missing("Arena Prototype", &gone);
                    }
                } else if let Some(location) = &self.options.location {
                    start.set_location(location);
                }
                start.preload(ctx);
                self.start = Some(start);
            }
        }
    }
}

impl eframe::App for StudioApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // While Play runs, Tab, Space and Enter go to the game before egui can use them.
        if let Phase::Opened(editor) = &mut self.phase {
            editor.raw_input_hook(raw_input);
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.splash_window {
            [0.0; 4]
        } else {
            theme::bg_base().to_normalized_gamma_f32()
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.splash_window {
            // Cheap when nothing changed; follows the system theme while "System" is chosen.
            settings::apply_settings(&ctx, &self.settings.settings);
        }
        self.title_bar(ui, frame);
        match &mut self.phase {
            Phase::Colour(demo) => {
                if demo.show(ui) {
                    self.phase = Phase::Start;
                    ctx.request_repaint();
                }
            }
            Phase::Splash(splash) => {
                splash.show(ui, self.icon.as_ref());
                // Paint first, then work: the step named on screen is the one running.
                if let Some(step) = splash.next_step() {
                    self.run_step(&ctx, step);
                    if let Phase::Splash(splash) = &mut self.phase {
                        splash.step_done();
                    }
                    ctx.request_repaint();
                } else if splash.finished() {
                    splash::become_main_window(&ctx);
                    self.splash_window = false;
                    settings::apply_settings(&ctx, &self.settings.settings);
                    self.phase = Phase::Start;
                    ctx.request_repaint();
                } else {
                    ctx.request_repaint_after(Duration::from_millis(30));
                }
            }
            Phase::Start => {
                let action = self.start.as_mut().and_then(|start| start.show(ui));
                match action {
                    Some(StartAction::OpenProject { name, path }) => match EditorUi::open(&path) {
                        Ok(mut editor) => {
                            editor.hints = crate::hints::Hints::load_default();
                            self.phase = Phase::Opened(Box::new(editor));
                            self.opened_name = name;
                        }
                        Err(e) => {
                            log::warn!("could not open {name}: {e}");
                            if let Some(start) = &mut self.start {
                                start.open_failed(&name, &e);
                            }
                        }
                    },
                    Some(StartAction::OpenSettings) => self.settings_open = true,
                    Some(StartAction::OpenLicences) => self.licences = Some(Licences::default()),
                    None => {}
                }
            }
            Phase::Components => widgets::components::show(ui, self.icon.as_ref()),
            Phase::Opened(editor) => {
                // Closing the window with unsaved changes asks first (#319 review).
                if ctx.input(|i| i.viewport().close_requested())
                    && !self.quit_confirmed
                    && !editor.request_quit()
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
                editor.hints.show_tips = self.settings.settings.show_tips;
                editor.dialog_open = self.settings_open || self.licences.is_some();
                let exit = editor.ui(ui, frame.wgpu_render_state());
                if editor.take_settings_request() {
                    self.settings_open = true;
                }
                match exit {
                    EditorExit::Stay => {}
                    EditorExit::Close => self.phase = Phase::Start,
                    EditorExit::Quit => {
                        self.quit_confirmed = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
        }
        if self.settings_open {
            let out = settings_ui::show(&ctx, &mut self.settings.settings);
            if out.changed {
                // Repaint in the new look at once, then remember it.
                settings::apply_settings(&ctx, &self.settings.settings);
                self.settings.save();
                ctx.request_repaint();
            }
            if out.reset_tips {
                match &mut self.phase {
                    Phase::Opened(editor) => editor.hints.reset(),
                    _ => crate::hints::Hints::load_default().reset(),
                }
            }
            if out.licences {
                self.settings_open = false;
                self.licences = Some(Licences::default());
            }
            if out.close {
                self.settings_open = false;
            }
        }
        if let Some(screen) = &mut self.licences
            && screen.show(&ctx)
        {
            self.licences = None;
        }
        if let Some(job) = &mut self.screenshot {
            job.tick(&ctx);
        }
    }
}

impl StudioApp {
    /// The window's own title bar, as a 40 px top panel above every screen (Windows only; the
    /// splash has none).
    fn title_bar(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        if !title_bar::CUSTOM {
            return;
        }
        // Hook the window on the first frame, splash included, so the window never shows with
        // winit's non-client strip once it is the main window (see title_bar.rs).
        self.title_bar.attach(frame, ui.ctx());
        if self.splash_window {
            self.title_bar.hide();
            return;
        }
        let input = TitleBarInput {
            title: match self.phase {
                Phase::Opened(_) => self.opened_name.as_str(),
                _ => WINDOW_TITLE,
            },
            icon: self.icon.as_ref(),
            // In the editor Ctrl+K is grid snapping; on the start screen it is the search's.
            start_screen: !matches!(self.phase, Phase::Opened(_)),
            search_shortcut: matches!(self.phase, Phase::Start)
                && !self.settings_open
                && self.licences.is_none(),
        };
        let bar = &mut self.title_bar;
        let out = egui::Panel::top("title_bar")
            .exact_size(title_bar::HEIGHT)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new())
            .show(ui, |ui| bar.show(ui, &input))
            .inner;
        if out.open_settings {
            self.settings_open = true;
        }
    }
}

/// Screenshot mode keeps the list in memory so it never reads or writes the real config folder.
fn recent_projects(screenshot: bool) -> RecentProjects {
    if screenshot {
        RecentProjects::load(None)
    } else {
        RecentProjects::load_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_scene_is_parsed() {
        let opts = Options::parse(["--scene".to_owned(), "colour".to_owned()]).unwrap();
        assert_eq!(opts.scene, Scene::Colour);
    }

    #[test]
    fn size_and_components_scene_are_parsed() {
        let args = ["--scene", "components", "--size", "1440x900"].map(str::to_owned);
        let opts = Options::parse(args).unwrap();
        assert_eq!(opts.scene, Scene::Components);
        assert_eq!(opts.size, Some([1440.0, 900.0]));
        assert!(Options::parse(["--size", "big"].map(str::to_owned)).is_err());
        assert!(Options::parse(["--size", "10x10"].map(str::to_owned)).is_err());
    }

    #[test]
    fn licences_scene_is_parsed() {
        let opts = Options::parse(["--scene", "licences"].map(str::to_owned)).unwrap();
        assert_eq!(opts.scene, Scene::Licences);
    }

    #[test]
    fn settings_scene_and_theme_are_parsed() {
        let args = ["--scene", "settings", "--theme", "Titanium"].map(str::to_owned);
        let opts = Options::parse(args).unwrap();
        assert_eq!(opts.scene, Scene::Settings);
        assert_eq!(opts.theme, Some(ThemeChoice::Titanium));
        assert!(Options::parse(["--theme", "neon"].map(str::to_owned)).is_err());
        assert!(Options::parse(["--theme".to_owned()]).is_err());
    }

    #[test]
    fn screenshot_mode_never_uses_the_real_config_dir() {
        assert!(!recent_projects(true).is_persistent());
    }
}
