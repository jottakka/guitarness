//! The chart the views draw: a thin resource around the core's [`Timeline`].
//!
//! The data model (events, chords, glyphs, spans) lives in `guitarness_core::timeline`; the
//! game only adds the Bevy `Resource` impl and the `Chart::judged`-style shortcuts.

use std::ops::Deref;

use bevy::prelude::Resource;

pub use guitarness_core::timeline::{Chord, Dir, Event, Glyph, Kind, OPEN, Style, Timeline};

#[derive(Resource, Clone, Debug)]
pub struct Chart(pub Timeline);

impl Deref for Chart {
    type Target = Timeline;
    fn deref(&self) -> &Timeline {
        &self.0
    }
}

impl Chart {
    /// Hardcoded demo chart.
    pub fn demo() -> Chart {
        Chart(Timeline::demo())
    }

    /// Stroke-style gallery chart.
    pub fn gallery() -> Chart {
        Chart(Timeline::gallery())
    }

    pub fn is_ghost(ev: &Event) -> bool {
        Timeline::is_ghost(ev)
    }

    pub fn is_mute(ev: &Event) -> bool {
        Timeline::is_mute(ev)
    }

    /// Ghost strokes are never judged.
    pub fn judged(ev: &Event) -> bool {
        Timeline::judged(ev)
    }
}
