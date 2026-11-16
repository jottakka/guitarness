//! Theme colours (DESIGN 5). The interval type itself lives in `tonica-theory`.

use bevy::prelude::*;

pub use tonica_theory::interval::Interval;

/// Colour of an interval glyph.
pub fn interval_color(i: Interval) -> Color {
    match i {
        Interval::Root => Color::srgb_u8(0xff, 0x59, 0x64),
        Interval::MinorThird => Color::srgb_u8(0xff, 0xd2, 0x3f),
        Interval::MajorThird => Color::srgb_u8(0xc8, 0xe6, 0x4b),
        Interval::Fifth => Color::srgb_u8(0x38, 0xbd, 0xf8),
        Interval::MinorSeventh => Color::srgb_u8(0xe8, 0x79, 0xf9),
        Interval::Other => Color::srgb_u8(0x8a, 0x93, 0xa6),
    }
}

/// Near-black navy background.
pub const BG: Color = Color::srgb(0.020, 0.027, 0.055);
pub const LANE: Color = Color::srgb(0.30, 0.38, 0.55);
pub const HIT: Color = Color::srgb(0.80, 0.92, 1.0);
pub const BAR: Color = Color::srgb(0.45, 0.55, 0.78);
