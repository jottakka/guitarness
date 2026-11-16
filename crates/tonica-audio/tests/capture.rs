//! U4: capture stage. Split invariance against the bare detector, channel picks, ring
//! overflow, tap, non-finite input, and no allocation in `process`.

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use tonica_audio::capture::{RAW_SECONDS, RawMark, RawOutput};
use tonica_audio::{CaptureOutputs, CaptureRings, CaptureStage, ChannelPick, InEvent, Stamp};
use tonica_dsp::{OnsetConfig, OnsetDetector, OnsetEvent};
use tonica_sim::timing::{Cond, make_take};

#[global_allocator]
static A: AllocDisabler = AllocDisabler;

const FS: f64 = 48000.0;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo)
    }
}

fn stamp(first: u64, frames: usize) -> Stamp {
    Stamp {
        first,
        frames: frames as u32,
        device_ns: first * 20_833,
        callback_ns: first * 20_833 + 1000,
    }
}

fn take() -> Vec<f32> {
    let mut c = Cond::base("cap", 7);
    c.sr = FS;
    c.events = 10;
    make_take(&c).audio
}

fn reference(mono: &[f32]) -> Vec<OnsetEvent> {
    let mut d = OnsetDetector::new(FS, OnsetConfig::default());
    let mut out = Vec::new();
    for b in mono.chunks(256) {
        d.process(b, |e| out.push(e));
    }
    d.flush(|e| out.push(e));
    out
}

fn stage(ch: u16, pick: ChannelPick, max_block: usize) -> (CaptureStage, CaptureOutputs) {
    let (r, o) = CaptureRings::new(FS);
    (CaptureStage::new(FS, ch, pick, max_block, r), o)
}

fn drain_onsets(o: &mut CaptureOutputs, into: &mut Vec<OnsetEvent>) {
    while let Ok(e) = o.events.pop() {
        if let InEvent::Onset(e) = e {
            into.push(e);
        }
    }
}

/// Runs the stage over interleaved audio with block sizes from `next`.
fn run(
    st: &mut CaptureStage,
    o: &mut CaptureOutputs,
    inter: &[f32],
    ch: usize,
    mut next: impl FnMut() -> usize,
) -> Vec<OnsetEvent> {
    let total = inter.len() / ch;
    let mut out = Vec::new();
    let mut at = 0;
    while at < total {
        let n = next().min(total - at);
        st.process(&inter[at * ch..(at + n) * ch], stamp(at as u64, n));
        drain_onsets(o, &mut out);
        at += n;
    }
    st.flush();
    drain_onsets(o, &mut out);
    out
}

#[test]
fn split_invariance_matches_direct_detector() {
    let mono = take();
    let want = reference(&mono);
    assert!(
        want.iter().any(|e| matches!(e, OnsetEvent::Strum(_))),
        "test take must contain strums"
    );
    for &b in &[1usize, 7, 64, 256, 480, 4096] {
        let (mut st, mut o) = stage(1, ChannelPick::Index(0), 512);
        let got = run(&mut st, &mut o, &mono, 1, || b);
        assert_eq!(got, want, "block size {b}");
        assert_eq!(st.stats().dropped_in, 0);
    }
    let mut rng = Rng(99);
    let (mut st, mut o) = stage(1, ChannelPick::Index(0), 300);
    let got = run(&mut st, &mut o, &mono, 1, || rng.range(1, 3000) as usize);
    assert_eq!(got, want, "random split (with chunking)");
}

#[test]
fn stereo_picks() {
    let mono = take();
    let n = 20000;
    let other: Vec<f32> = (0..n).map(|i| ((i % 100) as f32 - 50.0) * 0.002).collect();
    let mut inter = Vec::new();
    for i in 0..n {
        inter.push(mono[i]);
        inter.push(other[i]);
    }
    let expect = |f: &dyn Fn(f32, f32) -> f32| -> Vec<f32> {
        (0..n).map(|i| f(mono[i], other[i])).collect()
    };
    let cases: [(ChannelPick, Vec<f32>); 3] = [
        (ChannelPick::Index(0), expect(&|a, _| a)),
        (ChannelPick::Index(1), expect(&|_, b| b)),
        (ChannelPick::Mean, expect(&|a, b| (a + b) * 0.5)),
    ];
    for (pick, want) in cases {
        let (mut st, mut o) = stage(2, pick, 1024);
        o.set_tap(true);
        st.process(&inter, stamp(0, n));
        let mut got = Vec::new();
        while let Ok(x) = o.tap.pop() {
            got.push(x);
        }
        assert_eq!(got.len(), n);
        for (g, w) in got.iter().zip(&want) {
            assert!((g - w).abs() < 1e-6, "{pick:?}: {g} vs {w}");
        }
    }
    // Out-of-range index clamps to the last channel.
    let (mut st, mut o) = stage(2, ChannelPick::Index(9), 1024);
    o.set_tap(true);
    st.process(&inter[..2000], stamp(0, 1000));
    assert_eq!(o.tap.pop().unwrap(), other[0]);
}

#[test]
fn block_events_carry_time_mapping() {
    let mono = take();
    let (mut st, mut o) = stage(1, ChannelPick::Index(0), 128);
    // Callback 1 at frame 1000, then a 500-frame input gap, callback 2.
    st.process(&mono[..300], stamp(1000, 300));
    st.process(&mono[300..700], stamp(1800, 400));
    let mut blocks = Vec::new();
    while let Ok(e) = o.events.pop() {
        if let InEvent::Block { stamp, det_first } = e {
            blocks.push((stamp.first, det_first));
        }
    }
    assert_eq!(blocks, vec![(1000, 0), (1800, 300)]);
    assert_eq!(o.stats().frames, 700);
    st.reset();
    st.process(&mono[..10], stamp(5000, 10));
    let mut last = None;
    while let Ok(e) = o.events.pop() {
        last = Some(e);
    }
    assert!(matches!(last, Some(InEvent::Block { det_first: 0, .. })));
}

#[test]
fn event_ring_overflow_drops_and_counts() {
    let mono = take();
    let (r, mut o) = CaptureRings::with_capacity(FS, 8, 8);
    let mut st = CaptureStage::new(FS, 1, ChannelPick::Index(0), 256, r);
    // Nobody drains: 1000 blocks. Must return promptly and never panic.
    for k in 0..1000 {
        let a = (k * 64) % (mono.len() - 64);
        st.process(&mono[a..a + 64], stamp(k as u64 * 64, 64));
    }
    let s = st.stats();
    assert_eq!(s.blocks, 1000);
    assert!(s.dropped_in >= 992, "dropped_in = {}", s.dropped_in);
    assert!(s.dropped_pitch > 0, "pitch ring should overflow too");
    let mut n = 0;
    while o.events.pop().is_ok() {
        n += 1;
    }
    assert_eq!(n, 8);
}

/// An onset in the ring always follows its own `Block`: when the `Block` push fails the
/// onsets of that callback are dropped as well, even if the consumer frees room meanwhile.
/// A consumer thread races a tiny ring; every onset must lie at or before the end of the
/// latest `Block` that came before it.
#[test]
fn onsets_never_sit_behind_a_lost_block() {
    let mono = take();
    let (r, mut o) = CaptureRings::with_capacity(FS, 2, 64);
    let mut st = CaptureStage::new(FS, 1, ChannelPick::Index(0), 256, r);
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let d2 = done.clone();
    let consumer = std::thread::spawn(move || {
        let (mut onsets, mut jumps, mut expect) = (0u32, 0u32, 0u64);
        let mut last_end = None::<f64>;
        loop {
            let finished = d2.load(std::sync::atomic::Ordering::Acquire);
            while let Ok(ev) = o.events.pop() {
                match ev {
                    InEvent::Block { det_first, .. } => {
                        jumps += u32::from(det_first != expect);
                        expect = det_first + 64;
                        last_end = Some(expect as f64);
                    }
                    InEvent::Onset(e) => {
                        let (t, at) = match e {
                            OnsetEvent::Heard { t, at } => (t, at),
                            OnsetEvent::Strum(s) => (s.t, s.t),
                        };
                        let end = last_end.expect("onset before any block");
                        assert!(t <= end + 1.0 && at <= end + 1.0, "{t} {at} > {end}");
                        onsets += 1;
                    }
                }
            }
            if finished {
                break;
            }
            std::thread::yield_now();
        }
        (onsets, jumps)
    });
    for _ in 0..300 {
        let mut k = 0usize;
        while (k + 1) * 64 <= mono.len() {
            st.process(&mono[k * 64..(k + 1) * 64], stamp(k as u64 * 64, 64));
            k += 1;
        }
        st.reset();
    }
    done.store(true, std::sync::atomic::Ordering::Release);
    let (onsets, _) = consumer.join().unwrap();
    assert!(onsets > 0, "no onsets seen");
    assert!(st.stats().dropped_in > 0);
}

#[test]
fn tap_holds_two_seconds_in_order() {
    let (mut st, mut o) = stage(1, ChannelPick::Index(0), 1000);
    // Off by default.
    let ramp: Vec<f32> = (0..96_000 + 5000).map(|i| i as f32).collect();
    st.process(&ramp[..1000], stamp(0, 1000));
    assert!(o.tap.pop().is_err());
    assert_eq!(o.stats().tap_dropped, 0);

    o.set_tap(true);
    for (k, c) in ramp[1000..].chunks(777).enumerate() {
        st.process(c, stamp(1000 + k as u64 * 777, c.len()));
    }
    assert_eq!(o.tap_origin(), 1000);
    assert_eq!(o.tap.slots(), 96_000);
    let got: Vec<f32> = std::iter::from_fn(|| o.tap.pop().ok()).collect();
    assert_eq!(got.len(), 96_000);
    for (i, v) in got.iter().enumerate() {
        assert_eq!(*v, (1000 + i) as f32, "sample {i}");
    }
    assert_eq!(o.stats().tap_dropped, 4000);
}

#[test]
fn non_finite_samples_are_zeroed_and_counted() {
    let mono = take();
    let mut dirty = mono[..48_000].to_vec();
    for i in (100..dirty.len()).step_by(1000) {
        dirty[i] = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY][i / 1000 % 3];
    }
    let bad = dirty.iter().filter(|x| !x.is_finite()).count() as u64;
    let (mut st, mut o) = stage(1, ChannelPick::Index(0), 512);
    o.set_tap(true);
    st.process(&dirty, stamp(0, dirty.len()));
    assert_eq!(o.stats().nonfinite, bad);
    let mut clean = mono[..48_000].to_vec();
    for (c, d) in clean.iter_mut().zip(&dirty) {
        if !d.is_finite() {
            *c = 0.0;
        }
    }
    let got: Vec<f32> = std::iter::from_fn(|| o.tap.pop().ok()).collect();
    assert_eq!(got, clean);
    // The detector and tuner stay healthy afterwards.
    let rest = &mono[48_000..];
    st.process(rest, stamp(48_000, rest.len()));
    while let Ok(f) = o.pitch.pop() {
        assert!(f.t.is_finite() && f.f.is_finite() && f.rms.is_finite());
    }
    // A bad sample on a channel that is not picked is never read; under Mean it is counted.
    let (mut st2, o2) = stage(2, ChannelPick::Index(0), 64);
    st2.process(&[0.5, f32::NAN, 0.25, 1.0], stamp(0, 2));
    assert_eq!(o2.stats().nonfinite, 0);
    let (mut st3, o3) = stage(2, ChannelPick::Mean, 64);
    st3.process(&[0.5, f32::NAN, 0.25, 1.0], stamp(0, 2));
    assert_eq!(o3.stats().nonfinite, 1);
}

#[test]
fn pitch_frames_flow() {
    let mono = take();
    let (mut st, mut o) = stage(1, ChannelPick::Index(0), 512);
    for (k, c) in mono.chunks(480).enumerate() {
        st.process(c, stamp(k as u64 * 480, c.len()));
        while o.pitch.pop().is_ok() {}
        while o.events.pop().is_ok() {}
    }
    assert_eq!(st.stats().dropped_pitch, 0);
}

fn exercise(st: &mut CaptureStage, o: &mut CaptureOutputs, inter: &[f32], ch: usize) {
    let total = inter.len() / ch;
    let mut at = 0;
    while at < total {
        let n = 480.min(total - at);
        let s = stamp(at as u64, n);
        assert_no_alloc(|| st.process(&inter[at * ch..(at + n) * ch], s));
        while o.events.pop().is_ok() {}
        while o.pitch.pop().is_ok() {}
        while o.tap.pop().is_ok() {}
        at += n;
    }
}

#[test]
fn no_alloc_in_process() {
    let mono = take();
    let mut inter = Vec::new();
    for &x in &mono {
        inter.push(x);
        inter.push(x * 0.5);
    }
    inter[4000] = f32::NAN;
    let (mut st, mut o) = stage(2, ChannelPick::Mean, 480);
    o.set_tap(true);
    exercise(&mut st, &mut o, &inter, 2);
    // Larger than max_block (chunked) and with full rings.
    let (r, mut o2) = CaptureRings::with_capacity(FS, 4, 4);
    let mut st2 = CaptureStage::new(FS, 2, ChannelPick::Index(1), 100, r);
    o2.set_tap(true);
    let s = stamp(0, 20_000);
    assert_no_alloc(|| st2.process(&inter[..40_000], s));
    assert_no_alloc(|| st2.flush());
    exercise(&mut st2, &mut o2, &inter, 2);
    assert!(st2.stats().dropped_in == 0 || st2.stats().frames > 0);
}

/// Child half of the negative control: allocates inside `assert_no_alloc`, so the
/// process must abort. Does nothing unless the parent sets the variable.
#[test]
fn negative_control_child() {
    if std::env::var_os("TONICA_ALLOC_CHILD").is_none() {
        return;
    }
    assert_no_alloc(|| {
        let v: Vec<u8> = std::hint::black_box(Vec::with_capacity(64));
        std::hint::black_box(&v);
    });
}

#[test]
fn negative_control_aborts() {
    let exe = std::env::current_exe().unwrap();
    let st = std::process::Command::new(exe)
        .args(["--exact", "negative_control_child", "--nocapture"])
        .env("TONICA_ALLOC_CHILD", "1")
        .status()
        .unwrap();
    assert!(!st.success(), "allocating under assert_no_alloc must abort");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(st.signal(), Some(6), "expected SIGABRT, got {st:?}");
    }
}

// ---- Raw ring (A0) ----

/// Everything a stage published, drained.
#[derive(Debug, PartialEq)]
struct Drained {
    events: Vec<InEvent>,
    pitch_t: Vec<f64>,
    tap: Vec<f32>,
    stats: tonica_audio::CaptureStats,
}

fn drain_all(o: &mut CaptureOutputs) -> Drained {
    Drained {
        events: std::iter::from_fn(|| o.events.pop().ok()).collect(),
        pitch_t: std::iter::from_fn(|| o.pitch.pop().ok().map(|f| f.t)).collect(),
        tap: std::iter::from_fn(|| o.tap.pop().ok()).collect(),
        stats: o.stats(),
    }
}

fn read_all(raw: &mut RawOutput) -> Vec<(RawMark, Vec<f32>)> {
    let mut buf = Vec::new();
    std::iter::from_fn(|| raw.read_block(&mut buf).map(|m| (m, buf.clone()))).collect()
}

/// Feeds `inter` in callbacks of `n` frames with `stamp.first = at`, draining as it goes.
fn feed_drain(
    st: &mut CaptureStage,
    o: &mut CaptureOutputs,
    raw: &mut RawOutput,
    inter: &[f32],
    ch: usize,
    n: usize,
) -> Vec<(RawMark, Vec<f32>)> {
    let total = inter.len() / ch;
    let mut out = Vec::new();
    let mut at = 0;
    while at < total {
        let k = n.min(total - at);
        st.process(&inter[at * ch..(at + k) * ch], stamp(at as u64, k));
        while o.events.pop().is_ok() {}
        while o.pitch.pop().is_ok() {}
        out.extend(read_all(raw));
        at += k;
    }
    out
}

#[test]
fn raw_off_changes_nothing() {
    let mono = take();
    let src = &mono[..96_000];
    let run_one = |mode: u8| {
        let (mut st, mut o) = stage(1, ChannelPick::Index(0), 480);
        let mut raw = o.take_raw().expect("raw consumer");
        o.set_tap(true);
        if mode == 1 {
            // Switched on then off before any audio: same as never touched.
            o.set_raw(true);
            o.set_raw(false);
        }
        if mode == 2 {
            o.set_raw(true);
        }
        for (k, c) in src.chunks(700).enumerate() {
            st.process(c, stamp(k as u64 * 700, c.len()));
            if mode == 2 {
                read_all(&mut raw);
            }
        }
        st.flush();
        let d = drain_all(&mut o);
        let blocks = read_all(&mut raw);
        (d, blocks.len(), raw.dropped_blocks())
    };
    let (never, n0, d0) = run_one(0);
    let (off, n1, d1) = run_one(1);
    let (on, n2, d2) = run_one(2);
    assert_eq!((n0, d0, n1, d1), (0, 0, 0, 0), "off: ring untouched");
    assert_eq!(never, off);
    assert_eq!(n2, 0, "drained inside the loop");
    assert_eq!(d2, 0);
    // The other outputs do not depend on the raw ring either.
    assert_eq!(never, on);
    assert!(never.events.len() > 10 && !never.tap.is_empty());
    // The consumer can be taken only once.
    let (_st, mut o) = stage(1, ChannelPick::Index(0), 64);
    assert!(o.take_raw().is_some());
    assert!(o.take_raw().is_none());
}

#[test]
fn raw_blocks_equal_downmix_and_are_contiguous_for_any_split() {
    let mono = take();
    let frames = 30_000;
    let mut inter = Vec::with_capacity(frames * 2);
    for &x in &mono[..frames] {
        inter.push(x);
        inter.push(x * 0.5 - 0.01);
    }
    for (ch, pick) in [(2u16, ChannelPick::Mean), (2, ChannelPick::Index(1))] {
        let expect: Vec<f32> = inter
            .as_chunks::<2>()
            .0
            .iter()
            .map(|f| match pick {
                ChannelPick::Mean => (0.0 + f[0] + f[1]) * 0.5,
                ChannelPick::Index(i) => f[usize::from(i)],
            })
            .collect();
        for &max_block in &[100usize, 480, 4096] {
            for &n in &[1usize, 2, 3, 7, 64, 480, 1000, 4096] {
                // One-frame callbacks over the whole take would be slow; cap the length.
                let len = if n < 7 { 3000 } else { frames };
                let (r, mut o) = CaptureRings::new(FS);
                let mut st = CaptureStage::new(FS, ch, pick, max_block, r);
                let mut raw = o.take_raw().unwrap();
                o.set_raw(true);
                let blocks = feed_drain(&mut st, &mut o, &mut raw, &inter[..len * 2], 2, n);
                let mut cat = Vec::new();
                let (mut in_next, mut det_next) = (0u64, 0u64);
                for (m, b) in &blocks {
                    assert_eq!(m.frames as usize, b.len());
                    assert!(m.frames as usize <= max_block && m.frames > 0);
                    assert_eq!(m.in_first, in_next, "n={n} mb={max_block}");
                    assert_eq!(m.det_first, det_next, "n={n} mb={max_block}");
                    in_next += u64::from(m.frames);
                    det_next += u64::from(m.frames);
                    cat.extend_from_slice(b);
                }
                assert_eq!(cat.len(), len, "n={n} mb={max_block}");
                assert_eq!(cat, expect[..len], "n={n} mb={max_block}");
                assert_eq!(raw.dropped_blocks(), 0);
            }
        }
    }
}

#[test]
fn raw_marks_follow_stamp_jumps_in_input_but_not_in_detector_time() {
    let mono = take();
    let (mut st, mut o) = stage(1, ChannelPick::Index(0), 128);
    let mut raw = o.take_raw().unwrap();
    o.set_raw(true);
    let mut firsts = Vec::new();
    let mut jump = 0u64;
    for (k, c) in mono[..256 * 6].chunks(256).enumerate() {
        if k == 3 {
            jump = 10_000; // input gap: the device skipped 10 000 frames
        }
        st.process(c, stamp(k as u64 * 256 + jump, c.len()));
        firsts.push(read_all(&mut raw));
    }
    let blocks: Vec<_> = firsts.into_iter().flatten().collect();
    assert_eq!(blocks.len(), 12); // 6 callbacks, 2 chunks of 128 each
    for (i, (m, b)) in blocks.iter().enumerate() {
        assert_eq!(
            m.det_first,
            i as u64 * 128,
            "detector time stays continuous"
        );
        let extra = if i >= 6 { 10_000 } else { 0 };
        assert_eq!(m.in_first, i as u64 * 128 + extra, "input frame jumps");
        assert_eq!(b[..], mono[i * 128..(i + 1) * 128]);
    }
}

#[test]
fn raw_stalled_consumer_drops_whole_blocks_and_recovers() {
    let mono = take();
    let n = 700usize; // does not divide the ring: the last block that fits leaves a remainder
    let cap = (FS * RAW_SECONDS) as usize;
    let (r, mut o) = CaptureRings::new(FS);
    let mut st = CaptureStage::new(FS, 1, ChannelPick::Index(0), 1024, r);
    let mut raw = o.take_raw().unwrap();
    o.set_raw(true);
    let total = (FS * 5.0) as usize; // consumer stopped for 5 s
    let src: Vec<f32> = (0..total + 20_000).map(|i| mono[i % mono.len()]).collect();
    let mut at = 0;
    let mut sent = 0u64;
    while at < total {
        let k = n.min(total - at);
        st.process(&src[at..at + k], stamp(at as u64, k));
        while o.events.pop().is_ok() {}
        while o.pitch.pop().is_ok() {}
        at += k;
        sent += 1;
    }
    let got = read_all(&mut raw);
    let kept = got.len() as u64;
    assert_eq!(kept, (cap / n) as u64, "only whole blocks fit");
    assert_eq!(raw.dropped_blocks(), sent - kept);
    assert_eq!(o.raw_dropped_blocks(), sent - kept);
    assert!(raw.dropped_blocks() > 50);
    for (i, (m, b)) in got.iter().enumerate() {
        assert_eq!(m.frames as usize, n);
        assert_eq!(m.in_first, (i * n) as u64);
        assert_eq!(b[..], src[i * n..(i + 1) * n], "kept blocks are intact");
    }
    // The consumer is back: later blocks arrive intact. `feed_drain` restarts the stamp at 0,
    // so `in_first` is relative to the slice while `det_first` keeps counting from the start.
    let after = feed_drain(&mut st, &mut o, &mut raw, &src[total..], 1, n);
    assert!(!after.is_empty());
    for (m, b) in &after {
        let i = total + m.in_first as usize;
        assert_eq!(
            m.det_first, i as u64,
            "detector time counts every frame, dropped or not"
        );
        assert_eq!(b[..], src[i..i + m.frames as usize]);
        assert_eq!(m.frames as usize, b.len());
    }
    assert_eq!(after[0].0.det_first, total as u64);
    // A full mark ring also drops whole blocks and never leaves orphan samples.
    let (r, mut o) = CaptureRings::new(FS);
    let mut st = CaptureStage::new(FS, 1, ChannelPick::Index(0), 16, r);
    let mut raw = o.take_raw().unwrap();
    o.set_raw(true);
    let few = tonica_audio::capture::RAW_MARK_CAP + 50;
    for k in 0..few {
        st.process(&src[k * 16..(k + 1) * 16], stamp((k * 16) as u64, 16));
        while o.events.pop().is_ok() {}
        while o.pitch.pop().is_ok() {}
    }
    let got = read_all(&mut raw);
    assert_eq!(got.len() as u64 + raw.dropped_blocks(), few as u64);
    assert!(raw.dropped_blocks() >= 1);
    for (m, b) in &got {
        let i = m.in_first as usize;
        assert_eq!(b[..], src[i..i + 16]);
    }
}

#[test]
fn raw_no_alloc_with_ring_on() {
    let mono = take();
    let mut inter = Vec::new();
    for &x in &mono {
        inter.push(x);
        inter.push(x * 0.5);
    }
    inter[4000] = f32::NAN;
    let (mut st, mut o) = stage(2, ChannelPick::Mean, 480);
    let mut raw = o.take_raw().unwrap();
    o.set_raw(true);
    let mut buf = Vec::with_capacity(4096);
    let total = inter.len() / 2;
    let mut at = 0;
    while at < total {
        let n = 480.min(total - at);
        let s = stamp(at as u64, n);
        assert_no_alloc(|| st.process(&inter[at * 2..(at + n) * 2], s));
        while o.events.pop().is_ok() {}
        while o.pitch.pop().is_ok() {}
        while raw.read_block(&mut buf).is_some() {}
        at += n;
    }
    assert_eq!(raw.dropped_blocks(), 0);
    // Chunked callback with full rings (everything dropped, still no allocation).
    let (r, mut o2) = CaptureRings::with_capacity(FS, 4, 4);
    let mut st2 = CaptureStage::new(FS, 2, ChannelPick::Index(1), 100, r);
    let mut raw2 = o2.take_raw().unwrap();
    o2.set_raw(true);
    let s = stamp(0, 20_000);
    for _ in 0..12 {
        assert_no_alloc(|| st2.process(&inter[..40_000], s));
    }
    assert!(raw2.dropped_blocks() > 0);
    assert_eq!(raw2.read_block(&mut buf).map(|m| m.frames), Some(100));
}

#[test]
fn raw_non_finite_becomes_zero_like_the_detector_sees() {
    let mono = take();
    let mut dirty = mono[..48_000].to_vec();
    for i in (100..dirty.len()).step_by(1000) {
        dirty[i] = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY][i / 1000 % 3];
    }
    let (mut st, mut o) = stage(1, ChannelPick::Index(0), 512);
    let mut raw = o.take_raw().unwrap();
    o.set_raw(true);
    o.set_tap(true);
    let blocks = feed_drain(&mut st, &mut o, &mut raw, &dirty, 1, 333);
    let cat: Vec<f32> = blocks.iter().flat_map(|(_, b)| b.iter().copied()).collect();
    assert!(cat.iter().all(|x| x.is_finite()));
    let mut clean = mono[..48_000].to_vec();
    for (c, d) in clean.iter_mut().zip(&dirty) {
        if !d.is_finite() {
            *c = 0.0;
        }
    }
    assert_eq!(cat, clean);
    // Same samples the tap (and so the detector) got.
    let tap: Vec<f32> = std::iter::from_fn(|| o.tap.pop().ok()).collect();
    assert_eq!(cat, tap);
}
