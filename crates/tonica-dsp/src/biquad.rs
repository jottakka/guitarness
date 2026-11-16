//! RBJ biquads, matching the Web Audio `BiquadFilterNode` formulas used by the demo.
//! Coefficients and state are f64; each output sample is rounded to f32 (as the demo's
//! Float32Array buffers do between filters).

#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    HighPass,
    LowPass,
}

impl Biquad {
    pub fn new(kind: Kind, f0: f64, q: f64, sr: f64) -> Self {
        let w0 = 2.0 * std::f64::consts::PI * f0 / sr;
        let c = w0.cos();
        let al = w0.sin() / (2.0 * q);
        let (b0, b1, b2) = match kind {
            Kind::HighPass => ((1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0),
            Kind::LowPass => ((1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0),
        };
        let a0 = 1.0 + al;
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: -2.0 * c / a0,
            a2: (1.0 - al) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        let xi = x as f64;
        let y = self.b0 * xi + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = xi;
        self.y2 = self.y1;
        self.y1 = y;
        y as f32
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

/// The demo's input chain: high-pass 62 Hz, then two low-passes at 2.6 kHz (Q 0.707).
#[derive(Clone, Copy, Debug)]
pub struct InputChain {
    hp: Biquad,
    lp1: Biquad,
    lp2: Biquad,
}

impl InputChain {
    pub fn demo(sr: f64) -> Self {
        Self {
            hp: Biquad::new(Kind::HighPass, 62.0, 0.707, sr),
            lp1: Biquad::new(Kind::LowPass, 2600.0, 0.707, sr),
            lp2: Biquad::new(Kind::LowPass, 2600.0, 0.707, sr),
        }
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        self.lp2.tick(self.lp1.tick(self.hp.tick(x)))
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for v in buf.iter_mut() {
            *v = self.tick(*v);
        }
    }
}
