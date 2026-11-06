//! Themes as GPUI Kit draws them. Which themes there are, how a terminal palette becomes
//! one, and importing them are `catmail_frontend::theme`'s; [`appearance`] turns them into
//! GPUI Kit themes and applies the settings' ones, and [`colors`] mixes the app's own
//! colors from a theme.

pub mod appearance;
pub mod colors;

pub use catmail_frontend::theme::{DEFAULT_DARK, DEFAULT_LIGHT, Rgb, TerminalPalette};
