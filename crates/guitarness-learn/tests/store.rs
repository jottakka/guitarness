//! N4 tests (1), (2), (3), (6): line round trip, Guest touches no file, truncated file and
//! unknown `k`, `profile.json` shape.

use std::fs;
use std::path::PathBuf;

use guitarness_core::gates::GateState;
use guitarness_core::pulse::Exercise;
use guitarness_core::verdict::{VerdictType, Why};
use guitarness_learn::display::DisplayLedger;
use guitarness_learn::store::{
    Decoded, Store, create_local, decode_line, encode_line, local_id_from_seed, open_profile,
    profile_dir,
};
use guitarness_learn::types::{
    Axis, BlockKind, BlockPlan, BlockRecord, BlockSpec, Candidate, Choice, Evidence, LogLine,
    Offer, Outcome, ProfileId, Provenance, ShadowLine, SkillKey, ZeroWhy,
};

struct Tmp(PathBuf);
impl Tmp {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("guitarness-n4-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn key(s: &str) -> SkillKey {
    SkillKey(s.to_string())
}

fn spec(bpm: u16) -> BlockSpec {
    BlockSpec {
        exercise: Exercise::OnClickQuarters,
        bpm,
        bars: 4,
    }
}

fn ev(id: u32, outcome: Outcome, vtype: VerdictType, gate: GateState, dt: Option<f32>) -> Evidence {
    Evidence {
        id,
        skill: key("pulse:eighth-up"),
        vtype,
        outcome,
        gate,
        dt_ms: dt,
    }
}

fn edge_block() -> BlockRecord {
    let whys = [
        Why::NoCalibration,
        Why::ClockNotLocked,
        Why::Glitch,
        Why::GateLocked,
        Why::Disputed,
        Why::Ghost,
        Why::NoAttack,
    ];
    let mut evidence = vec![
        ev(
            0,
            Outcome::Success,
            VerdictType::Timing,
            GateState::Open,
            Some(-12.5),
        ),
        ev(
            1,
            Outcome::Failure,
            VerdictType::Timing,
            GateState::Provisional,
            Some(f32::MAX),
        ),
        ev(
            2,
            Outcome::Zero(ZeroWhy::NotHeard),
            VerdictType::Attack,
            GateState::Open,
            None,
        ),
        ev(
            3,
            Outcome::Zero(ZeroWhy::Disputed),
            VerdictType::Attack,
            GateState::Locked,
            Some(0.1),
        ),
        ev(
            4,
            Outcome::Success,
            VerdictType::ChordIdentity,
            GateState::Open,
            Some(f32::MIN_POSITIVE),
        ),
    ];
    for (i, w) in whys.iter().enumerate() {
        evidence.push(ev(
            10 + i as u32,
            Outcome::Zero(ZeroWhy::NotJudged(*w)),
            VerdictType::Timing,
            GateState::Open,
            None,
        ));
    }
    BlockRecord {
        ts_ms: i64::MIN,
        dur_ms: u32::MAX,
        session: u32::MAX,
        kind: BlockKind::Stretch,
        spec: BlockSpec {
            exercise: Exercise::OnClickEighths,
            bpm: u16::MAX,
            bars: u8::MAX,
        },
        after_gap: true,
        strums: u32::MAX,
        evidence,
        prov: Provenance {
            judge_v: "judge \"v1\"\n\u{e9}\\".into(),
            basis: "loopback".into(),
            gate: GateState::Provisional,
            tape: Some("tape/\u{1F3B8}.json".into()),
        },
    }
}

fn cand(kind: BlockKind, p: f64, p_hat: Option<f64>, summary: bool) -> Candidate {
    Candidate {
        plan: BlockPlan {
            spec: spec(63),
            kind,
            skill: key("pulse:quarter"),
            show_summary: summary,
        },
        p,
        p_hat,
    }
}

fn edge_floats() -> Vec<f64> {
    vec![
        0.0,
        -0.0,
        0.1 + 0.2,
        1.0 / 3.0,
        f64::MIN_POSITIVE,
        5e-324,
        f64::MAX,
        f64::MIN,
        f64::EPSILON,
        1.0 - f64::EPSILON / 2.0,
        0.999_999_999_999_999_9,
        1e-7,
        123_456_789.123_456_79,
        f64::from_bits(0x000f_ffff_ffff_ffff),
        f64::from_bits(2),
    ]
}

fn all_lines() -> Vec<LogLine> {
    let mut v = vec![LogLine::Block(edge_block())];
    let mut b2 = edge_block();
    b2.ts_ms = i64::MAX;
    b2.evidence.clear();
    b2.prov.tape = None;
    v.push(LogLine::Block(b2));
    v.push(LogLine::Dispute {
        block_ts: -5,
        id: u32::MAX,
    });
    let floats = edge_floats();
    let cands: Vec<Candidate> = floats
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            cand(
                [
                    BlockKind::Probe,
                    BlockKind::Familiar,
                    BlockKind::Recover,
                    BlockKind::Review,
                    BlockKind::Stretch,
                    BlockKind::Free,
                ][i % 6],
                p,
                if i % 2 == 0 { Some(p) } else { None },
                i % 3 == 0,
            )
        })
        .collect();
    v.push(LogLine::Offer(Offer {
        cands: cands.clone(),
        chosen: 3,
        change: Some((Axis::Tempo, 60, 63)),
        seed: u64::MAX,
    }));
    v.push(LogLine::Offer(Offer {
        cands: cands[..2].to_vec(),
        chosen: 0,
        change: Some((Axis::Rhythm, 0, 1)),
        seed: 0,
    }));
    v.push(LogLine::Offer(Offer {
        cands: vec![],
        chosen: 0,
        change: None,
        seed: 1,
    }));
    for c in [
        Choice::Accepted,
        Choice::SomethingElse,
        Choice::ProbeSkipped,
    ] {
        v.push(LogLine::Choice(c));
    }
    for (i, &p) in [0.0f32, 0.1, f32::MIN_POSITIVE, f32::MAX, 1.0 / 3.0]
        .iter()
        .enumerate()
    {
        v.push(LogLine::Shadow(ShadowLine {
            name: "none".into(),
            skill: key("pulse:quarter"),
            p,
            due_ms: if i % 2 == 0 { Some(i64::MIN) } else { None },
        }));
    }
    v
}

/// Bit-level equality for floats (so 0.0 vs -0.0 and ulp errors are caught), `PartialEq` otherwise.
fn assert_bits_equal(a: &LogLine, b: &LogLine) {
    assert_eq!(a, b);
    match (a, b) {
        (LogLine::Offer(x), LogLine::Offer(y)) => {
            for (c, d) in x.cands.iter().zip(&y.cands) {
                assert_eq!(c.p.to_bits(), d.p.to_bits(), "p bits");
                assert_eq!(c.p_hat.map(f64::to_bits), d.p_hat.map(f64::to_bits));
            }
        }
        (LogLine::Shadow(x), LogLine::Shadow(y)) => assert_eq!(x.p.to_bits(), y.p.to_bits()),
        (LogLine::Block(x), LogLine::Block(y)) => {
            for (e, f) in x.evidence.iter().zip(&y.evidence) {
                assert_eq!(e.dt_ms.map(f32::to_bits), f.dt_ms.map(f32::to_bits));
            }
        }
        _ => {}
    }
}

// (1) Round trip of every line kind, edge floats included, in memory and through a file.
#[test]
fn every_line_kind_round_trips() {
    let lines = all_lines();
    for l in &lines {
        let s = encode_line(l);
        assert!(!s.contains('\n'), "one line per record");
        let Decoded::Line(back) = decode_line(&s) else {
            panic!("did not decode: {s}");
        };
        assert_bits_equal(l, &back);
    }
    let t = Tmp::new("roundtrip");
    let mut st = Store::open(&t.0).unwrap();
    for l in &lines {
        st.append(l).unwrap();
    }
    drop(st);
    let st = Store::open(&t.0).unwrap();
    assert_eq!(st.lines().len(), lines.len());
    assert_eq!(
        (st.unknown_lines(), st.bad_lines(), st.truncated_bytes()),
        (0, 0, 0)
    );
    for (a, b) in lines.iter().zip(st.lines()) {
        assert_bits_equal(a, b);
    }
}

// The line kinds carry the `k` field of plan section 3.
#[test]
fn k_field_names_the_record() {
    let want = [
        "block", "block", "dispute", "offer", "offer", "offer", "choice", "choice", "choice",
    ];
    for (l, w) in all_lines().iter().zip(want) {
        let v: serde_json::Value = serde_json::from_str(&encode_line(l)).unwrap();
        assert_eq!(v["k"], w);
    }
}

// (2) Guest: no filesystem call. An empty temp dir stays empty.
#[test]
fn guest_never_touches_the_filesystem() {
    let t = Tmp::new("guest");
    let mut st = open_profile(&t.0, &ProfileId::Guest).unwrap();
    assert!(!st.is_persistent());
    for l in all_lines() {
        st.append(&l).unwrap();
    }
    st.save_display(&DisplayLedger::new()).unwrap();
    assert!(st.load_display().is_none());
    assert_eq!(st.lines().len(), all_lines().len());
    assert!(profile_dir(&t.0, &ProfileId::Guest).is_none());
    let mut m = Store::memory();
    m.append(&LogLine::Choice(Choice::Accepted)).unwrap();
    assert_eq!(
        fs::read_dir(&t.0).unwrap().count(),
        0,
        "temp dir must stay empty"
    );
    // The parent of the temp dir is not required to be clean, but Guest never created `profiles`.
    assert!(!t.0.join("profiles").exists());
}

// (3) A cut file opens up to the last whole line; unknown `k` is counted, not fatal.
#[test]
fn truncated_file_and_unknown_k() {
    let t = Tmp::new("trunc");
    let lines = [
        LogLine::Choice(Choice::Accepted),
        LogLine::Dispute { block_ts: 7, id: 2 },
        LogLine::Choice(Choice::SomethingElse),
    ];
    let mut text = String::new();
    text.push_str(&encode_line(&lines[0]));
    text.push('\n');
    text.push_str("{\"k\":\"future_kind\",\"x\":1}\n");
    text.push_str(&encode_line(&lines[1]));
    text.push('\n');
    text.push_str("{\"k\":\"future_kind\"}\n");
    text.push_str("not json at all\n");
    text.push_str(&encode_line(&lines[2]));
    text.push('\n');
    let whole_len = text.len();
    let cut = encode_line(&LogLine::Block(edge_block()));
    text.push_str(&cut[..cut.len() / 2]);
    fs::write(t.0.join("learn.jsonl"), &text).unwrap();

    let mut st = Store::open(&t.0).unwrap();
    assert_eq!(st.lines(), &lines);
    assert_eq!(st.unknown_lines(), 2);
    assert_eq!(st.bad_lines(), 1);
    assert_eq!(st.truncated_bytes(), cut.len() / 2);
    assert_eq!(
        fs::metadata(t.0.join("learn.jsonl")).unwrap().len() as usize,
        whole_len
    );

    // The next append starts on a clean line; a reopen sees everything.
    st.append(&LogLine::Dispute { block_ts: 9, id: 9 }).unwrap();
    drop(st);
    let st = Store::open(&t.0).unwrap();
    assert_eq!(st.lines().len(), 4);
    assert_eq!(st.lines()[3], LogLine::Dispute { block_ts: 9, id: 9 });
    assert_eq!(
        (st.unknown_lines(), st.bad_lines(), st.truncated_bytes()),
        (2, 1, 0)
    );

    // A cut inside the first line leaves an empty log.
    fs::write(t.0.join("learn.jsonl"), &cut[..10]).unwrap();
    let st = Store::open(&t.0).unwrap();
    assert!(st.lines().is_empty());
}

// (6) profile.json has exactly v, id, created_ms; the id comes from the caller's seed.
#[test]
fn profile_json_has_exactly_three_fields() {
    let t = Tmp::new("profile");
    let id = create_local(&t.0, 42, 1_700_000_000_123).unwrap();
    assert_eq!(id, local_id_from_seed(42));
    let ProfileId::Local(s) = &id else { panic!() };
    assert_eq!(s.len(), 16);
    assert!(
        s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    let dir = profile_dir(&t.0, &id).unwrap();
    assert_eq!(dir, t.0.join("profiles").join(s));
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("profile.json")).unwrap()).unwrap();
    let o = v.as_object().unwrap();
    let mut names: Vec<&str> = o.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["created_ms", "id", "v"]);
    assert_eq!(o["v"], 1);
    assert_eq!(o["id"], s.as_str());
    assert_eq!(o["created_ms"], 1_700_000_000_123i64);
    // Same seed again: refused, nothing overwritten. Different seeds give different ids.
    assert!(create_local(&t.0, 42, 5).is_err());
    assert_ne!(local_id_from_seed(1), local_id_from_seed(2));
    // Malformed ids never become paths.
    assert!(profile_dir(&t.0, &ProfileId::Local("../../etc".into())).is_none());
    assert!(open_profile(&t.0, &ProfileId::Local("..".into())).is_err());
    // A Local store persists lines and display.json under the profile folder.
    let mut st = open_profile(&t.0, &id).unwrap();
    assert!(st.is_persistent());
    st.append(&LogLine::Choice(Choice::Accepted)).unwrap();
    st.save_display(&DisplayLedger::new()).unwrap();
    assert!(st.load_display().is_some());
    assert!(dir.join("learn.jsonl").is_file());
    assert!(dir.join("display.json").is_file());
}
