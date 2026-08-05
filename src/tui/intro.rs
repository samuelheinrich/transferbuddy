//! Atari-style intro screen: a block-letter TRANSFER BUDDY logo that wipes in,
//! cycles through the classic rainbow bars, types out a subtitle and finishes
//! with a blinking READY prompt. The animation runs for roughly two seconds
//! (any key fast-forwards it) and then holds until Enter/Space/Esc.

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event as CEvent, KeyCode, KeyEventKind};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use super::theme;

/// 5x5 block glyphs — only the letters used by the logo exist.
fn glyph(c: char) -> [&'static str; 5] {
    match c {
        'T' => ["█████", "  █  ", "  █  ", "  █  ", "  █  "],
        'R' => ["████ ", "█   █", "████ ", "█  █ ", "█   █"],
        'A' => [" ███ ", "█   █", "█████", "█   █", "█   █"],
        'N' => ["█   █", "██  █", "█ █ █", "█  ██", "█   █"],
        'S' => [" ████", "█    ", " ███ ", "    █", "████ "],
        'F' => ["█████", "█    ", "████ ", "█    ", "█    "],
        'E' => ["█████", "█    ", "████ ", "█    ", "█████"],
        'B' => ["████ ", "█   █", "████ ", "█   █", "████ "],
        'U' => ["█   █", "█   █", "█   █", "█   █", " ███ "],
        'D' => ["████ ", "█   █", "█   █", "█   █", "████ "],
        'Y' => ["█   █", " █ █ ", "  █  ", "  █  ", "  █  "],
        _ => ["     ", "     ", "     ", "     ", "     "],
    }
}

/// Render `text` as five rows of block letters.
fn banner(text: &str) -> Vec<String> {
    (0..5)
        .map(|row| {
            text.chars()
                .map(|c| glyph(c)[row].to_string())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

const FRAMES: usize = 56;
const FRAME_MS: u64 = 38;
const SUBTITLE: &str = "MULTI PROTOCOL FILE TRANSFER FOR CISCO GEAR";

/// Play the intro, then hold the finished screen until the user starts the
/// TUI with Enter, Space or Esc. A key during the animation only fast-forwards
/// to the end — nobody falls into the TUI by accident.
pub fn play(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    sound: bool,
) -> Result<()> {
    crate::sound::play(sound, crate::sound::Tone::Boot);
    for frame in 0..FRAMES {
        terminal.draw(|f| draw(f, frame, false))?;
        if event::poll(Duration::from_millis(FRAME_MS))? {
            match event::read()? {
                CEvent::Key(k) if k.kind == KeyEventKind::Press => break,
                _ => {}
            }
        }
    }
    // Hold: the rainbow keeps cycling and the prompt blinks while we wait.
    let mut frame = FRAMES;
    loop {
        terminal.draw(|f| draw(f, frame, true))?;
        if event::poll(Duration::from_millis(120))? {
            if let CEvent::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press
                    && matches!(
                        k.code,
                        KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Esc
                    )
                {
                    return Ok(());
                }
            }
        }
        frame += 1;
    }
}

fn draw(f: &mut Frame, frame: usize, waiting: bool) {
    f.render_widget(
        ratatui::widgets::Block::default().style(theme::screen()),
        f.area(),
    );

    let top = banner("TRANSFER");
    let bottom = banner("BUDDY");
    let logo_width = top.first().map(|l| l.chars().count()).unwrap_or(0);
    // Centre the shorter word under the longer one.
    let centered = |row: &String| -> String {
        let lead = (logo_width.saturating_sub(row.chars().count())) / 2;
        format!("{}{row}", " ".repeat(lead))
    };

    // Phase 1: wipe the logo in from the left.
    let reveal = ((frame + 1) * logo_width / 14).min(logo_width);

    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in top.iter().chain(bottom.iter()).enumerate() {
        // Phase 2: rainbow bars scroll through the logo rows.
        let color = theme::RAINBOW[(i + frame / 2) % theme::RAINBOW.len()];
        let row = centered(row);
        let shown: String = row.chars().take(reveal).collect();
        let pad = " ".repeat(logo_width.saturating_sub(shown.chars().count()));
        lines.push(Line::from(Span::styled(
            format!("{shown}{pad}"),
            Style::default().fg(color).bold(),
        )));
        // Blank row between the two words.
        if i == 4 {
            lines.push(Line::from(""));
        }
    }

    lines.push(Line::from(""));

    // Phase 3: type out the subtitle.
    let typed = frame.saturating_sub(16) * 3;
    let subtitle: String = SUBTITLE.chars().take(typed).collect();
    lines.push(Line::from(Span::styled(
        subtitle,
        Style::default().fg(theme::CYAN),
    )));
    lines.push(Line::from(""));

    // Loading bar, Atari-cassette style.
    let bar_width = logo_width.min(48);
    let filled = ((frame + 1) * bar_width / FRAMES).min(bar_width);
    lines.push(Line::from(vec![
        Span::styled("▓".repeat(filled), Style::default().fg(theme::ACCENT)),
        Span::styled(
            "░".repeat(bar_width.saturating_sub(filled)),
            Style::default().fg(theme::DIM),
        ),
    ]));
    lines.push(Line::from(""));

    // Phase 4: READY prompt with a blinking cursor.
    if frame > 42 {
        let cursor = if (frame / 4).is_multiple_of(2) { "█" } else { " " };
        lines.push(Line::from(vec![
            Span::styled("READY", Style::default().fg(theme::HILITE).bold()),
            Span::styled(format!(" {cursor}"), Style::default().fg(theme::HILITE)),
        ]));
    } else {
        lines.push(Line::from(""));
    }

    let width = (logo_width as u16 + 6).min(f.area().width);
    let height = (lines.len() as u16 + 2).min(f.area().height);
    let area = Rect::new(
        f.area().x + (f.area().width.saturating_sub(width)) / 2,
        f.area().y + (f.area().height.saturating_sub(height)) / 2,
        width,
        height,
    );
    f.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(theme::panel_double(&format!(" v{} ", crate::VERSION))),
        area,
    );

    // Bottom line: skip hint while the animation runs, start prompt once it
    // has finished.
    if f.area().height > height + 2 {
        let hint = Rect::new(f.area().x, f.area().bottom() - 1, f.area().width, 1);
        let line = if waiting {
            // Slow arcade blink — visible three ticks out of four.
            if (frame / 4) % 4 == 3 {
                Line::from("")
            } else {
                Line::from(Span::styled(
                    "PRESS ENTER TO START",
                    Style::default().fg(theme::ACCENT).bold(),
                ))
            }
        } else {
            Line::from(Span::styled(
                "press any key to skip",
                Style::default().fg(theme::DIM),
            ))
        };
        f.render_widget(Paragraph::new(line).alignment(Alignment::Center), hint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_rows_are_rectangular() {
        let rows = banner("TRANSFER");
        assert_eq!(rows.len(), 5);
        let width = rows[0].chars().count();
        // 8 letters à 5 columns + 7 separators.
        assert_eq!(width, 8 * 5 + 7);
        assert!(rows.iter().all(|r| r.chars().count() == width));
    }

    #[test]
    fn every_logo_letter_has_a_glyph() {
        for c in "TRANSFERBUDDY".chars() {
            assert_ne!(glyph(c), glyph(' '), "missing glyph for {c}");
        }
    }
}
