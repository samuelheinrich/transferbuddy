//! Shared ANSI palette; the terminal grid/parser is owned by the SSH session.
pub fn rgb(color: vt100::Color) -> Option<[u8; 3]> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Rgb(r, g, b) => Some([r, g, b]),
        vt100::Color::Idx(n) => Some(match n {
            0..=15 => [
                [0, 0, 0],
                [205, 49, 49],
                [13, 188, 121],
                [229, 229, 16],
                [36, 114, 200],
                [188, 63, 188],
                [17, 168, 205],
                [229, 229, 229],
                [102, 102, 102],
                [241, 76, 76],
                [35, 209, 139],
                [245, 245, 67],
                [59, 142, 234],
                [214, 112, 214],
                [41, 184, 219],
                [255, 255, 255],
            ][n as usize],
            16..=231 => {
                let n = n - 16;
                let scale = [0, 95, 135, 175, 215, 255];
                [
                    scale[(n / 36) as usize],
                    scale[((n / 6) % 6) as usize],
                    scale[(n % 6) as usize],
                ]
            }
            _ => {
                let v = 8 + (n - 232) * 10;
                [v, v, v]
            }
        }),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn colors_and_chunked_ansi() {
        assert_eq!(rgb(vt100::Color::Idx(196)), Some([255, 0, 0]));
        assert_eq!(rgb(vt100::Color::Idx(255)), Some([238, 238, 238]));
        let mut parser = vt100::Parser::new(4, 20, 100);
        for chunk in [b"hello\r".as_slice(), b"\x1b[", b"31mOK", b"\x1b[0m\x1b[K"] {
            parser.process(chunk)
        }
        assert_eq!(parser.screen().contents(), "OK");
        assert_eq!(
            parser.screen().cell(0, 0).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );
    }
}
