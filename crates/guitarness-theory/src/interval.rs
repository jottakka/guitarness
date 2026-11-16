//! Intervals relative to a chord root, with Brazilian notation.
//!
//! Colour lives in the game (it depends on Bevy); this module is pure.

/// Interval of a note relative to the chord root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Interval {
    Root,
    MinorThird,
    MajorThird,
    Fifth,
    MinorSeventh,
    Other,
}

impl Interval {
    pub const ALL: [Interval; 6] = [
        Interval::Root,
        Interval::MinorThird,
        Interval::MajorThird,
        Interval::Fifth,
        Interval::MinorSeventh,
        Interval::Other,
    ];

    /// Classify a semitone distance from the root (any integer; wraps mod 12).
    pub fn from_semitones(semis: i32) -> Self {
        match semis.rem_euclid(12) {
            0 => Interval::Root,
            3 => Interval::MinorThird,
            4 => Interval::MajorThird,
            7 => Interval::Fifth,
            10 => Interval::MinorSeventh,
            _ => Interval::Other,
        }
    }

    /// Position in [`Interval::ALL`].
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|i| *i == self).unwrap_or(5)
    }

    /// Semitones above the root, `None` for [`Interval::Other`].
    pub fn semitones(self) -> Option<u8> {
        match self {
            Interval::Root => Some(0),
            Interval::MinorThird => Some(3),
            Interval::MajorThird => Some(4),
            Interval::Fifth => Some(7),
            Interval::MinorSeventh => Some(10),
            Interval::Other => None,
        }
    }

    /// Brazilian notation: T (guitarness), 3m, 3M, 5J (justa), 7m; a dot for the rest.
    pub fn label_br(self) -> &'static str {
        match self {
            Interval::Root => "T",
            Interval::MinorThird => "3m",
            Interval::MajorThird => "3M",
            Interval::Fifth => "5J",
            Interval::MinorSeventh => "7m",
            Interval::Other => "\u{b7}",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_and_wraps() {
        assert_eq!(Interval::from_semitones(0), Interval::Root);
        assert_eq!(Interval::from_semitones(12), Interval::Root);
        assert_eq!(Interval::from_semitones(-5), Interval::Fifth);
        assert_eq!(Interval::from_semitones(10), Interval::MinorSeventh);
        assert_eq!(Interval::from_semitones(1), Interval::Other);
    }

    #[test]
    fn index_matches_all_and_labels() {
        for (k, i) in Interval::ALL.iter().enumerate() {
            assert_eq!(i.index(), k);
            assert!(!i.label_br().is_empty());
        }
        assert_eq!(Interval::Fifth.label_br(), "5J");
        assert_eq!(Interval::MajorThird.semitones(), Some(4));
        assert_eq!(Interval::Other.semitones(), None);
    }
}
