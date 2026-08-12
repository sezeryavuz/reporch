//! Colours.
//!
//! Deliberately restrained: reporch runs in the user's terminal, on their colour scheme. Only
//! indexed ANSI colours are used, so the palette the user chose is respected instead of being
//! overridden with hard-coded RGB that fights a light background.

use ratatui::style::{Color, Modifier, Style};

/// The one accent colour, used for the product name and the selected row.
pub const ACCENT: Color = Color::Cyan;
/// Secondary text: hints, timestamps, the subtitle.
pub const DIM: Color = Color::DarkGray;

// State colours for the §5.8 run screen and the doctor view; defined with the rest of the
// palette so the whole scheme is decided in one place rather than per screen.
#[allow(dead_code, reason = "used by the §5.8 run screen, landing in M5")]
pub const OK: Color = Color::Green;
#[allow(dead_code, reason = "used by the §5.8 run screen, landing in M5")]
pub const WARN: Color = Color::Yellow;
#[allow(dead_code, reason = "used by the §5.8 run screen, landing in M5")]
pub const FAIL: Color = Color::Red;

/// The selected row in a list.
pub fn selected() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

/// Colour for a doctor severity.
#[allow(dead_code, reason = "used by the in-TUI doctor view, landing in M2")]
pub fn severity(s: reporch_claude::Severity) -> Color {
    match s {
        reporch_claude::Severity::Ok => OK,
        reporch_claude::Severity::Warn => WARN,
        reporch_claude::Severity::Fail => FAIL,
    }
}
