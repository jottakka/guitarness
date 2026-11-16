//! Karplus-Strong plucked string with all-pass fine tuning (port of `ksPluck`).
//! Used by tests, the simulator and the virtual guitar. Allocates: not for the audio path.

/// `rnd` returns uniform values in [0, 1).
pub fn ks_pluck(sr: f64, freq: f64, dur: f64, mut rnd: impl FnMut() -> f64) -> Vec<f32> {
    let p = sr / freq;
    let n = (p - 0.6).floor() as usize;
    let d = p - 0.5 - n as f64;
    let a = (1.0 - d) / (1.0 + d);
    let len = (sr * dur).floor() as usize;
    let mut out = vec![0.0f32; len];
    let mut buf = vec![0.0f32; n];
    let mut lp = 0.0f64;
    let mut m = 0.0f64;
    for b in buf.iter_mut() {
        lp += 0.55 * ((rnd() * 2.0 - 1.0) - lp);
        *b = lp as f32;
        m += *b as f64;
    }
    m /= n as f64;
    for b in buf.iter_mut() {
        *b = (*b as f64 - m) as f32;
    }
    let g = 10f64.powf(-3.0 / (freq * if freq < 200.0 { 3.2 } else { 2.4 }));
    let (mut pos, mut last, mut pin, mut pout, mut pk) = (0usize, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for o in out.iter_mut() {
        let cur = buf[pos] as f64;
        let avg = 0.5 * (cur + last);
        last = cur;
        let ap = a * avg + pin - a * pout;
        pin = avg;
        pout = ap;
        buf[pos] = (ap * g) as f32;
        *o = cur as f32;
        pk = pk.max(cur.abs());
        pos += 1;
        if pos == n {
            pos = 0;
        }
    }
    if pk > 0.0 {
        for o in out.iter_mut() {
            *o = (*o as f64 / pk) as f32;
        }
    }
    out
}

/// The demo's mulberry32-style PRNG (`rng` in test/golden.js), for reproducible signals.
pub struct Mulberry32(u32);

impl Mulberry32 {
    pub fn new(seed: u32) -> Self {
        Self(seed)
    }
    pub fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6D2B_79F5);
        let a = self.0;
        let mut t = (a ^ (a >> 15)).wrapping_mul(1 | a);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        ((t ^ (t >> 14)) as f64) / 4_294_967_296.0
    }
}
