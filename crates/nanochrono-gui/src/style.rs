// SPDX-License-Identifier: Apache-2.0
//! The NanoChrono palette and reusable widget styles.
//!
//! Colours are carried over from the Win32 build, converted out of `COLORREF`
//! (which is `0x00BBGGRR`, not RGB — several of the original constants read
//! backwards if you assume otherwise). The terminal-green-on-black identity is
//! preserved exactly.

use iced::widget::{button, container, text_input};
use iced::{Background, Border, Color, Theme};

/// `0xRRGGBB` to an `iced::Color`.
const fn rgb(hex: u32) -> Color {
    Color::from_rgb(
        ((hex >> 16) & 0xFF) as f32 / 255.0,
        ((hex >> 8) & 0xFF) as f32 / 255.0,
        (hex & 0xFF) as f32 / 255.0,
    )
}

/// Window background.
pub const BG: Color = rgb(0x000000);
/// Navigation strip, one step above the background.
pub const NAV_BG: Color = rgb(0x050805);
/// Panel interior.
pub const PANEL_BG: Color = rgb(0x0A120A);
/// Log / readout interior, darkest of the surfaces.
pub const INSET_BG: Color = rgb(0x040A04);
/// Hairline borders.
pub const BORDER: Color = rgb(0x1A1A1A);
/// Selected row or active tab.
pub const SELECTED_BG: Color = rgb(0x131E13);
/// Hovered row.
pub const HOVER_BG: Color = rgb(0x0C140C);

/// The signature neon green: running timer, headings, accents.
pub const TIMER: Color = rgb(0x00FF4E);
/// Dark green glow laid under the timer face.
pub const TIMER_GLOW: Color = rgb(0x00521B);
/// Amber: paused.
pub const PAUSED: Color = rgb(0xFFAA00);
/// Blue: stopped.
pub const STOPPED: Color = rgb(0x0044AA);
/// Body text.
pub const INFO: Color = rgb(0xB7B7B7);
/// Dimmed text: units, hints, secondary labels.
pub const MUTED: Color = rgb(0x6E7A6E);
/// Status-bar text.
pub const STATUS: Color = rgb(0x33DD66);
/// A capability that is present.
pub const OK: Color = rgb(0x88FF33);
/// A capability the machine does not have.
pub const OFF: Color = rgb(0x446644);
/// Section labels in the benchmark panel.
pub const LABEL: Color = rgb(0x99CC99);
/// Errors and failed probes.
pub const ALERT: Color = rgb(0xFF5555);
/// Readout values, brighter than body text.
pub const VALUE: Color = rgb(0xFFFFFF);

/// The window surface.
pub fn root(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG)),
        text_color: Some(INFO),
        ..container::Style::default()
    }
}

/// The top navigation strip.
pub fn nav(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(NAV_BG)),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..container::Style::default()
    }
}

/// A titled readout panel.
pub fn panel(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(PANEL_BG)),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 2.0.into(),
        },
        ..container::Style::default()
    }
}

/// A sunken surface: the benchmark log, the timer face.
pub fn inset(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(INSET_BG)),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 2.0.into(),
        },
        ..container::Style::default()
    }
}

/// The bottom status bar.
pub fn status_bar(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(rgb(0x111111))),
        text_color: Some(STATUS),
        ..container::Style::default()
    }
}

/// A tab or toggle: filled green when active, outlined when not.
pub fn tab(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let hovered = matches!(status, button::Status::Hovered);
        button::Style {
            background: Some(Background::Color(if active {
                TIMER
            } else if hovered {
                HOVER_BG
            } else {
                NAV_BG
            })),
            text_color: if active { BG } else { INFO },
            border: Border {
                color: if active { TIMER } else { BORDER },
                width: 1.0,
                radius: 2.0.into(),
            },
            ..button::Style::default()
        }
    }
}

/// A selectable row in the benchmark panel.
///
/// `enabled` false means the CPU lacks the family: it still renders, greyed,
/// so the panel shows the whole ISA landscape rather than silently hiding what
/// this machine cannot do.
pub fn row(selected: bool, enabled: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let hovered = matches!(status, button::Status::Hovered) && enabled;
        button::Style {
            background: Some(Background::Color(if selected {
                SELECTED_BG
            } else if hovered {
                HOVER_BG
            } else {
                Color::TRANSPARENT
            })),
            text_color: if enabled { OK } else { OFF },
            border: Border {
                color: if selected { TIMER } else { Color::TRANSPARENT },
                width: 1.0,
                radius: 2.0.into(),
            },
            ..button::Style::default()
        }
    }
}

/// A plain action button.
pub fn action(_theme: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered);
    button::Style {
        background: Some(Background::Color(if hovered { HOVER_BG } else { NAV_BG })),
        text_color: INFO,
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 2.0.into(),
        },
        ..button::Style::default()
    }
}

/// A text entry on the dark surface.
pub fn input(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused);
    text_input::Style {
        background: Background::Color(INSET_BG),
        border: Border {
            color: if focused { TIMER } else { BORDER },
            width: 1.0,
            radius: 2.0.into(),
        },
        icon: MUTED,
        placeholder: MUTED,
        value: VALUE,
        selection: SELECTED_BG,
    }
}

/// Colour for the timer face, given the stopwatch state.
pub fn timer_color(state: nanochrono_core::StopwatchState) -> Color {
    use nanochrono_core::StopwatchState::*;
    match state {
        Paused => PAUSED,
        Stopped => STOPPED,
        Reset | Running => TIMER,
    }
}
