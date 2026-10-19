//! Colours and symbols, after the mockup in `docs/design/tui`: a dark
//! ground, a quiet surface under fields, one accent, and a colour each for
//! errors, success and warnings. On the machine's own console the 8 colour
//! slots are reprogrammed to that palette; a terminal with true colour gets
//! it in RGB, others in 256-colour indexes. Plain mode drops colour and
//! symbols: ASCII only, focus in reverse video.

use ratatui::style::{Color, Modifier, Style};

/// How the palette reaches the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colors {
    /// The console's 8 slots hold the palette (see [`console_palette`]).
    Console,
    TrueColor,
    Indexed,
}

/// A colour of the palette, by role. The order is the console slot each
/// takes: 0 is black, 1 red, … 7 white.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Ground,
    Error,
    Success,
    Warning,
    Surface,
    Secondary,
    Accent,
    Text,
}

/// RGB of each tone, in slot order.
const PALETTE: [(u8, u8, u8); 8] = [
    (0x10, 0x14, 0x18),
    (0xfd, 0xa4, 0xaf),
    (0xa7, 0xd7, 0xae),
    (0xf2, 0xcc, 0x8f),
    (0x1b, 0x22, 0x2a),
    (0xa1, 0xac, 0xb8),
    (0x7d, 0xd3, 0xfc),
    (0xed, 0xf2, 0xf7),
];

/// The nearest 256-colour indexes, in slot order.
const INDEXED: [u8; 8] = [233, 217, 151, 222, 235, 247, 117, 255];

/// The palette as `setvtrgb` reads it: the red, green and blue of the 16
/// console colours, one line each; bright colours repeat the normal ones.
pub fn console_palette() -> String {
    let line = |channel: fn(&(u8, u8, u8)) -> u8| {
        let values: Vec<String> = PALETTE.iter().map(|c| channel(c).to_string()).collect();
        let values = values.join(",");
        format!("{values},{values}\n")
    };
    line(|c| c.0) + &line(|c| c.1) + &line(|c| c.2)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub colors: Colors,
    /// The terminal draws ✓ ● ━ ▌ … (a UTF-8 terminal, or the console with
    /// the eurlatgr font loaded).
    pub rich: bool,
    /// Plain mode: ASCII and no colour, for terminals that render neither.
    pub plain: bool,
}

impl Theme {
    pub fn new(colors: Colors, rich: bool) -> Self {
        Self {
            colors,
            rich,
            plain: false,
        }
    }

    /// Plain mode from the start; Ctrl+T still brings `colors` back.
    pub fn plain(colors: Colors) -> Self {
        Self {
            colors,
            rich: false,
            plain: true,
        }
    }

    pub fn toggle_plain(&mut self) {
        self.plain = !self.plain;
    }

    fn color(self, tone: Tone) -> Color {
        let slot = tone as usize;
        match self.colors {
            Colors::Console => [
                Color::Black,
                Color::Red,
                Color::Green,
                Color::Yellow,
                Color::Blue,
                Color::Magenta,
                Color::Cyan,
                // Normal white: bright white is out of a 512-glyph console's reach.
                Color::Gray,
            ][slot],
            Colors::TrueColor => {
                let (r, g, b) = PALETTE[slot];
                Color::Rgb(r, g, b)
            }
            Colors::Indexed => Color::Indexed(INDEXED[slot]),
        }
    }

    /// The screen: text on the ground.
    pub fn ground(self) -> Style {
        self.styled(
            Style::new()
                .fg(self.color(Tone::Text))
                .bg(self.color(Tone::Ground)),
        )
    }

    /// Text in `tone`, on the ground.
    pub fn fg(self, tone: Tone) -> Style {
        self.styled(Style::new().fg(self.color(tone)))
    }

    /// Text in `tone`, on the surface fields and keys sit on.
    pub fn on_surface(self, tone: Tone) -> Style {
        self.styled(
            Style::new()
                .fg(self.color(tone))
                .bg(self.color(Tone::Surface)),
        )
    }

    /// Ground-coloured text on `tone`: the focused control, a chosen line.
    /// Reverse video in plain mode.
    pub fn filled(self, tone: Tone) -> Style {
        if self.plain {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
                .fg(self.color(Tone::Ground))
                .bg(self.color(tone))
        }
    }

    pub fn focus(self) -> Style {
        self.filled(Tone::Accent)
    }

    fn styled(self, style: Style) -> Style {
        if self.plain { Style::new() } else { style }
    }

    fn glyph(self, rich: &'static str, ascii: &'static str) -> &'static str {
        if self.rich && !self.plain {
            rich
        } else {
            ascii
        }
    }

    pub fn check(self) -> &'static str {
        self.glyph("✓", "*")
    }

    pub fn cross(self) -> &'static str {
        self.glyph("✗", "x")
    }

    /// Marks the focused control's label.
    pub fn pointer(self) -> &'static str {
        self.glyph("›", ">")
    }

    /// The focused field's left edge.
    pub fn edge(self) -> &'static str {
        self.glyph("▌", "[")
    }

    pub fn chosen(self) -> &'static str {
        self.glyph("(●)", "(*)")
    }

    /// One of several choices, taken.
    pub fn ticked(self) -> &'static str {
        self.glyph("[✓]", "[x]")
    }

    pub fn arrow(self) -> &'static str {
        self.glyph("→", "->")
    }

    pub fn up(self) -> &'static str {
        self.glyph("↑", "^")
    }

    pub fn down(self) -> &'static str {
        self.glyph("↓", "v")
    }

    pub fn ellipsis(self) -> &'static str {
        self.glyph("…", "...")
    }

    pub fn dot(self) -> &'static str {
        self.glyph("·", "-")
    }

    pub fn secret(self) -> char {
        if self.rich && !self.plain { '•' } else { '*' }
    }

    /// The step bar: done, then still to come.
    pub fn bar(self) -> (&'static str, &'static str) {
        (self.glyph("━", "="), self.glyph("━", "-"))
    }

    /// A block of colour, for the partitions' legend.
    pub fn block(self) -> &'static str {
        self.glyph("■", "#")
    }

    /// Up and down, then left and right, as the keys' legend writes them.
    pub fn arrows(self) -> (&'static str, &'static str) {
        (self.glyph("↑↓", "up/down"), self.glyph("←→", "left/right"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_console_palette_is_what_setvtrgb_reads() {
        let palette = console_palette();
        let lines: Vec<&str> = palette.lines().collect();
        assert_eq!(lines.len(), 3);
        for line in lines {
            assert_eq!(line.split(',').count(), 16);
        }
        assert!(palette.starts_with("16,253,167,242,27,161,125,237,16,"));
    }

    #[test]
    fn plain_mode_has_no_colour_and_ascii_only() {
        let mut theme = Theme::new(Colors::TrueColor, true);
        theme.toggle_plain();
        assert_eq!(theme.fg(Tone::Error), Style::new());
        assert_eq!(theme.focus(), Style::new().add_modifier(Modifier::REVERSED));
        assert!(theme.check().is_ascii() && theme.edge().is_ascii());
    }
}
