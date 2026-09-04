// SPDX-License-Identifier: Apache-2.0
//! The analogue clock face.
//!
//! Ported from the Win32 `draw_title_analog_clock`, which drew straight to the
//! device context with `Ellipse`/`MoveToEx`/`LineTo`. Here it is an `iced`
//! canvas program, so the same geometry survives but resizing, DPI and
//! double-buffering are the toolkit's problem rather than ours.
//!
//! The second hand advances continuously rather than ticking: the underlying
//! value is nanoseconds, and quantising it to whole seconds would throw away
//! the precision the whole program exists to display.

use std::f32::consts::PI;

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Style};
use iced::{Point, Rectangle, Renderer, Theme, Vector};

use crate::style;

/// Draws a clock face for one instant.
#[derive(Debug, Clone, Copy)]
pub struct AnalogClock {
    /// Wall-clock time to display, already offset into the target zone.
    pub local_ns: u64,
}

impl AnalogClock {
    pub fn new(local_ns: u64) -> Self {
        AnalogClock { local_ns }
    }

    /// Hand angles in radians, measured clockwise from twelve o'clock.
    fn angles(&self) -> (f32, f32, f32) {
        let ns_of_day = self.local_ns % 86_400_000_000_000;
        let seconds = (ns_of_day % 60_000_000_000) as f64 / 1e9;
        let minutes = ((ns_of_day / 60_000_000_000) % 60) as f64 + seconds / 60.0;
        let hours = ((ns_of_day / 3_600_000_000_000) % 12) as f64 + minutes / 60.0;

        let turn = |fraction: f64| (fraction * 2.0 * PI as f64 - PI as f64 / 2.0) as f32;
        (
            turn(hours / 12.0),
            turn(minutes / 60.0),
            turn(seconds / 60.0),
        )
    }
}

impl<Message> canvas::Program<Message> for AnalogClock {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let centre = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let radius = (bounds.width.min(bounds.height) / 2.0 - 6.0).max(8.0);

        frame.stroke(
            &Path::circle(centre, radius),
            Stroke {
                style: Style::Solid(style::BORDER),
                width: 1.5,
                ..Stroke::default()
            },
        );

        // Hour ticks, with the quarters emphasised.
        for tick in 0..12 {
            let angle = tick as f32 / 12.0 * 2.0 * PI - PI / 2.0;
            let major = tick % 3 == 0;
            let inner = radius * if major { 0.82 } else { 0.90 };
            let direction = Vector::new(angle.cos(), angle.sin());
            frame.stroke(
                &Path::line(
                    centre + direction * inner,
                    centre + direction * (radius * 0.96),
                ),
                Stroke {
                    style: Style::Solid(if major { style::INFO } else { style::BORDER }),
                    width: if major { 2.0 } else { 1.0 },
                    ..Stroke::default()
                },
            );
        }

        let (hour, minute, second) = self.angles();
        let hand = |frame: &mut Frame, angle: f32, length: f32, width: f32, colour| {
            let direction = Vector::new(angle.cos(), angle.sin());
            frame.stroke(
                &Path::line(centre, centre + direction * (radius * length)),
                Stroke {
                    style: Style::Solid(colour),
                    width,
                    ..Stroke::default()
                },
            );
        };

        hand(&mut frame, hour, 0.50, 3.0, style::VALUE);
        hand(&mut frame, minute, 0.72, 2.0, style::VALUE);
        hand(&mut frame, second, 0.86, 1.0, style::TIMER);

        frame.fill(&Path::circle(centre, 3.0), style::TIMER);

        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// At exactly midnight every hand points at twelve, i.e. -pi/2.
    #[test]
    fn midnight_points_all_hands_up() {
        let (h, m, s) = AnalogClock::new(0).angles();
        let up = -PI / 2.0;
        assert!((h - up).abs() < 1e-5);
        assert!((m - up).abs() < 1e-5);
        assert!((s - up).abs() < 1e-5);
    }

    /// At 03:00 the hour hand is horizontal, pointing right.
    #[test]
    fn three_oclock_points_the_hour_hand_right() {
        let (h, _, _) = AnalogClock::new(3 * 3_600_000_000_000).angles();
        assert!(h.abs() < 1e-5, "hour angle was {h}");
    }

    /// The second hand moves within a single second: it sweeps, not ticks.
    #[test]
    fn the_second_hand_sweeps_continuously() {
        let (_, _, a) = AnalogClock::new(0).angles();
        let (_, _, b) = AnalogClock::new(500_000_000).angles();
        assert!(
            (a - b).abs() > 1e-3,
            "second hand did not move within a second"
        );
    }

    /// Days roll over cleanly rather than accumulating.
    #[test]
    fn the_face_wraps_at_a_full_day() {
        let day = 86_400_000_000_000u64;
        let a = AnalogClock::new(day + 3_600_000_000_000).angles();
        let b = AnalogClock::new(3_600_000_000_000).angles();
        assert!((a.0 - b.0).abs() < 1e-5);
    }
}
