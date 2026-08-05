//! Atari-8bit-flavoured palette for the whole TUI.
//!
//! The look is a dark blue "screen" with an Atari-blue frame, amber titles
//! (amber monitor) and phosphor-green highlights. Everything the views draw
//! goes through the constants and helpers here so the style stays consistent.

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders};

/// Screen background — near black with a blue cast, like a CRT in a dark room.
pub const BG: Color = Color::Rgb(0x0b, 0x0f, 0x1a);
/// Panel borders (Atari blue).
pub const FRAME: Color = Color::Rgb(0x3f, 0x7f, 0xd0);
/// Normal text.
pub const TEXT: Color = Color::Rgb(0xd6, 0xec, 0xff);
/// Secondary text, labels, disabled rows.
pub const DIM: Color = Color::Rgb(0x5a, 0x7d, 0xa6);
/// Titles, keys, anything the eye should land on first (amber monitor).
pub const ACCENT: Color = Color::Rgb(0xff, 0xb0, 0x00);
/// Phosphor green — selections and live values.
pub const HILITE: Color = Color::Rgb(0x33, 0xff, 0x66);
pub const OK: Color = Color::Rgb(0x33, 0xff, 0x66);
pub const WARN: Color = Color::Rgb(0xff, 0xd2, 0x4d);
pub const ERR: Color = Color::Rgb(0xff, 0x55, 0x55);
pub const CYAN: Color = Color::Rgb(0x33, 0xd6, 0xff);
/// Background of a selected row.
pub const SEL_BG: Color = Color::Rgb(0x16, 0x3a, 0x66);

/// The Atari title-screen rainbow, used by the intro and the tab bar accents.
pub const RAINBOW: [Color; 7] = [
    Color::Rgb(0xff, 0x55, 0x55),
    Color::Rgb(0xff, 0xb0, 0x00),
    Color::Rgb(0xff, 0xe8, 0x4d),
    Color::Rgb(0x33, 0xff, 0x66),
    Color::Rgb(0x33, 0xd6, 0xff),
    Color::Rgb(0x7a, 0x5c, 0xff),
    Color::Rgb(0xff, 0x5c, 0xe0),
];

/// Base style for the whole screen.
pub fn screen() -> Style {
    Style::default().bg(BG).fg(TEXT)
}

/// Standard bordered panel with an amber title.
pub fn panel(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(FRAME))
        .style(Style::default().bg(BG))
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(ACCENT).bold(),
        ))
}

/// Heavier double-line panel for modals and the dashboard header.
pub fn panel_double(title: &str) -> Block<'static> {
    panel(title).border_type(BorderType::Double)
}

/// Dimmed caption, e.g. "root: ".
pub fn label(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(DIM))
}

/// A value belonging to a [`label`].
pub fn value(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(TEXT))
}

/// A keyboard key, highlighted so it pops out of the surrounding text.
pub fn key(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(ACCENT).bold())
}

/// Table header row style.
pub fn header() -> Style {
    Style::default().fg(CYAN).bold()
}

/// Selected list/table row.
pub fn selected() -> Style {
    Style::default().bg(SEL_BG).fg(HILITE).bold()
}

/// Strongly selected row (inside modals, where only one thing is in focus).
pub fn selected_strong() -> Style {
    Style::default().bg(ACCENT).fg(Color::Rgb(0x0b, 0x0f, 0x1a)).bold()
}
