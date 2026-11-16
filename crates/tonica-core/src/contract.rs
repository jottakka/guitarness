//! Requested versus judgeable contract of one chart event (plan 2.5).
//!
//! What an event asks of the player comes from the chart. What the microphone can
//! verify is derived here, on the spot, from the gate table and the calibration. It is
//! never stored in the chart, so `chart.json` stays at `v = 1`.

use crate::chart::{Chart, EventKind, Loc, Style};
use crate::gates::{GateState, GateTable};
use crate::verdict::{VerdictType, Why};

/// One thing an event may ask of the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Aspect {
    Attack,
    Timing,
    Direction,
    Accent,
    Muted,
    Chord,
    Strings,
    Fret,
}

impl Aspect {
    pub const ALL: [Aspect; 8] = [
        Aspect::Attack,
        Aspect::Timing,
        Aspect::Direction,
        Aspect::Accent,
        Aspect::Muted,
        Aspect::Chord,
        Aspect::Strings,
        Aspect::Fret,
    ];

    /// Index into `EventContract::check`.
    pub fn index(self) -> usize {
        self as usize
    }
}

/// How a requested aspect is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// The microphone verifies it; the state is the weakest gate behind it.
    Judged(GateState),
    /// Shown as a guide only, never with a result mark.
    Guide(Why),
}

/// `check[a.index()]` is `Some` exactly when the event asks for aspect `a`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct EventContract {
    pub check: [Option<Check>; 8],
}

impl EventContract {
    pub fn get(&self, a: Aspect) -> Option<Check> {
        self.check[a.index()]
    }

    pub fn requested(&self, a: Aspect) -> bool {
        self.get(a).is_some()
    }

    /// Requested and verified by the microphone.
    pub fn judged(&self, a: Aspect) -> bool {
        matches!(self.get(a), Some(Check::Judged(_)))
    }
}

/// Weakest of two gate states (`Locked` < `Provisional` < `Open`).
fn weakest(a: GateState, b: GateState) -> GateState {
    let rank = |s: GateState| match s {
        GateState::Locked => 0,
        GateState::Provisional => 1,
        GateState::Open => 2,
    };
    if rank(a) <= rank(b) { a } else { b }
}

/// What the event at `loc` in the first tier of `chart` asks for, and which of it the
/// microphone verifies. `timing_judged` is true when the clock is calibrated (see
/// `TimingRef::judges_timing`). Without it no attack can be placed, so `Attack` and
/// `Timing` are both guide (`NoCalibration`), as the judge answers. A `Loc` that points to
/// nothing, and a rest, ask for nothing.
pub fn contract(chart: &Chart, loc: Loc, gates: &GateTable, timing_judged: bool) -> EventContract {
    let mut out = EventContract::default();
    let Some(ev) = chart
        .tiers
        .get(loc.tier)
        .and_then(|t| t.events.get(loc.index))
    else {
        return out;
    };
    let mut ask = |a: Aspect, c: Check| out.check[a.index()] = Some(c);
    let gate = |ts: &[VerdictType]| -> Check {
        let mut st = GateState::Open;
        for &t in ts {
            if !gates.judges(t) {
                return Check::Guide(Why::GateLocked);
            }
            st = weakest(st, gates.state(t));
        }
        Check::Judged(st)
    };
    // The pulse judge never judges direction (player rule; its verdict is always
    // `NotJudged(GateLocked)`), whatever the gate says, so the contract must not promise it.
    let direction = || Check::Guide(Why::GateLocked);
    let attack = |ask: &mut dyn FnMut(Aspect, Check)| {
        if timing_judged {
            ask(Aspect::Attack, gate(&[VerdictType::Attack]));
        } else {
            ask(Aspect::Attack, Check::Guide(Why::NoCalibration));
        }
    };
    let timing = |ask: &mut dyn FnMut(Aspect, Check)| {
        let c = match gate(&[VerdictType::Timing]) {
            Check::Judged(_) if !timing_judged => Check::Guide(Why::NoCalibration),
            other => other,
        };
        ask(Aspect::Timing, c);
    };
    match &ev.kind {
        EventKind::Strum { style, .. } => {
            attack(&mut ask);
            timing(&mut ask);
            ask(Aspect::Direction, direction());
            ask(Aspect::Chord, gate(&[VerdictType::ChordIdentity]));
            ask(
                Aspect::Strings,
                gate(&[VerdictType::StringMissing, VerdictType::StringExtra]),
            );
            ask(Aspect::Fret, gate(&[VerdictType::WrongFret]));
            match style {
                Style::Accent => ask(Aspect::Accent, gate(&[VerdictType::Accent])),
                Style::Pm => ask(Aspect::Muted, gate(&[VerdictType::Muted])),
                Style::Full | Style::Roll => {}
            }
        }
        EventKind::Mute { .. } => {
            attack(&mut ask);
            timing(&mut ask);
            ask(Aspect::Direction, direction());
            ask(Aspect::Muted, gate(&[VerdictType::Muted]));
        }
        EventKind::Note { .. } => {
            attack(&mut ask);
            timing(&mut ask);
            ask(
                Aspect::Strings,
                gate(&[VerdictType::StringMissing, VerdictType::StringExtra]),
            );
            ask(Aspect::Fret, gate(&[VerdictType::WrongFret]));
        }
        EventKind::Ghost { .. } => ask(Aspect::Direction, Check::Guide(Why::Ghost)),
        EventKind::Rest => {}
    }
    out
}
