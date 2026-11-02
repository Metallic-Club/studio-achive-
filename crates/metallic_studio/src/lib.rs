//! Metallic Studio: the editor application.
//!
//! This crate holds the app shell (window, theme, splash), the start screen, the template
//! handling, the editor, and the installer side: [`updater`] (update check, download, apply) and
//! [`github_auth`] (GitHub device sign-in and token storage).
//!
//! # The editor
//!
//! - [`editor::EditorState`]: the open game's world, [`selection::Selection`],
//!   [`commands::History`] (every edit is a [`commands::Command`]), tool settings, Output sink and
//!   [`play::PlayController`]. No UI.
//! - [`editor_ui::EditorUi`]: the window layout around the panels: [`viewport`] (3D view, picking,
//!   [`gizmo`]s, [`camera`]), [`explorer`], [`properties`], [`output`] and [`script_editor`].
//! - [`actions`] and [`keymap`]: toolbar, menu and keyboard commands as data.
//!
//! Seams for later work, documented in their modules: actions as data ([`actions`]), the tagged
//! non-blocking Output sink ([`output`]), play modes with start/stop/status hooks ([`play`]),
//! and the undo, selection and gizmo APIs the building tools use ([`commands`], [`selection`],
//! [`gizmo`]). Panels take colours, font sizes and metrics only from [`tokens::ui_tokens`].
//!
//! Building core (#213-#215): [`preview`] (the gesture overlay: one gesture, one undo step),
//! [`snapping`] (point and surface snapping), [`pivot`] (pivots and rigid motions) and the
//! drag behaviours in [`gizmo`] and [`viewport`]. [`smart_duplicate`] repeats a moved copy's
//! offset on Ctrl+D.
//!
//! [`colour`] holds the building colour popover, its colour maths and palette files; it also runs
//! as a demo screen (`--scene colour`).
//!
//! [`tools`] holds the pure cores of the building tools (shared geometry, scatter, selection,
//! measure, arrange). [`editor_tools`] wires them to the toolbar, keymap, viewport and undo;
//! [`editor_input_actions`] docks the [`input_actions`] panel with the editor's undo, and
//! [`editor_colour`] hosts the colour popover for Properties' Color3 rows. [`data_dirs`] comes
//! from main.
//!
//! [`splash`] is the startup window; [`title_bar`] is the window's own title bar on Windows;
//! [`widgets`] holds Studio's own buttons, plates, cards, toasts and the cut.
//!
//! Library code returns [`StudioError`]; only `main.rs` decides what to show on failure.

#![warn(missing_docs)]

pub mod actions;
pub mod app;
pub mod camera;
pub mod collisions;
pub mod colour;
pub mod commands;
pub mod context_bar;
pub mod data_dirs;
pub mod editor;
pub mod editor_colour;
pub mod editor_input_actions;
pub mod editor_tools;
pub mod editor_ui;
pub mod error;
pub mod explorer;
pub mod expr;
pub mod github_auth;
pub mod gizmo;
pub mod hints;
pub mod icons;
pub mod input_actions;
pub mod keymap;
pub mod licences;
pub mod output;
pub mod palette;
pub mod panel;
pub mod picking;
pub mod pivot;
pub mod play;
pub mod png_image;
pub mod preview;
pub mod properties;
pub mod recent;
pub mod screenshot;
pub mod script_editor;
pub mod selection;
pub mod settings;
pub mod settings_ui;
pub mod smart_duplicate;
pub mod snapping;
pub mod splash;
pub mod start_screen;
pub mod style_picker;
pub mod templates;
pub mod theme;
pub mod title_bar;
pub mod tokens;
pub mod toolbar;
pub mod tools;
pub mod transform;
pub mod updater;
pub mod viewport;
pub mod viewport_header;
pub mod widgets;

pub use error::StudioError;
