//! S1: tape format, writer and reader. Synthetic data only: no device, no microphone.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use guitarness_audio::capture::{CaptureRings, CaptureStage};
use guitarness_audio::clock::{ClockParts, ClockQuality};
use guitarness_audio::stats::Ring;
use guitarness_audio::{
    BackendEvent, ChannelPick, ClockSnapshot, ClockStep, EngineEvent, MixEvent, OnsetNote, Side,
    Stamp, StampSource, StepCause,
};
use guitarness_core::gates::GateTable;
use guitarness_core::judge::JudgeConfig;
use guitarness_core::stats::{NoClaim, RoundStats, Tendency};
use guitarness_core::timing::{Level, TimingRef};
use guitarness_core::verdict::{Attack, Direction, JudgeOut, PulseVerdict, Timing, Why};
use guitarness_dsp::{OnsetEvent, Strum};
use guitarness_session::format::{
    Decoded, Json, SessionCmdLog, TAPE_VERSION, TakeSpan, TapeError, TapeEvent, TapeHeader,
    decode_line, event_line, header_from_text, header_to_text, new_tape_dir, tape_folder_name,
    utc_iso,
};
use guitarness_session::reader::{Tape, prune};
use guitarness_session::rig::{HitKind, TruthStrum};
use guitarness_session::writer::TapeWriter;

const FS: u32 = 48_000;

/// A fresh temp folder, removed on drop.
struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Tmp {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "guitarness-tape-io-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn parts() -> ClockParts {
    ClockParts {
        epoch: 3,
        quality: ClockQuality::Locked,
        n_in: 12345.0,
        t_in: 0.123_456_789_012_345_67,
        p_in: 1.0 / 48_000.0,
        n_out: 54321.5,
        t_out: 1.000_000_000_000_000_2,
        p_out: 1.0 / 44_100.0 + 1e-12,
        fs_in: 48_000.0,
        fs_out: 44_100.0,
        src_in: StampSource::Device,
        src_out: StampSource::Callback,
        base_ns: Some(1_700_000_000_123_456_789_012_345),
    }
}

fn stats() -> RoundStats {
    RoundStats {
        events: 32,
        heard: 30,
        not_heard: 2,
        not_judged: 1,
        disputed: 1,
        center: 10,
        in_window: 15,
        outside: 5,
        timed: 30,
        median_ms: Some(-29.987_654_321),
        spread_ms: Some(9.1),
        uncertainty_ms: None,
        p90_abs_ms: Some(51.25),
        constancy_focus: false,
        tendency: Tendency::Early(-29.987_654_321),
    }
}

/// One of every line type (and every variant of the ones that have several), no NaN.
fn all_events() -> Vec<TapeEvent> {
    let heard = OnsetEvent::Heard {
        t: 4800.5,
        at: 5120.0,
    };
    let strum = OnsetEvent::Strum(Strum {
        t: 4800.25,
        spread: 311.5,
        peaks: 4,
        strength: 3.25,
        rise_db: 17.5,
    });
    let note = |event, at_in, judged| OnsetNote {
        event,
        t_in: 4800.25,
        at_in,
        judged,
        epoch: 2,
    };
    let verdict = |attack, timing, dt, fin| {
        JudgeOut::Verdict(PulseVerdict {
            id: 7,
            attack,
            timing,
            dt_ms: dt,
            dir: Direction::NotJudged(Why::GateLocked),
            onset_in: dt.map(|d| 4800.0 + f64::from(d)),
            pos_in_bar: 2,
            fin,
        })
    };
    vec![
        TapeEvent::Take {
            label: "silence".into(),
        },
        TapeEvent::Engine(EngineEvent::Onset(note(heard, Some(5120.0), true))),
        TapeEvent::Engine(EngineEvent::Onset(note(strum, None, false))),
        TapeEvent::Engine(EngineEvent::Mix(MixEvent::Started { tag: 1, at: 96_000 })),
        TapeEvent::Engine(EngineEvent::Mix(MixEvent::Beat {
            tag: 1,
            index: 3,
            at: 96_000 + 36_000,
        })),
        TapeEvent::Engine(EngineEvent::Mix(MixEvent::Late { tag: 2, by: 77 })),
        TapeEvent::Engine(EngineEvent::Mix(MixEvent::Rejected { tag: 9 })),
        TapeEvent::Engine(EngineEvent::Clock(ClockSnapshot::from_parts(parts()))),
        TapeEvent::Engine(EngineEvent::Clock(ClockSnapshot::from_parts(ClockParts {
            base_ns: None,
            quality: ClockQuality::Acquiring,
            ..parts()
        }))),
        TapeEvent::Engine(EngineEvent::Step(ClockStep {
            side: Side::Out,
            epoch: 1,
            frame: 123_456,
            step_s: -0.020_833_333_333_333_332,
            frames: -1000.0,
            cause: StepCause::Notice,
        })),
        TapeEvent::Engine(EngineEvent::Unjudged {
            epoch: 1,
            from_in: 100.5,
            to_in: 9000.25,
        }),
        TapeEvent::Engine(EngineEvent::Backend(BackendEvent::XrunIn)),
        TapeEvent::Engine(EngineEvent::Stopped(BackendEvent::DeviceLost)),
        TapeEvent::Engine(EngineEvent::Reopened { epoch: 4 }),
        TapeEvent::Engine(EngineEvent::Dropped {
            ring: Ring::Pitch,
            n: 12,
        }),
        TapeEvent::Cmd(SessionCmdLog::Soundcheck { force: true }),
        TapeEvent::Cmd(SessionCmdLog::StartRound {
            lead_in_beats: 4,
            take: Some("a \"quoted\"\ttake\n".into()),
        }),
        TapeEvent::Cmd(SessionCmdLog::StartRound {
            lead_in_beats: 0,
            take: None,
        }),
        TapeEvent::Cmd(SessionCmdLog::StopRound),
        TapeEvent::Cmd(SessionCmdLog::SetLevel(Level::Band)),
        TapeEvent::Cmd(SessionCmdLog::Record(false)),
        TapeEvent::Cmd(SessionCmdLog::Rewire),
        TapeEvent::Cmd(SessionCmdLog::Shutdown),
        TapeEvent::Cmd(SessionCmdLog::Dispute),
        TapeEvent::Cmd(SessionCmdLog::Click {
            bpm: 92.5,
            beats: 16,
            accent_every: 4,
        }),
        TapeEvent::Cmd(SessionCmdLog::CancelSoundcheck),
        TapeEvent::Anchor {
            out_frame: 1_234_567,
            chart_json: guitarness_core::timeline::demo_chart().to_json(),
            timing: None,
            judge: None,
        },
        TapeEvent::Anchor {
            out_frame: 7_654_321,
            chart_json: guitarness_core::timeline::demo_chart().to_json(),
            timing: Some(TimingRef::estimate(61.25, 4.5, 12)),
            judge: Some(JudgeConfig {
                close_margin_ms: 77.5,
                ..JudgeConfig::new(44_100.0, Level::Band)
            }),
        },
        TapeEvent::Out(JudgeOut::Flash {
            id: Some(7),
            dt_ms: Some(-12.5),
        }),
        TapeEvent::Out(JudgeOut::Flash {
            id: None,
            dt_ms: None,
        }),
        TapeEvent::Out(verdict(Attack::Heard, Timing::InWindow, Some(-12.5), false)),
        TapeEvent::Out(verdict(Attack::Heard, Timing::Center, Some(3.25), true)),
        TapeEvent::Out(verdict(
            Attack::NotHeard,
            Timing::NotJudged(Why::NoAttack),
            None,
            true,
        )),
        TapeEvent::Out(verdict(
            Attack::NotJudged(Why::Glitch),
            Timing::NotJudged(Why::ClockNotLocked),
            None,
            true,
        )),
        TapeEvent::Out(verdict(Attack::Heard, Timing::Outside, Some(88.0), true)),
        TapeEvent::Out(JudgeOut::Withdrawn {
            id: 7,
            why: Why::Disputed,
        }),
        TapeEvent::Out(JudgeOut::Extra { t_in: 99_999.5 }),
        TapeEvent::Round(stats()),
        TapeEvent::Round(RoundStats {
            median_ms: None,
            spread_ms: None,
            p90_abs_ms: None,
            constancy_focus: true,
            tendency: Tendency::NotClaimed(NoClaim::EstimateOnly),
            ..stats()
        }),
        TapeEvent::Round(RoundStats {
            tendency: Tendency::Late(21.5),
            ..stats()
        }),
        TapeEvent::Round(RoundStats {
            tendency: Tendency::Centered,
            ..stats()
        }),
        TapeEvent::Truth(TruthStrum {
            id: Some(3),
            kind: HitKind::Mute,
            out_frame: 777,
            played: true,
            extra: false,
            muted: true,
            g_in: 1.5,
            arrival_in: 2.5,
            onset_in: Some(3.5),
            dt_ms: Some(-4.5),
        }),
        TapeEvent::Truth(TruthStrum {
            id: None,
            kind: HitKind::Ghost,
            out_frame: 0,
            played: false,
            extra: true,
            muted: false,
            g_in: 0.0,
            arrival_in: 0.0,
            onset_in: None,
            dt_ms: None,
        }),
    ]
}

fn header() -> TapeHeader {
    let mut h = TapeHeader::new(FS, FS);
    h.in_id = "in-\u{e9}\"x".into();
    h.out_id = "out\\x".into();
    h.origin_offset_ns = -1234;
    h.stamp_out = StampSource::Callback;
    h.timing = TimingRef::loopback(87.3, 0.4);
    h.video_offset_ms = -12.25;
    h
}

/// Deterministic white-ish noise in `[-0.5, 0.5]`.
fn noise(n: usize, seed: u32) -> Vec<f32> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        })
        .collect()
}

fn stamp(first: u64, frames: usize) -> Stamp {
    Stamp {
        first,
        frames: frames as u32,
        device_ns: 0,
        callback_ns: 0,
    }
}

fn wait_idle() {
    // The writer drains every 10 ms; 80 ms is several drains.
    std::thread::sleep(Duration::from_millis(80));
}

// ---------------------------------------------------------------------------------------

#[test]
fn every_line_type_round_trips_with_partial_eq() {
    // Through the line codec ...
    for (i, ev) in all_events().iter().enumerate() {
        let line = event_line(i as u64, ev).expect("taped");
        assert!(!line.contains('\n'));
        assert_eq!(
            decode_line(&line),
            Decoded::Event(i as u64, ev.clone()),
            "{line}"
        );
    }
    // ... and through a real tape on disk.
    let tmp = Tmp::new("roundtrip");
    let dir = tmp.path().join("t");
    let w = TapeWriter::create(&dir, header(), None).unwrap();
    for ev in all_events() {
        w.event(ev);
    }
    let sum = w.finish().unwrap();
    assert!(sum.complete && sum.disputed);
    assert_eq!(sum.events, all_events().len() as u64);
    let tape = Tape::open(&dir).unwrap();
    assert!(tape.complete);
    assert_eq!((tape.unknown_lines, tape.bad_lines), (0, 0));
    assert!(!tape.has_audio());
    let want: Vec<(u64, TapeEvent)> = all_events()
        .into_iter()
        .enumerate()
        .map(|(i, e)| (i as u64, e))
        .collect();
    assert_eq!(tape.events, want);
    // The chart came back as the text that went in, and is still a chart.
    let anchor = tape
        .events
        .iter()
        .find_map(|(_, e)| match e {
            TapeEvent::Anchor { chart_json, .. } => Some(chart_json.clone()),
            _ => None,
        })
        .unwrap();
    assert!(guitarness_core::chart::Chart::from_json(&anchor).is_ok());
    assert_eq!(tape.header.takes.len(), 1);
    assert_eq!(
        tape.header.takes[0],
        TakeSpan {
            label: "silence".into(),
            seq_first: 0,
            seq_last: want.len() as u64 - 1
        }
    );
    assert!(tape.header.disputed);
}

#[test]
fn edge_floats_round_trip_and_nan_is_null() {
    let weird = [
        0.0,
        -0.0,
        5e-324,
        -5e-324,
        f64::MIN_POSITIVE,
        2.225_073_858_507_201e-308,
        f64::MAX,
        f64::MIN,
        f64::EPSILON,
        0.1 + 0.2,
        1e15,
        9_999_999_999_999_998.0,
        1e-5,
        9.999_999e-6,
        123_456_789.123_456_79,
        1.797_693_134_862_315_7e308,
        // A value serde_json without float_roundtrip is known to parse one ulp off.
        2.225_073_858_507_201e-308,
        8.941_922_252_618_167e-2,
    ];
    for x in weird {
        let ev = TapeEvent::Out(JudgeOut::Extra { t_in: x });
        let line = event_line(1, &ev).unwrap();
        let Decoded::Event(_, TapeEvent::Out(JudgeOut::Extra { t_in })) = decode_line(&line) else {
            panic!("{line}");
        };
        assert_eq!(t_in.to_bits(), x.to_bits(), "{x:e} via {line}");
    }
    // f32 fields keep their own shortest form.
    for x in [0.0f32, -0.0, f32::MIN_POSITIVE, 1e-45, f32::MAX, 0.1, 17.5] {
        let ev = TapeEvent::Out(JudgeOut::Flash {
            id: Some(1),
            dt_ms: Some(x),
        });
        let line = event_line(1, &ev).unwrap();
        let Decoded::Event(_, TapeEvent::Out(JudgeOut::Flash { dt_ms, .. })) = decode_line(&line)
        else {
            panic!("{line}");
        };
        assert_eq!(dt_ms.unwrap().to_bits(), x.to_bits(), "{x:e}");
    }
    // NaN goes out as null and comes back as NaN; so does an infinity.
    for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let note = OnsetNote {
            event: OnsetEvent::Heard { t: x, at: 1.0 },
            t_in: x,
            at_in: Some(x),
            judged: false,
            epoch: 0,
        };
        let line = event_line(0, &TapeEvent::Engine(EngineEvent::Onset(note))).unwrap();
        assert!(line.contains("\"t_in\":null"), "{line}");
        assert!(!line.contains("NaN") && !line.contains("inf"), "{line}");
        let Decoded::Event(_, TapeEvent::Engine(EngineEvent::Onset(back))) = decode_line(&line)
        else {
            panic!("{line}");
        };
        assert!(back.t_in.is_nan());
        assert_eq!(back.at_in, None, "an Option field reads null as None");
        let OnsetEvent::Heard { t, at } = back.event else {
            panic!()
        };
        assert!(t.is_nan() && at == 1.0);
    }
    // base_ns is decimal text, so an i128 beyond 2^64 and a negative one survive.
    for b in [i128::MAX, i128::MIN, -1, 0, 1 << 100] {
        let ev = TapeEvent::Engine(EngineEvent::Clock(ClockSnapshot::from_parts(ClockParts {
            base_ns: Some(b),
            ..parts()
        })));
        let line = event_line(0, &ev).unwrap();
        assert!(line.contains(&format!("\"base_ns\":\"{b}\"")), "{line}");
        assert_eq!(decode_line(&line), Decoded::Event(0, ev));
    }
}

#[test]
fn header_round_trips_with_profile_and_gates() {
    let mut h = header();
    h.set_gates(&GateTable::builtin());
    let key = guitarness_audio::ProfileKey::new(
        "fake",
        &guitarness_audio::StreamInfo {
            fs_in: FS,
            fs_out: FS,
            ch_in: 1,
            ch_out: 2,
            in_id: "i".into(),
            out_id: "o".into(),
            origin_offset_ns: 0,
        },
    );
    let profile = guitarness_audio::Profile {
        key,
        l_rt_ms: Some(87.300_000_000_000_01),
        mad_ms: Some(0.4),
        tendency_ms: Some(-30.1),
        tendency_mad_ms: Some(8.7),
        combined_ms: None,
        combined_mad_ms: None,
        combined_n: None,
        judgeable: true,
        date: "2026-11-16".into(),
        clock_quality: ClockQuality::Locked,
        drift_ppm: -12.5,
        stamps_in: StampSource::Device,
        stamps_out: StampSource::Device,
    };
    h.set_profile(&profile);
    h.takes.push(TakeSpan {
        label: "chords".into(),
        seq_first: 3,
        seq_last: 9,
    });
    h.disputed = true;
    for timing in [
        TimingRef::loopback(87.3, 0.4),
        TimingRef::estimate(120.5, 6.0, 12),
        TimingRef::none(),
    ] {
        h.timing = timing;
        let text = header_to_text(&h);
        assert!(text.contains(&format!("\"v\": {TAPE_VERSION}")));
        assert_eq!(header_from_text(&text).unwrap(), h);
    }
    let back = header_from_text(&header_to_text(&h)).unwrap();
    assert_eq!(back.profile().unwrap(), profile);
    assert_eq!(back.gates().unwrap(), GateTable::builtin());
}

#[test]
fn wrong_version_and_unknown_lines() {
    let tmp = Tmp::new("version");
    let dir = tmp.path().join("t");
    let w = TapeWriter::create(&dir, header(), None).unwrap();
    w.event(TapeEvent::Cmd(SessionCmdLog::StopRound));
    w.finish().unwrap();

    // A wrong `v` is an error, whichever it is.
    let good = fs::read_to_string(dir.join("tape.json")).unwrap();
    for bad in ["\"v\": 2", "\"v\": 0"] {
        fs::write(
            dir.join("tape.json"),
            good.replace(&format!("\"v\": {TAPE_VERSION}"), bad),
        )
        .unwrap();
        let got = Tape::open(&dir).unwrap_err();
        assert!(
            matches!(got, TapeError::Version(Some(v)) if v != TAPE_VERSION),
            "{got:?}"
        );
    }
    fs::write(dir.join("tape.json"), good.replace("\"v\": 1,", "")).unwrap();
    assert!(matches!(
        Tape::open(&dir).unwrap_err(),
        TapeError::Version(None)
    ));
    fs::write(dir.join("tape.json"), &good).unwrap();

    // Unknown `k`, garbage and a known type missing a field are counted, nothing else.
    let mut events = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    events.push_str("{\"seq\":1,\"k\":\"hologram\",\"x\":1}\n");
    events.push_str("not json at all\n");
    events.push_str("{\"seq\":2,\"k\":\"extra\"}\n"); // no t_in
    events.push_str("{\"seq\":3,\"k\":\"cmd\",\"c\":\"stop_round\"}\n");
    fs::write(dir.join("events.jsonl"), events).unwrap();
    let t = Tape::open(&dir).unwrap();
    assert_eq!((t.unknown_lines, t.bad_lines), (1, 2));
    assert_eq!(t.events.len(), 2);
    assert_eq!(t.events[1].0, 3);
    assert!(
        !t.complete && !t.header.complete,
        "damage clears completeness"
    );

    // A tape whose tape.json is missing is not a tape.
    fs::remove_file(dir.join("tape.json")).unwrap();
    assert!(matches!(Tape::open(&dir).unwrap_err(), TapeError::Io(_)));
}

/// Feeds `src` to a real `CaptureStage` in blocks of `block`, one stamp per block, with a
/// jump of the input frame counter of `jump` frames after `jump_after` blocks. Returns
/// the input frame of each sample.
fn feed(
    st: &mut CaptureStage,
    src: &[f32],
    block: usize,
    jump_after: usize,
    jump: u64,
    pace: Option<Duration>,
) -> Vec<u64> {
    let mut frames = Vec::new();
    let mut first = 0u64;
    for (k, c) in src.chunks(block).enumerate() {
        if k == jump_after {
            first += jump;
        }
        st.process(c, stamp(first, c.len()));
        frames.extend(first..first + c.len() as u64);
        first += c.len() as u64;
        if let Some(p) = pace {
            std::thread::sleep(p);
        }
    }
    frames
}

#[test]
fn audio_is_bit_exact_through_a_real_capture_stage_with_an_input_jump() {
    let tmp = Tmp::new("audio");
    let dir = tmp.path().join("t");
    let (rings, mut outs) = CaptureRings::new(f64::from(FS));
    let mut st = CaptureStage::new(f64::from(FS), 1, ChannelPick::Index(0), 512, rings);
    let raw = outs.take_raw().unwrap();
    outs.set_raw(true);
    let w = TapeWriter::create(&dir, header(), Some(raw)).unwrap();

    let src = noise(FS as usize, 7); // 1 s
    let frames = feed(
        &mut st,
        &src,
        480,
        40,
        5000,
        Some(Duration::from_micros(300)),
    );
    st.flush();
    wait_idle();
    let sum = w.finish().unwrap();
    assert!(sum.complete, "{sum:?}");
    assert_eq!(sum.mic_frames, src.len() as u64);
    assert_eq!((sum.lost_frames, sum.dropped_raw_blocks), (0, 0));

    let tape = Tape::open(&dir).unwrap();
    assert!(tape.complete && tape.has_audio());
    // Two runs: the jump of the input counter is the only thing that splits them.
    let runs = tape.runs();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0].in_first, 0);
    assert_eq!(runs[0].det_first, 0);
    assert_eq!(runs[0].frames, 40 * 480);
    assert_eq!(runs[1].det_first, 40 * 480);
    assert_eq!(runs[1].in_first, 40 * 480 + 5000);
    assert_eq!(runs[0].frames + runs[1].frames, src.len() as u64);
    assert!(runs.iter().all(|r| !r.lost));

    // Bit for bit, in detector time ...
    let mic = tape.mic().unwrap();
    assert_eq!(mic.len(), src.len());
    assert!(
        mic.iter()
            .zip(&src)
            .all(|(a, b)| a.to_bits() == b.to_bits())
    );
    // ... and as a valid WAV any reader takes.
    let mut rd = hound::WavReader::open(dir.join("mic.wav")).unwrap();
    assert_eq!(rd.spec().sample_rate, FS);
    assert_eq!(rd.spec().channels, 1);
    let wav: Vec<f32> = rd.samples::<f32>().map(Result::unwrap).collect();
    assert_eq!(wav, src);

    // Detector time maps to input frames through the runs.
    assert_eq!(tape.in_of_det(10.5), Some(10.5));
    assert_eq!(
        tape.in_of_det(f64::from(40 * 480) + 1.25),
        Some(f64::from(40 * 480 + 5000) + 1.25)
    );

    // A window inside one run.
    let a = tape.audio(100, 200).unwrap();
    assert_eq!(a, src[100..200]);
    // A window around the jump: samples on both sides, zeros where the input counter
    // skipped, positioned by input frame.
    let jump_in = 40 * 480u64;
    let (from, to) = (jump_in - 30, jump_in + 5000 + 50);
    let w = tape.audio(from, to).unwrap();
    assert_eq!(w.len() as u64, to - from);
    assert_eq!(w[..30], src[40 * 480 - 30..40 * 480]);
    assert!(w[30..30 + 5000].iter().all(|x| *x == 0.0));
    assert_eq!(w[30 + 5000..], src[40 * 480..40 * 480 + 50]);
    let pieces = tape.audio_pieces(from, to).unwrap();
    assert_eq!(pieces.len(), 2);
    assert_eq!((pieces[0].in_first, pieces[0].samples.len()), (from, 30));
    assert_eq!(
        (pieces[1].in_first, pieces[1].samples.len()),
        (jump_in + 5000, 50)
    );
    // Each sample of every window is the sample of its input frame.
    for probe in [0usize, 479, 480, 19_199, 19_200, 30_000, src.len() - 1] {
        let f = frames[probe];
        assert_eq!(tape.audio(f, f + 1).unwrap(), [src[probe]]);
    }
    // Windows outside the recording are zeros; a backwards window is an error.
    assert!(
        tape.audio(10_000_000, 10_000_010)
            .unwrap()
            .iter()
            .all(|x| *x == 0.0)
    );
    assert!(matches!(tape.audio(5, 4).unwrap_err(), TapeError::Range(_)));
}

#[test]
fn a_lost_raw_block_is_zero_filled_flagged_and_makes_the_tape_incomplete() {
    let tmp = Tmp::new("lost");
    let dir = tmp.path().join("t");
    let (rings, mut outs) = CaptureRings::new(f64::from(FS));
    let mut st = CaptureStage::new(f64::from(FS), 1, ChannelPick::Index(0), 512, rings);
    let raw = outs.take_raw().unwrap();
    outs.set_raw(true);

    // Nobody drains: the 4 s ring fills and later blocks are dropped (all or nothing).
    let before = noise(5 * FS as usize, 11);
    feed(&mut st, &before, 480, usize::MAX, 0, None);
    assert!(outs.raw_dropped_blocks() > 0);
    let w = TapeWriter::create(&dir, header(), Some(raw)).unwrap();
    wait_idle();
    // Now the writer is up: more audio arrives, with a hole before it.
    let after = noise(FS as usize / 2, 12);
    for (k, c) in after.chunks(480).enumerate() {
        let first = before.len() as u64 + (k * 480) as u64;
        st.process(c, stamp(first, c.len()));
        std::thread::sleep(Duration::from_micros(300));
    }
    st.flush();
    wait_idle();
    let sum = w.finish().unwrap();
    assert!(!sum.complete);
    assert!(sum.dropped_raw_blocks > 0);
    assert!(sum.lost_frames > 0);

    let tape = Tape::open(&dir).unwrap();
    assert!(!tape.complete, "a tape with a hole stays out of the gates");
    assert!(!tape.header.complete);
    let lost: Vec<_> = tape.runs().iter().filter(|r| r.lost).collect();
    assert_eq!(lost.len(), 1, "{:?}", tape.runs());
    // The WAV is still contiguous in detector time: its length is the whole stream.
    let total = (before.len() + after.len()) as u64;
    assert_eq!(tape.mic().unwrap().len() as u64, total);
    let kept: u64 = tape
        .runs()
        .iter()
        .filter(|r| !r.lost)
        .map(|r| r.frames)
        .sum();
    assert_eq!(kept + lost[0].frames, total);
    // The lost run is zeros and says so; the audio after it is exact.
    let r = lost[0];
    let p = tape
        .audio_pieces(r.in_first, r.in_first + r.frames)
        .unwrap();
    assert_eq!(p.len(), 1, "{p:?}");
    assert_eq!(
        (p[0].in_first, p[0].samples.len() as u64),
        (r.in_first, r.frames)
    );
    assert!(p[0].lost && p[0].samples.iter().all(|s| *s == 0.0));
    let tail = tape
        .audio(
            before.len() as u64,
            before.len() as u64 + after.len() as u64,
        )
        .unwrap();
    assert_eq!(tail, after);
}

#[test]
fn a_tape_that_was_not_closed_opens_incomplete() {
    let tmp = Tmp::new("crash");
    let live = tmp.path().join("live");
    let copy = tmp.path().join("copy");
    let (rings, mut outs) = CaptureRings::new(f64::from(FS));
    let mut st = CaptureStage::new(f64::from(FS), 1, ChannelPick::Index(0), 512, rings);
    let raw = outs.take_raw().unwrap();
    outs.set_raw(true);
    let w = TapeWriter::create(&live, header(), Some(raw)).unwrap();
    let src = noise(FS as usize / 2, 3);
    feed(
        &mut st,
        &src,
        480,
        usize::MAX,
        0,
        Some(Duration::from_micros(200)),
    );
    st.flush();
    w.event(TapeEvent::Take {
        label: "one".into(),
    });
    for ev in all_events().into_iter().skip(1) {
        w.event(ev);
    }
    // After the 0.5 s flush the files hold what was sent so far. "Crash": copy them while
    // the writer is still open, and cut the copies the way a dying process leaves them.
    std::thread::sleep(Duration::from_millis(900));
    fs::create_dir_all(&copy).unwrap();
    for f in ["tape.json", "events.jsonl", "mic.wav", "mic.runs.json"] {
        fs::copy(live.join(f), copy.join(f)).unwrap();
    }
    w.finish().unwrap();

    let events = fs::read(copy.join("events.jsonl")).unwrap();
    let n_lines = events.iter().filter(|b| **b == b'\n').count();
    assert_eq!(n_lines, all_events().len(), "all flushed");
    // Cut the last line in half and the WAV in the middle of a sample.
    let cut = events.len() - 20;
    fs::write(copy.join("events.jsonl"), &events[..cut]).unwrap();
    let wav = fs::read(copy.join("mic.wav")).unwrap();
    fs::write(copy.join("mic.wav"), &wav[..wav.len() - 4001]).unwrap();

    let t = Tape::open(&copy).unwrap();
    assert!(!t.complete && !t.header.complete);
    assert_eq!(t.bad_lines, 1, "the cut line");
    assert_eq!(t.events.len(), n_lines - 1);
    assert_eq!(t.header.counts.events, t.events.len() as u64);
    // Takes and the dispute flag are rebuilt from the lines.
    assert_eq!(t.header.takes.len(), 1);
    assert_eq!(t.header.takes[0].label, "one");
    assert!(t.header.disputed);
    // The audio is the file's length, not what the stale header said.
    assert!(t.has_audio());
    let mic = t.mic().unwrap();
    let data = wav.windows(4).position(|w| w == b"data").unwrap() + 8;
    assert_eq!(mic.len(), (wav.len() - data - 4001) / 4);
    assert_eq!(mic, src[..mic.len()]);
}

#[test]
fn create_never_overwrites_a_tape_and_close_on_drop_finishes_it() {
    let tmp = Tmp::new("create");
    let dir = tmp.path().join("t");
    {
        let w = TapeWriter::create(&dir, header(), None).unwrap();
        w.event(TapeEvent::Cmd(SessionCmdLog::StopRound));
        assert!(!w.failed());
        assert_eq!(w.dir(), dir);
        // dropped without finish
    }
    let t = Tape::open(&dir).unwrap();
    assert!(t.complete, "a dropped writer still closes the tape");
    assert_eq!(t.events.len(), 1);
    let err = TapeWriter::create(&dir, header(), None).err().unwrap();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(Tape::open(&dir).unwrap().events.len(), 1, "left alone");
}

#[test]
fn update_header_and_takes() {
    let tmp = Tmp::new("hdr");
    let dir = tmp.path().join("t");
    let w = TapeWriter::create(&dir, header(), None).unwrap();
    w.event(TapeEvent::Cmd(SessionCmdLog::Soundcheck { force: false }));
    w.event(TapeEvent::Take { label: "a".into() });
    w.event(TapeEvent::Cmd(SessionCmdLog::StopRound));
    w.event(TapeEvent::Take { label: "b".into() });
    w.update_header(|h| h.video_offset_ms = 41.5);
    w.event(TapeEvent::Cmd(SessionCmdLog::StopRound));
    w.event(TapeEvent::Cmd(SessionCmdLog::StopRound));
    w.finish().unwrap();
    let t = Tape::open(&dir).unwrap();
    assert_eq!(t.header.video_offset_ms, 41.5);
    let spans: Vec<_> = t
        .header
        .takes
        .iter()
        .map(|s| (s.label.as_str(), s.seq_first, s.seq_last))
        .collect();
    assert_eq!(spans, [("a", 1, 2), ("b", 3, 5)]);
    assert!(!t.header.disputed);
}

#[test]
fn folder_names_follow_the_plan() {
    assert_eq!(utc_iso(0), "1970-01-01T00:00:00Z");
    assert_eq!(utc_iso(1_700_000_000), "2023-11-14T22:13:20Z");
    assert_eq!(utc_iso(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(
        tape_folder_name(1_700_000_000, 0x00ab),
        "20231114-221320-00ab"
    );
    let tmp = Tmp::new("names");
    let a = new_tape_dir(&tmp.path().join("tapes")).unwrap();
    let b = new_tape_dir(&tmp.path().join("tapes")).unwrap();
    assert_ne!(a, b);
    let name = a.file_name().unwrap().to_str().unwrap();
    assert_eq!(name.len(), "20231114-221320-00ab".len());
    assert_eq!(&name[8..9], "-");
    assert_eq!(&name[15..16], "-");
    assert!(name[16..].chars().all(|c| c.is_ascii_hexdigit()));
    assert!(a.is_dir());
}

fn make_tape(root: &Path, name: &str, events: usize, dispute: bool) -> PathBuf {
    let dir = root.join(name);
    let w = TapeWriter::create(&dir, header(), None).unwrap();
    for i in 0..events {
        w.event(TapeEvent::Out(JudgeOut::Extra {
            t_in: 1000.0 + i as f64,
        }));
    }
    if dispute {
        w.event(TapeEvent::Cmd(SessionCmdLog::Dispute));
        w.event(TapeEvent::Out(JudgeOut::Withdrawn {
            id: 3,
            why: Why::Disputed,
        }));
    }
    w.finish().unwrap();
    dir
}

/// P1-2: an `anchor` line carries its round's timing reference and judge settings; a line
/// written before the fields existed (or with `null`) reads back with `None`.
#[test]
fn anchor_lines_carry_their_round_reference() {
    let chart = guitarness_core::timeline::demo_chart().to_json();
    let old = format!("{{\"seq\":4,\"k\":\"anchor\",\"out_frame\":99,\"chart\":{chart}}}");
    let nulls = format!(
        "{{\"seq\":4,\"k\":\"anchor\",\"out_frame\":99,\"chart\":{chart},\"timing\":null,\"judge\":null}}"
    );
    for line in [old, nulls] {
        match decode_line(&line) {
            Decoded::Event(4, TapeEvent::Anchor { timing, judge, .. }) => {
                assert_eq!((timing, judge), (None, None), "{line}");
            }
            other => panic!("{other:?}"),
        }
    }
    let ev = TapeEvent::Anchor {
        out_frame: 99,
        chart_json: chart,
        timing: Some(TimingRef::loopback(87.3, 0.4)),
        judge: Some(JudgeConfig::new(48_000.0, Level::Apprentice)),
    };
    let line = event_line(4, &ev).unwrap();
    assert!(
        line.contains("\"timing\":{") && line.contains("\"judge\":{"),
        "{line}"
    );
    assert_eq!(decode_line(&line), Decoded::Event(4, ev));
    // A bad reference is a bad line, not a silent `None`.
    let bad = line.replace("\"basis\":\"loopback\"", "\"basis\":\"psychic\"");
    assert_eq!(decode_line(&bad), Decoded::Bad);
}

/// A dispute command alone does not mark a tape; a verdict withdrawn for it does.
#[test]
fn only_a_withdrawn_verdict_marks_a_tape_disputed() {
    let tmp = Tmp::new("disputed");
    let a = tmp.path().join("a");
    let w = TapeWriter::create(&a, header(), None).unwrap();
    w.event(TapeEvent::Cmd(SessionCmdLog::Dispute));
    w.event(TapeEvent::Out(JudgeOut::Withdrawn {
        id: 2,
        why: Why::Glitch,
    }));
    let sum = w.finish().unwrap();
    assert!(!sum.disputed);
    assert!(!Tape::open(&a).unwrap().header.disputed);
    let b = make_tape(tmp.path(), "b", 3, true);
    assert!(Tape::open(&b).unwrap().header.disputed);
}

fn size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().metadata().unwrap().len())
        .sum()
}

#[test]
fn prune_respects_the_cap_and_never_deletes_a_disputed_tape() {
    let tmp = Tmp::new("prune");
    let root = tmp.path().join("tapes");
    fs::create_dir_all(&root).unwrap();
    // Oldest first by name. The second one is disputed.
    let t1 = make_tape(&root, "20260101-000000-0001", 400, false);
    let t2 = make_tape(&root, "20260102-000000-0002", 400, true);
    let t3 = make_tape(&root, "20260103-000000-0003", 400, false);
    let t4 = make_tape(&root, "20260104-000000-0004", 400, false);
    // Not a tape: left alone and not counted.
    let other = root.join("notes");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("x.txt"), vec![0u8; 100_000]).unwrap();
    let s = size(&t1);
    assert!(s > 5000);
    let total = s + size(&t2) + size(&t3) + size(&t4);

    // Room for everything: nothing happens.
    assert_eq!(prune(&root, total).unwrap(), 0);
    assert!(t1.exists() && t2.exists() && t3.exists() && t4.exists());

    // One tape too many: the oldest goes.
    let freed = prune(&root, total - 1).unwrap();
    assert_eq!(freed, s);
    assert!(!t1.exists() && t2.exists() && t3.exists() && t4.exists());

    // Cap of zero: everything that may go, goes; the disputed tape stays and the cap is
    // not met (it is not allowed to be).
    let freed = prune(&root, 0).unwrap();
    assert!(freed > 0);
    assert!(t2.exists() && !t3.exists() && !t4.exists());
    assert!(other.join("x.txt").exists());
    assert_eq!(prune(&root, 0).unwrap(), 0);
    // A missing root is not an error.
    assert_eq!(prune(&tmp.path().join("nope"), 0).unwrap(), 0);
    // The contested tape is still a readable tape.
    assert!(Tape::open(&t2).unwrap().header.disputed);
}

#[test]
fn prune_keeps_live_and_contested_tapes_that_were_not_closed() {
    let tmp = Tmp::new("prune-open");
    let root = tmp.path().join("tapes");
    fs::create_dir_all(&root).unwrap();
    let make_open = |name: &str, dispute: bool| {
        // An unclosed tape: closed tape rewritten as `complete: false`.
        let d = make_tape(&root, name, 50, dispute);
        let text = fs::read_to_string(d.join("tape.json")).unwrap();
        // Forget the header's own flags, as a crash before the next flush would.
        let text = text
            .replace("\"complete\": true", "\"complete\": false")
            .replace("\"disputed\": true", "\"disputed\": false");
        fs::write(d.join("tape.json"), text).unwrap();
        d
    };
    let old = |d: &Path| {
        let f = fs::File::options()
            .write(true)
            .open(d.join("events.jsonl"))
            .unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
    };
    let live = make_open("20260101-000000-0001", false); // fresh mtime: may be live
    let stale = make_open("20260102-000000-0002", false);
    let contested = make_open("20260103-000000-0003", true);
    old(&stale);
    old(&contested);
    prune(&root, 0).unwrap();
    assert!(live.exists(), "written to a moment ago");
    assert!(!stale.exists(), "crashed and old");
    assert!(contested.exists(), "the dispute line is enough");
}

#[test]
fn json_text_is_strict_and_keeps_numbers_as_written() {
    for bad in [
        "",
        "{",
        "[1,]",
        "{\"a\":}",
        "01x",
        "tru",
        "\"abc",
        "{} x",
        "1e999",
        "[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[1]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]",
    ] {
        assert!(Json::parse(bad).is_err(), "{bad}");
    }
    let v = Json::parse(" {\"a\":[1, 2.50, -0, 1e3, \"x\\u00e9\\ud83d\\ude00\\n\"], \"b\":null} ")
        .unwrap();
    assert_eq!(
        v.to_text(),
        "{\"a\":[1,2.50,-0,1e3,\"x\u{e9}\u{1F600}\\n\"],\"b\":null}"
    );
    let ctl = Json::Str("\u{1}\u{1f}".into());
    assert_eq!(ctl.to_text(), "\"\\u0001\\u001f\"");
    assert_eq!(Json::parse(&ctl.to_text()).unwrap(), ctl);
}

/// A closed tape with audio (a real capture stage) and a few events.
fn audio_tape(dir: &Path) {
    let (rings, mut outs) = CaptureRings::new(f64::from(FS));
    let mut st = CaptureStage::new(f64::from(FS), 1, ChannelPick::Index(0), 512, rings);
    let raw = outs.take_raw().unwrap();
    outs.set_raw(true);
    let w = TapeWriter::create(dir, header(), Some(raw)).unwrap();
    for i in 0..5 {
        w.event(TapeEvent::Out(JudgeOut::Extra { t_in: i as f64 }));
    }
    feed(
        &mut st,
        &noise(9600, 5),
        480,
        usize::MAX,
        0,
        Some(Duration::from_micros(200)),
    );
    st.flush();
    wait_idle();
    assert!(w.finish().unwrap().complete);
    assert!(Tape::open(dir).unwrap().complete);
}

#[test]
fn damage_clears_completeness() {
    let tmp = Tmp::new("damage");
    let base = tmp.path().join("base");
    audio_tape(&base);
    let copy = |name: &str| {
        let d = tmp.path().join(name);
        fs::create_dir_all(&d).unwrap();
        for f in ["tape.json", "events.jsonl", "mic.wav", "mic.runs.json"] {
            fs::copy(base.join(f), d.join(f)).unwrap();
        }
        d
    };
    let check = |d: &Path, what: &str| {
        let t = Tape::open(d).unwrap();
        assert!(!t.complete && !t.header.complete, "{what}");
    };
    let d = copy("no-events");
    fs::remove_file(d.join("events.jsonl")).unwrap();
    check(&d, "events.jsonl missing");
    let d = copy("no-audio");
    fs::remove_file(d.join("mic.wav")).unwrap();
    fs::remove_file(d.join("mic.runs.json")).unwrap();
    check(&d, "both audio files missing");
    let d = copy("gap");
    let text = fs::read_to_string(d.join("events.jsonl")).unwrap();
    let kept: Vec<&str> = text
        .lines()
        .enumerate()
        .filter(|(i, _)| *i != 2)
        .map(|(_, l)| l)
        .collect();
    fs::write(d.join("events.jsonl"), kept.join("\n") + "\n").unwrap();
    check(&d, "seq gap and count mismatch");
    let d = copy("tail");
    let text = fs::read_to_string(d.join("events.jsonl")).unwrap();
    fs::write(d.join("events.jsonl"), &text[..text.len() - 10]).unwrap();
    check(&d, "cut last line");
    let d = copy("short-wav");
    let wav = fs::read(d.join("mic.wav")).unwrap();
    fs::write(d.join("mic.wav"), &wav[..wav.len() - 400]).unwrap();
    check(&d, "wav shorter than runs");
    let d = copy("long-wav");
    let mut wav = fs::read(d.join("mic.wav")).unwrap();
    wav.extend_from_slice(&[0u8; 400]);
    fs::write(d.join("mic.wav"), &wav).unwrap();
    check(&d, "wav longer than header");
}

#[test]
fn malformed_runs_do_not_panic_and_are_not_trusted() {
    let tmp = Tmp::new("runs");
    let base = tmp.path().join("base");
    audio_tape(&base);
    let good = fs::read_to_string(base.join("mic.runs.json")).unwrap();
    let run = |det: u64, inf: u64, frames: u64| {
        format!(
            "{{\"v\":1,\"fs_in\":{FS},\"runs\":[{{\"det_first\":{det},\"in_first\":{inf},\"frames\":{frames},\"lost\":false}}]}}"
        )
    };
    let two = |a: (u64, u64, u64), b: (u64, u64, u64)| {
        format!(
            "{{\"v\":1,\"fs_in\":{FS},\"runs\":[{{\"det_first\":{},\"in_first\":{},\"frames\":{},\"lost\":false}},{{\"det_first\":{},\"in_first\":{},\"frames\":{},\"lost\":false}}]}}",
            a.0, a.1, a.2, b.0, b.1, b.2
        )
    };
    let m = u64::MAX;
    for (name, text) in [
        ("overflow-sum", two((0, 0, m), (m, m, m))),
        ("overflow-in", run(0, m, 9600)),
        ("overlap", two((0, 0, 5000), (4000, 4000, 4600))),
        ("hole", two((0, 0, 4800), (5000, 5000, 4800))),
    ] {
        let d = tmp.path().join(name);
        fs::create_dir_all(&d).unwrap();
        for f in ["tape.json", "events.jsonl", "mic.wav"] {
            fs::copy(base.join(f), d.join(f)).unwrap();
        }
        fs::write(d.join("mic.runs.json"), text).unwrap();
        let t = Tape::open(&d).unwrap();
        assert!(!t.complete && !t.header.complete, "{name}");
        assert!(
            t.audio(0, 10).is_err() && t.mic().is_err(),
            "{name}: no trusted audio"
        );
    }
    assert!(good.contains("runs"));
}

#[test]
fn dispute_survives_a_cut_utf8_tail_and_unreadable_events_keep_the_tape() {
    let tmp = Tmp::new("prune-utf8");
    let root = tmp.path().join("tapes");
    fs::create_dir_all(&root).unwrap();
    let d = make_tape(&root, "20260101-000000-0001", 3, false);
    let text = fs::read_to_string(d.join("tape.json")).unwrap();
    fs::write(
        d.join("tape.json"),
        text.replace("\"complete\": true", "\"complete\": false"),
    )
    .unwrap();
    let mut bytes =
        b"{\"seq\":0,\"k\":\"cmd\",\"c\":\"dispute\"}\n{\"seq\":1,\"k\":\"take\",\"label\":\""
            .to_vec();
    bytes.push(0xC3); // half of a two-byte character
    fs::write(d.join("events.jsonl"), bytes).unwrap();
    fs::File::options()
        .write(true)
        .open(d.join("events.jsonl"))
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    prune(&root, 0).unwrap();
    assert!(d.exists(), "the dispute is on the tape");
}

#[test]
fn lost_events_and_a_full_queue_make_the_tape_incomplete() {
    let tmp = Tmp::new("loss");
    for (name, ring, complete) in [
        ("events", Ring::Events, false),
        ("out", Ring::OutStamps, false),
        ("in", Ring::InEvents, false),
        ("pitch", Ring::Pitch, true),
    ] {
        let dir = tmp.path().join(name);
        let w = TapeWriter::create(&dir, header(), None).unwrap();
        w.event(TapeEvent::Engine(EngineEvent::Dropped { ring, n: 1 }));
        let sum = w.finish().unwrap();
        assert_eq!(sum.complete, complete, "{name}");
        assert_eq!(Tape::open(&dir).unwrap().complete, complete, "{name}");
    }
    // A queue of one slot against a tight loop must overflow, and say so.
    let dir = tmp.path().join("flood");
    let w = TapeWriter::create_with_queue(&dir, header(), None, 1).unwrap();
    for i in 0..200_000 {
        w.event(TapeEvent::Out(JudgeOut::Extra { t_in: f64::from(i) }));
    }
    assert!(w.overflowed());
    let sum = w.finish().unwrap();
    assert!(!sum.complete);
    let t = Tape::open(&dir).unwrap();
    assert!(!t.complete && t.events.len() < 200_000);
}

#[test]
fn json_grammar_is_strict() {
    for bad in [
        "01",
        "-01",
        "1.",
        ".5",
        "1e",
        "1e+",
        "+1",
        "-",
        "[00]",
        "\"a\nb\"",
        "\"a\tb\"",
        "\"\\ud800\\u0041\"",
        "\"\\ud800\"",
        "\"\\ud800x\"",
        "\"\\udc00\"",
        "\"\\ud800\\ud800\"",
        "\"\\uzzzz\"",
    ] {
        assert!(Json::parse(bad).is_err(), "{bad:?}");
    }
    for good in [
        "0",
        "-0",
        "0.5",
        "10",
        "-1.5e+3",
        "1E-7",
        "[0,1]",
        "\"\\ud83d\\ude00\"",
    ] {
        assert!(Json::parse(good).is_ok(), "{good:?}");
    }
}
