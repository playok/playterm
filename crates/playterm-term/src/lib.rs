//! Terminal emulation core for playterm.
//!
//! Wraps `alacritty_terminal` with:
//! - per-session character set handling (UTF-8 / CP949),
//! - an event queue for things the terminal wants to send back to the remote side,
//! - an xterm-compatible color palette.

mod charset;
mod log;
mod palette;
mod terminal;

pub use alacritty_terminal::index::{Column, Line, Point, Side};
pub use alacritty_terminal::selection::{SelectionRange, SelectionType};
pub use alacritty_terminal::term::TermMode;
pub use alacritty_terminal::term::cell::{Cell, Flags};
pub use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
pub use charset::{Charset, InputEncoder, OutputDecoder};
pub use log::{LogMode, SessionLog};
pub use palette::Palette;
pub use terminal::{Terminal, TerminalEvent};
