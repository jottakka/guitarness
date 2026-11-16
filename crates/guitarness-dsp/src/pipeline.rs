//! Input chain + frontend + tracker in one object: what the audio thread (or an offline
//! WAV analysis) drives. `process` takes raw microphone blocks.

use crate::biquad::InputChain;
use crate::detector::AcfMode;
use crate::frontend::{Frame, Frontend, FrontendConfig, MAX_TARGETS};
use crate::tracker::{Event, Tracker, TrackerParams};

pub struct Pipeline {
    chain: InputChain,
    front: Frontend,
    tracker: Tracker,
    scratch: Vec<f32>,
    tid: u32,
    a4: f64,
}

impl Pipeline {
    /// `max_block`: largest block the caller will pass (pre-allocates scratch).
    pub fn demo(sr: f64, mode: AcfMode, max_block: usize) -> Self {
        let cfg = FrontendConfig::demo(sr);
        let hop_s = cfg.hop as f64 * cfg.d as f64 / sr;
        Self {
            chain: InputChain::demo(sr),
            front: Frontend::new(cfg, mode),
            tracker: Tracker::new(TrackerParams::for_hop(hop_s)),
            scratch: vec![0.0; max_block],
            tid: 0,
            a4: 440.0,
        }
    }

    pub fn config(&self) -> FrontendConfig {
        self.front.config()
    }

    /// Tuning reference (A4 in Hz) used to turn target MIDI notes into frequencies.
    pub fn set_a4(&mut self, a4: f64) {
        self.a4 = a4;
    }

    pub fn tracker(&self) -> &Tracker {
        &self.tracker
    }

    /// Announce the expected notes. `shown_t` is when the game showed them (input clock).
    pub fn set_targets(&mut self, midis: &[i32], shown_t: f64) {
        self.tid = self.tid.wrapping_add(1);
        let mut hz = [0.0f64; MAX_TARGETS];
        let n = midis.len().min(MAX_TARGETS);
        for (h, &m) in hz.iter_mut().zip(&midis[..n]) {
            *h = crate::mtof(m as f64, self.a4);
        }
        self.front.set_targets(&hz[..n], self.tid);
        self.tracker.set_targets(&midis[..n], self.tid, shown_t);
    }

    /// Feed raw mic samples (`block.len() <= max_block`). `t0` = time of the first sample.
    pub fn process(
        &mut self,
        block: &[f32],
        t0: f64,
        mut on_frame: impl FnMut(&Frame),
        mut on_event: impl FnMut(Event),
    ) {
        let n = block.len();
        let buf = &mut self.scratch[..n];
        buf.copy_from_slice(block);
        self.chain.process(buf);
        let tracker = &mut self.tracker;
        self.front.process(buf, t0, |fr| {
            on_frame(fr);
            tracker.push(fr, &mut on_event);
        });
    }
}
