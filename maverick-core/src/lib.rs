#![warn(clippy::pedantic)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    clippy::module_name_repetitions,
    clippy::wildcard_imports,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::unreadable_literal,
    clippy::manual_let_else,
    clippy::semicolon_if_nothing_returned,
    clippy::items_after_statements,
    clippy::unused_self,
    clippy::should_implement_trait,
    clippy::struct_excessive_bools,
    clippy::return_self_not_must_use,
    clippy::many_single_char_names
)]

pub mod types;
pub mod wallpaper;

pub use types::{
    Action, Camera, Client, Column, Dir, Edge, Focus, FullscreenPolicy, FullscreenSnapshot,
    LayoutKind, Monitor, PendingFocus, Rect, ReservedArea, ReservedRegion, SizeHints, State,
    ViewportMode, WallpaperCmd, WinFlags, WindowId, WindowMode,
};
