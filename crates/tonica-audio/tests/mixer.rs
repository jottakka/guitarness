//! T2 (exact start, block-split invariance, metronome grid, late, exhaustion)
//! and T7 (no allocation in `render`, with a negative control).

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use tonica_audio::{MixCmd, MixEvent, Mixer, SoundBank, SoundId, VOICES};

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

struct Rig {
    mixer: Mixer,
    tx: rtrb::Producer<MixCmd>,
    rx: rtrb::Consumer<MixEvent>,
    bank: SoundBank,
}

fn rig_with(fs: f64, ev_cap: usize) -> Rig {
    let (tx, crx) = rtrb::RingBuffer::new(256);
    let (etx, rx) = rtrb::RingBuffer::new(ev_cap);
    Rig {
        mixer: Mixer::new(SoundBank::new(fs), crx, etx),
        tx,
        rx,
        bank: SoundBank::new(fs),
    }
}

fn rig() -> Rig {
    rig_with(FS, 65536)
}

impl Rig {
    fn cmd(&mut self, c: MixCmd) {
        self.tx.push(c).unwrap();
    }
    fn events(&mut self, into: &mut Vec<MixEvent>) {
        while let Ok(e) = self.rx.pop() {
            into.push(e);
        }
    }
    fn run_from(
        &mut self,
        from: u64,
        total: u64,
        channels: u16,
        ev: &mut Vec<MixEvent>,
    ) -> Vec<f32> {
        let mut out = Vec::new();
        let mut buf = vec![0.0f32; 480 * channels as usize];
        let mut pos = 0;
        while pos < total {
            self.mixer.render(&mut buf, channels, from + pos);
            out.extend_from_slice(&buf);
            self.events(ev);
            pos += 480;
        }
        out
    }
    /// Renders `total` frames with the given block sizes (cycled). Returns mono-or-interleaved output.
    fn run(
        &mut self,
        total: u64,
        channels: u16,
        mut sizes: impl FnMut() -> usize,
        ev: &mut Vec<MixEvent>,
    ) -> Vec<f32> {
        let mut out = Vec::with_capacity(total as usize * channels as usize);
        let mut pos = 0u64;
        let mut buf = Vec::new();
        while pos < total {
            let n = sizes().min((total - pos) as usize).max(1);
            buf.clear();
            buf.resize(n * channels as usize, 7.0); // dirty on purpose
            self.mixer.render(&mut buf, channels, pos);
            out.extend_from_slice(&buf);
            self.events(ev);
            pos += n as u64;
        }
        out
    }
}

const ALL: [SoundId; 3] = [SoundId::Click, SoundId::Accent, SoundId::Probe];

#[test]
fn t2_exact_start_1000_random_schedules() {
    let mut rng = Rng(1);
    let mut max_err = 0u64;
    for sched in 0..1000 {
        let mut r = rig();
        let mut refo = vec![0.0f32; 30000];
        let mut want = Vec::new();
        let mut at = rng.range(0, 700);
        let n = rng.range(1, 12);
        for tag in 0..n as u32 {
            let id = ALL[rng.range(0, 3) as usize];
            let gain = [1.0f32, 0.5, 0.25][rng.range(0, 3) as usize];
            let snd = r.bank.get(id).to_vec();
            if at as usize + snd.len() >= refo.len() {
                break;
            }
            for (i, s) in snd.iter().enumerate() {
                refo[at as usize + i] = s * gain;
            }
            r.cmd(MixCmd::Play {
                tag,
                sound: id,
                at,
                gain,
            });
            want.push(MixEvent::Started { tag, at });
            at += snd.len() as u64 + rng.range(0, 2000);
        }
        let mut ev = Vec::new();
        let out = r.run(30000, 1, || rng.range(1, 1100) as usize, &mut ev);
        assert_eq!(ev, want, "schedule {sched}");
        // Start error: first differing index between output and reference is none.
        let err = out
            .iter()
            .zip(&refo)
            .position(|(a, b)| a.to_bits() != b.to_bits())
            .map_or(0, |p| p as u64 + 1);
        max_err = max_err.max(err);
        assert_eq!(err, 0, "schedule {sched}: first mismatch at sample {err}");
    }
    eprintln!("T2 max start error over 1000 schedules: {max_err} samples");
}

fn busy_schedule(r: &mut Rig, rng: &mut Rng) {
    for tag in 1..=40u32 {
        r.cmd(MixCmd::Play {
            tag,
            sound: ALL[rng.range(0, 3) as usize],
            at: rng.range(0, 40000),
            gain: 0.3,
        });
    }
    r.cmd(MixCmd::Metronome {
        tag: 100,
        start: 123,
        period: 4999.37,
        accent_every: 4,
        beats: 0,
        gain: 0.8,
    });
    r.cmd(MixCmd::Metronome {
        tag: 101,
        start: 5000,
        period: 777.77,
        accent_every: 0,
        beats: 30,
        gain: 0.5,
    });
    r.cmd(MixCmd::StopTag { tag: 3, at: 2000 });
    r.cmd(MixCmd::StopTag {
        tag: 100,
        at: 38000,
    });
}

#[test]
fn t2_block_split_invariance() {
    for channels in [1u16, 2, 6] {
        let mut outs: Vec<(Vec<f32>, Vec<MixEvent>)> = Vec::new();
        for mode in 0..5u64 {
            let mut r = rig();
            busy_schedule(&mut r, &mut Rng(9));
            let mut ev = Vec::new();
            let mut rng = Rng(100 + mode);
            let out = match mode {
                0 => r.run(60000, channels, || 64, &mut ev),
                1 => r.run(60000, channels, || 480, &mut ev),
                2 => r.run(60000, channels, || 1, &mut ev),
                _ => r.run(60000, channels, || rng.range(1, 3000) as usize, &mut ev),
            };
            outs.push((out, ev));
        }
        // Events of different metronomes inside one block come out track by track, so
        // compare them in a canonical order (the set and the frame numbers must match).
        for (_, ev) in outs.iter_mut() {
            ev.sort_by_key(|e| match *e {
                MixEvent::Started { tag, at } => (0, at, tag, 0),
                MixEvent::Beat { tag, index, at } => (1, at, tag, index),
                MixEvent::Late { tag, by } => (2, by as u64, tag, 0),
                MixEvent::Rejected { tag } => (3, 0, tag, 0),
            });
        }
        let (base, base_ev) = &outs[0];
        assert!(base.iter().any(|v| *v != 0.0));
        let c = channels as usize;
        for f in 0..base.len() / c {
            for k in 1..c {
                assert_eq!(base[f * c].to_bits(), base[f * c + k].to_bits());
            }
        }
        for (out, ev) in &outs[1..] {
            assert!(
                out.iter()
                    .zip(base)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "output differs ({channels} ch)"
            );
            assert_eq!(ev, base_ev, "events differ ({channels} ch)");
        }
    }
}

#[test]
fn t2_metronome_beats_exact_over_10000_beats() {
    for (i, (period, accent)) in [
        (1000.5f64, 4u8),
        (1234.567, 3),
        (777.25, 0),
        (48000.0 / 3.0, 1),
    ]
    .into_iter()
    .enumerate()
    {
        let mut r = rig();
        let start = 1000 + i as u64 * 37;
        r.cmd(MixCmd::Metronome {
            tag: 5,
            start,
            period,
            accent_every: accent,
            beats: 10_000,
            gain: 1.0,
        });
        let last = start + (9999.0 * period).round() as u64;
        let total = last + 2000;
        let mut ev = Vec::new();
        let mut rng = Rng(77 + i as u64);
        let out = r.run(total, 1, || rng.range(1, 2000) as usize, &mut ev);
        // Reference: place the clicks directly.
        let mut refo = vec![0.0f32; total as usize];
        let mut beats = Vec::new();
        for k in 0..10_000u64 {
            let pos = start + (k as f64 * period).round() as u64;
            let id = if accent > 0 && k % accent as u64 == 0 {
                SoundId::Accent
            } else {
                SoundId::Click
            };
            for (j, s) in r.bank.get(id).iter().enumerate() {
                refo[pos as usize + j] += s;
            }
            beats.push(MixEvent::Beat {
                tag: 5,
                index: k as u32,
                at: pos,
            });
        }
        let mut want = vec![MixEvent::Started { tag: 5, at: start }];
        want.extend(beats);
        assert_eq!(ev, want, "period {period}");
        assert!(
            out.iter()
                .zip(&refo)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "audio differs, period {period}"
        );
        assert_eq!(r.mixer.active(), 0, "metronome should have finished");
    }
}

#[test]
fn late_play_is_in_phase_with_fade_in() {
    let mut r = rig();
    let mut ev = Vec::new();
    let _ = r.run(1000, 1, || 100, &mut ev);
    assert!(ev.is_empty());
    r.cmd(MixCmd::Play {
        tag: 9,
        sound: SoundId::Probe,
        at: 600,
        gain: 1.0,
    });
    let mut out = vec![0.0f32; 480];
    r.mixer.render(&mut out, 1, 1000);
    r.events(&mut ev);
    assert_eq!(ev, vec![MixEvent::Late { tag: 9, by: 400 }]);
    let snd = r.bank.get(SoundId::Probe);
    let fade = (0.001 * FS).round() as usize;
    for i in fade..480 {
        let want = if 400 + i < snd.len() {
            snd[400 + i]
        } else {
            0.0
        };
        assert_eq!(out[i].to_bits(), want.to_bits(), "frame {i}");
    }
    // The fade-in is a ramp: below the in-phase value.
    for i in 0..fade {
        let g = (i + 1) as f32 / fade as f32;
        assert_eq!(out[i].to_bits(), (snd[400 + i] * g).to_bits());
    }
    // Fully elapsed: Late and nothing else.
    let mut ev2 = Vec::new();
    r.cmd(MixCmd::Play {
        tag: 10,
        sound: SoundId::Click,
        at: 0,
        gain: 1.0,
    });
    r.mixer.render(&mut out, 1, 5000);
    r.events(&mut ev2);
    assert_eq!(ev2, vec![MixEvent::Late { tag: 10, by: 5000 }]);
    assert!(out.iter().all(|v| *v == 0.0));
}

#[test]
fn late_metronome_keeps_the_grid() {
    let mut r = rig();
    let (start, period) = (100u64, 1000.4f64);
    r.cmd(MixCmd::Metronome {
        tag: 2,
        start,
        period,
        accent_every: 0,
        beats: 0,
        gain: 1.0,
    });
    let mut out = vec![0.0f32; 4800];
    r.mixer.render(&mut out, 1, 10_000);
    let mut ev = Vec::new();
    r.events(&mut ev);
    assert!(matches!(ev[0], MixEvent::Late { tag: 2, by: 9900 }));
    for e in &ev[1..] {
        let MixEvent::Beat { index, at, .. } = *e else {
            panic!("{e:?}")
        };
        assert_eq!(at, start + (index as f64 * period).round() as u64);
        assert!((10_000..14_800).contains(&at));
    }
    assert_eq!(ev.len(), 1 + 5);
}

#[test]
fn voice_exhaustion_rejects() {
    let mut r = rig();
    for tag in 0..(VOICES as u32 + 3) {
        r.cmd(MixCmd::Play {
            tag,
            sound: SoundId::Click,
            at: 100,
            gain: 0.01,
        });
    }
    for tag in 0..5u32 {
        r.cmd(MixCmd::Metronome {
            tag: 1000 + tag,
            start: 100,
            period: 5000.0,
            accent_every: 0,
            beats: 3,
            gain: 0.1,
        });
    }
    let mut out = vec![0.0f32; 480];
    r.mixer.render(&mut out, 1, 0);
    let mut ev = Vec::new();
    r.events(&mut ev);
    let started = ev
        .iter()
        .filter(|e| matches!(e, MixEvent::Started { .. }))
        .count();
    let rejected: Vec<u32> = ev
        .iter()
        .filter_map(|e| match e {
            MixEvent::Rejected { tag } => Some(*tag),
            _ => None,
        })
        .collect();
    assert_eq!(started, VOICES + 4);
    assert_eq!(rejected, vec![32, 33, 34, 1004]);
    // Voices free up after the sounds end.
    let mut out = vec![0.0f32; 4800];
    r.mixer.render(&mut out, 1, 480);
    r.cmd(MixCmd::Play {
        tag: 77,
        sound: SoundId::Click,
        at: 6000,
        gain: 1.0,
    });
    r.mixer.render(&mut out, 1, 5280);
    let mut ev = Vec::new();
    r.events(&mut ev);
    assert!(ev.contains(&MixEvent::Started { tag: 77, at: 6000 }));
}

#[test]
fn stop_and_silence() {
    let mut r = rig();
    r.cmd(MixCmd::Metronome {
        tag: 1,
        start: 0,
        period: 1000.0,
        accent_every: 0,
        beats: 0,
        gain: 1.0,
    });
    r.cmd(MixCmd::StopTag { tag: 1, at: 5500 });
    let mut ev = Vec::new();
    let out = r.run(12000, 1, || 480, &mut ev);
    let beats = ev
        .iter()
        .filter(|e| matches!(e, MixEvent::Beat { .. }))
        .count();
    assert_eq!(beats, 6); // 0..=5000
    assert!(out[5500..].iter().all(|v| *v == 0.0));
    assert_eq!(r.mixer.active(), 0);

    r.cmd(MixCmd::Metronome {
        tag: 2,
        start: 12000,
        period: 1000.0,
        accent_every: 0,
        beats: 0,
        gain: 1.0,
    });
    r.cmd(MixCmd::Play {
        tag: 3,
        sound: SoundId::Probe,
        at: 20000,
        gain: 1.0,
    });
    r.cmd(MixCmd::Silence);
    let mut buf = vec![0.0f32; 4800];
    r.mixer.render(&mut buf, 1, 12000);
    assert!(buf[100..].iter().all(|v| *v == 0.0));
    assert_eq!(r.mixer.active(), 0);

    r.cmd(MixCmd::MasterGain(0.5));
    r.cmd(MixCmd::Play {
        tag: 4,
        sound: SoundId::Click,
        at: 20000,
        gain: 1.0,
    });
    let mut buf = vec![0.0f32; 480];
    r.mixer.render(&mut buf, 1, 20000);
    let snd = r.bank.get(SoundId::Click);
    assert_eq!(buf[100].to_bits(), (snd[100] * 0.5).to_bits());
}

#[test]
fn full_event_queue_drops_and_counts() {
    let mut r = rig_with(FS, 2);
    for tag in 0..10 {
        r.cmd(MixCmd::Play {
            tag,
            sound: SoundId::Click,
            at: 10,
            gain: 0.1,
        });
    }
    let mut out = vec![0.0f32; 480];
    r.mixer.render(&mut out, 2, 0);
    assert_eq!(r.mixer.dropped_events(), 8);
}

#[test]
fn works_at_44100() {
    let mut r = rig_with(44100.0, 1024);
    r.cmd(MixCmd::Play {
        tag: 1,
        sound: SoundId::Accent,
        at: 77,
        gain: 1.0,
    });
    let mut ev = Vec::new();
    let out = r.run(2000, 2, || 441, &mut ev);
    assert_eq!(ev, vec![MixEvent::Started { tag: 1, at: 77 }]);
    let snd = r.bank.get(SoundId::Accent);
    assert_eq!(out[(77 + 100) * 2].to_bits(), snd[100].to_bits());
}

#[test]
fn render_returns_while_a_producer_spams_commands() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    let Rig {
        mut mixer, mut tx, ..
    } = rig();
    let stop = Arc::new(AtomicBool::new(false));
    let sent = Arc::new(AtomicU64::new(0));
    let (s2, n2) = (stop.clone(), sent.clone());
    let h = std::thread::spawn(move || {
        while !s2.load(Ordering::Relaxed) {
            if tx.push(MixCmd::MasterGain(1.0)).is_ok() {
                n2.fetch_add(1, Ordering::Relaxed);
            }
        }
    });
    while sent.load(Ordering::Relaxed) == 0 {
        std::hint::spin_loop();
    }
    let mut buf = vec![0.0f32; 480];
    for i in 0..2000u64 {
        mixer.render(&mut buf, 1, i * 480);
    }
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
    assert!(sent.load(Ordering::Relaxed) > 0);
}

#[test]
fn huge_frame_numbers_are_rejected_without_panic() {
    let mut r = rig();
    for at in [u64::MAX, u64::MAX - 1, (1 << 62) + 1] {
        r.cmd(MixCmd::Play {
            tag: 1,
            sound: SoundId::Probe,
            at,
            gain: 1.0,
        });
        r.cmd(MixCmd::Metronome {
            tag: 2,
            start: at,
            period: 1000.0,
            accent_every: 0,
            beats: 0,
            gain: 1.0,
        });
        r.cmd(MixCmd::StopTag { tag: 3, at });
    }
    for (start, period, beats) in [
        (0u64, 1e300, 5u32),
        (0, f64::NAN, 0),
        (0, f64::INFINITY, 0),
        (1 << 62, 1e18, 1000),
        (u64::MAX - 5, 1.0, 10),
    ] {
        r.cmd(MixCmd::Metronome {
            tag: 4,
            start,
            period,
            accent_every: 0,
            beats,
            gain: 1.0,
        });
    }
    let mut out = vec![0.0f32; 480];
    r.mixer.render(&mut out, 1, 0);
    let mut ev = Vec::new();
    r.events(&mut ev);
    assert_eq!(ev.len(), 14, "{ev:?}");
    assert!(ev.iter().all(|e| matches!(e, MixEvent::Rejected { .. })));
    assert_eq!(r.mixer.active(), 0);
    // Rendering at a huge frame index must not panic either.
    r.cmd(MixCmd::Play {
        tag: 5,
        sound: SoundId::Click,
        at: u64::MAX - 10,
        gain: 1.0,
    });
    r.mixer.render(&mut out, 2, u64::MAX - 100);
    // A valid metronome near the limit works.
    r.cmd(MixCmd::Metronome {
        tag: 6,
        start: (1 << 62) - 10_000,
        period: 1000.0,
        accent_every: 0,
        beats: 5,
        gain: 1.0,
    });
    let mut ev = Vec::new();
    let _ = r.run_from((1 << 62) - 10_000, 12_000, 1, &mut ev);
    assert_eq!(
        ev.iter()
            .filter(|e| matches!(e, MixEvent::Beat { .. }))
            .count(),
        5
    );
}

// ---- T7: allocation ----

#[test]
fn t7_render_does_not_allocate() {
    let mut r = rig();
    let mut rng = Rng(5);
    let mut buf = vec![0.0f32; 480 * 2];
    let mut pos = 0u64;
    for block in 0..2000u32 {
        assert_no_alloc(|| {
            if block % 7 == 0 {
                let _ = r.tx.push(MixCmd::Play {
                    tag: block,
                    sound: ALL[(block % 3) as usize],
                    at: pos + rng.range(0, 5000),
                    gain: 0.4,
                });
            }
            if block == 3 {
                let _ = r.tx.push(MixCmd::Metronome {
                    tag: 900,
                    start: pos + 10,
                    period: 5333.3,
                    accent_every: 4,
                    beats: 0,
                    gain: 0.5,
                });
            }
            if block == 1500 {
                let _ = r.tx.push(MixCmd::StopTag { tag: 900, at: pos });
                let _ = r.tx.push(MixCmd::Silence);
            }
            if block % 11 == 0 {
                // Late commands and rejects exercise the other branches.
                let _ = r.tx.push(MixCmd::Play {
                    tag: 5000 + block,
                    sound: SoundId::Probe,
                    at: pos.saturating_sub(100),
                    gain: 0.2,
                });
            }
            r.mixer.render(&mut buf, 2, pos);
        });
        pos += 480;
        while r.rx.pop().is_ok() {}
    }
    assert!(buf.iter().all(|v| v.is_finite()));
}

/// Child half of the negative control: allocates inside `assert_no_alloc`, so the
/// process must abort. Does nothing unless the parent sets the variable.
#[test]
fn t7_negative_control_child() {
    if std::env::var_os("TONICA_ALLOC_CHILD").is_none() {
        return;
    }
    assert_no_alloc(|| {
        let v: Vec<u8> = std::hint::black_box(Vec::with_capacity(64));
        std::hint::black_box(&v);
    });
    // Reaching here means the check did not fire.
}

#[test]
fn t7_negative_control_aborts() {
    let exe = std::env::current_exe().unwrap();
    let st = std::process::Command::new(exe)
        .args(["--exact", "t7_negative_control_child", "--nocapture"])
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
