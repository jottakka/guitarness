//! Per-frame cost of the detector (target: ≤ 30 µs/frame on desktop).
use criterion::{Criterion, criterion_group, criterion_main};
use guitarness_dsp::synth::{Mulberry32, ks_pluck};
use guitarness_dsp::{AcfMode, Detector};
use std::hint::black_box;

fn window() -> Vec<f32> {
    let mut r = Mulberry32::new(7);
    let tone = ks_pluck(24000.0, 110.0, 0.2, || r.next_f64());
    tone[1000..2024].iter().map(|v| v * 0.3).collect()
}

fn bench(c: &mut Criterion) {
    let x = window();
    for (name, mode) in [("direct", AcfMode::Direct), ("fft", AcfMode::Fft)] {
        let mut det = Detector::new(24000.0, 1024, mode);
        c.bench_function(&format!("analyze_{name}"), |b| {
            b.iter(|| black_box(det.analyze(black_box(&x), 257)))
        });
        let mut det = Detector::new(24000.0, 1024, mode);
        c.bench_function(&format!("frame_{name}_analyze+3presence"), |b| {
            b.iter(|| {
                let r = det.analyze(black_box(&x), 257);
                let p = det.presence(110.0) + det.presence(146.83) + det.presence(196.0);
                black_box((r, p))
            })
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
