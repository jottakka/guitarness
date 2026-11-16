//! Port of the demo's AudioWorklet (`TonicaProc` in legacy/web-demo/src/worklet.js):
//! decimation by block averaging, a ring window, one analysis frame every `hop`
//! decimated samples, and per-target presence. No allocation after construction.

use crate::detector::{AcfMode, Detector};

/// Maximum simultaneous targets per frame.
pub const MAX_TARGETS: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    /// Time in seconds (input clock) of the sample that completed the hop.
    pub t: f64,
    pub f: f64,
    pub c: f64,
    pub rms: f64,
    pub rms_s: f64,
    /// Target set id the presences refer to.
    pub tid: u32,
    pub n_p: usize,
    pub p: [f64; MAX_TARGETS],
}

impl Frame {
    pub fn presences(&self) -> &[f64] {
        &self.p[..self.n_p]
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FrontendConfig {
    /// Input sample rate.
    pub sr: f64,
    /// Decimation factor.
    pub d: usize,
    /// Window length (decimated samples).
    pub w: usize,
    /// Hop (decimated samples).
    pub hop: usize,
}

impl FrontendConfig {
    /// The demo's parameters: ~24 kHz analysis, ~42.7 ms window, ~10.7 ms hop.
    pub fn demo(sr: f64) -> Self {
        let d = ((sr / 24000.0).round() as usize).max(1);
        let fsd = sr / d as f64;
        let w = 2 * js_round(fsd * 0.0427 / 2.0) as usize;
        let hop = js_round(fsd * 0.0107) as usize;
        Self { sr, d, w, hop }
    }
}

/// `Math.round` semantics (round half up).
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

pub struct Frontend {
    cfg: FrontendConfig,
    det: Detector,
    ring: Vec<f32>,
    lin: Vec<f32>,
    wp: usize,
    acc: f64,
    acc_n: usize,
    since: usize,
    targets: [f64; MAX_TARGETS],
    n_targets: usize,
    tid: u32,
}

impl Frontend {
    pub fn new(cfg: FrontendConfig, mode: AcfMode) -> Self {
        let fsd = cfg.sr / cfg.d as f64;
        Self {
            det: Detector::new(fsd, cfg.w, mode),
            ring: vec![0.0; cfg.w],
            lin: vec![0.0; cfg.w],
            wp: 0,
            acc: 0.0,
            acc_n: 0,
            since: 0,
            targets: [0.0; MAX_TARGETS],
            n_targets: 0,
            tid: 0,
            cfg,
        }
    }

    pub fn config(&self) -> FrontendConfig {
        self.cfg
    }

    /// Set the expected notes (Hz). Extra targets beyond [`MAX_TARGETS`] are ignored.
    pub fn set_targets(&mut self, hz: &[f64], tid: u32) {
        let n = hz.len().min(MAX_TARGETS);
        self.targets[..n].copy_from_slice(&hz[..n]);
        self.n_targets = n;
        self.tid = tid;
    }

    /// Feed one block. `t0` is the time of the block's first sample. Calls `on_frame` for
    /// every completed analysis frame.
    pub fn process(&mut self, block: &[f32], t0: f64, mut on_frame: impl FnMut(&Frame)) {
        let d = self.cfg.d;
        let w = self.cfg.w;
        for (i, &s) in block.iter().enumerate() {
            self.acc += s as f64;
            self.acc_n += 1;
            if self.acc_n == d {
                self.ring[self.wp] = (self.acc / d as f64) as f32;
                self.acc = 0.0;
                self.acc_n = 0;
                self.wp += 1;
                if self.wp == w {
                    self.wp = 0;
                }
                self.since += 1;
                if self.since >= self.cfg.hop {
                    self.since = 0;
                    let fr = self.analyze(t0 + i as f64 / self.cfg.sr);
                    on_frame(&fr);
                }
            }
        }
    }

    fn analyze(&mut self, t: f64) -> Frame {
        let w = self.cfg.w;
        let wp = self.wp;
        self.lin[..w - wp].copy_from_slice(&self.ring[wp..]);
        self.lin[w - wp..].copy_from_slice(&self.ring[..wp]);
        let r = self.det.analyze(&self.lin, self.cfg.hop);
        let mut fr = Frame {
            t,
            f: r.f,
            c: r.c,
            rms: r.rms,
            rms_s: r.rms_s,
            tid: self.tid,
            n_p: self.n_targets,
            p: [0.0; MAX_TARGETS],
        };
        for k in 0..self.n_targets {
            fr.p[k] = self.det.presence(self.targets[k]);
        }
        fr
    }
}
