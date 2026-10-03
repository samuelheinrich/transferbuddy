//! Export real ratatui buffers for the documentation, with no network access.
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use std::fmt::Write;
use std::path::Path;

fn color(value: Color, fallback: &str) -> String {
    match value {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Black => "#000000".into(),
        Color::Red => "#ff5555".into(),
        Color::Green => "#33ff66".into(),
        Color::Yellow => "#ffd24d".into(),
        Color::Blue => "#3f7fd0".into(),
        Color::Magenta => "#ff5ce0".into(),
        Color::Cyan => "#33d6ff".into(),
        Color::White => "#ffffff".into(),
        _ => fallback.into(),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(super) fn save(buffer: &Buffer, path: &Path) {
    let width = u32::from(buffer.area.width) * 9 + 32;
    let height = u32::from(buffer.area.height) * 18 + 32;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\n\
         <title>TransferBuddy {} terminal screenshot</title>\n\
         <desc>Generated from the real ratatui renderer using demonstration devices.</desc>\n\
         <rect width=\"100%\" height=\"100%\" fill=\"#0b0f1a\"/>\n\
         <g font-family=\"Menlo,DejaVu Sans Mono,monospace\" font-size=\"14\" xml:space=\"preserve\">\n",
        crate::VERSION
    );
    for y in 0..buffer.area.height {
        let mut x = 0;
        while x < buffer.area.width {
            let cell = &buffer[(x, y)];
            let start = x;
            let mut text = String::new();
            while x < buffer.area.width {
                let next = &buffer[(x, y)];
                if next.fg != cell.fg || next.bg != cell.bg || next.modifier != cell.modifier {
                    break;
                }
                text.push_str(next.symbol());
                x += 1;
            }
            let mut foreground = color(cell.fg, "#d6ecff");
            let mut background = color(cell.bg, "#0b0f1a");
            if cell.modifier.contains(Modifier::REVERSED) {
                std::mem::swap(&mut foreground, &mut background);
            }
            let px = u32::from(start) * 9 + 16;
            let py = u32::from(y) * 18 + 16;
            let length = u32::from(x - start) * 9;
            if background != "#0b0f1a" {
                writeln!(svg, "<rect x=\"{px}\" y=\"{py}\" width=\"{length}\" height=\"18\" fill=\"{background}\"/>").unwrap();
            }
            let weight = if cell.modifier.contains(Modifier::BOLD) {
                "bold"
            } else {
                "normal"
            };
            // Explicit cell positions also work in SVG viewers that collapse
            // whitespace inside text nodes (including macOS Quick Look).
            for (offset, symbol) in text.chars().enumerate() {
                if symbol.is_whitespace() {
                    continue;
                }
                let text_x = px + offset as u32 * 9;
                writeln!(svg, "<text x=\"{text_x}\" y=\"{}\" fill=\"{foreground}\" font-weight=\"{weight}\">{}</text>", py + 14, escape(&symbol.to_string())).unwrap();
            }
        }
    }
    svg.push_str("</g>\n</svg>\n");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, svg).unwrap();
}
