//! T1 tests: vetted moves respect J4, lint, Digest has no error-like field, rule fallback.

use guitarness_learn::coach::{
    Digest, Lang, LintError, Move, MoveChooser, PhraseId, Pick, RuleCoach, VettedMove, lint,
    resolve, vetted_moves,
};
use guitarness_learn::mastery::{HOUR_MS, LearnerLedger};
use guitarness_learn::picker::next_offer;
use guitarness_learn::types::{
    BlockKind, BlockRecord, BlockSpec, Evidence, LearnConfig, Outcome, Provenance, SessionCtx,
    SkillKey,
};
use tonica_core::gates::GateState;
use tonica_core::pulse::Exercise;
use tonica_core::verdict::VerdictType;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn cfg() -> LearnConfig {
    LearnConfig::default()
}

fn block(ts: i64, ex: Exercise, bpm: u16, s: u32, f: u32) -> BlockRecord {
    let skill = match ex {
        Exercise::OnClickQuarters => "pulse:quarter",
        Exercise::OnClickEighths => "pulse:eighth-down",
    };
    let evidence = (0..s + f)
        .map(|i| Evidence {
            id: i,
            skill: SkillKey(skill.into()),
            vtype: VerdictType::Timing,
            outcome: if i < s {
                Outcome::Success
            } else {
                Outcome::Failure
            },
            gate: GateState::Open,
            dt_ms: None,
        })
        .collect();
    BlockRecord {
        ts_ms: ts,
        dur_ms: 45_000,
        session: 1,
        kind: BlockKind::Familiar,
        spec: BlockSpec {
            exercise: ex,
            bpm,
            bars: 4,
        },
        after_gap: false,
        strums: s + f,
        evidence,
        prov: Provenance {
            judge_v: "t".into(),
            basis: "estimate".into(),
            gate: GateState::Open,
            tape: None,
        },
    }
}

/// Baseline of a skill as J4 defines it: comfort, or the starter in the skill's exercise.
fn base_of(l: &LearnerLedger, skill: &str) -> BlockSpec {
    if let Some(c) = l
        .skills
        .get(&SkillKey(skill.into()))
        .and_then(|s| s.comfort)
    {
        return c;
    }
    let ex = if skill == "pulse:quarter" {
        Exercise::OnClickQuarters
    } else {
        Exercise::OnClickEighths
    };
    BlockSpec {
        exercise: ex,
        ..cfg().starter
    }
}

fn axes(a: &BlockSpec, b: &BlockSpec) -> u32 {
    u32::from(a.exercise != b.exercise) + u32::from(a.bpm != b.bpm)
}

#[test]
fn every_vetted_move_respects_j4() {
    let c = cfg();
    let mut rng = Rng(7);
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..3000 {
        let mut l = LearnerLedger::new(&c);
        let mut t = 1_000 * HOUR_MS;
        for _ in 0..rng.below(7) {
            let ex = if rng.below(3) == 0 {
                Exercise::OnClickEighths
            } else {
                Exercise::OnClickQuarters
            };
            let bpm = 50 + rng.below(8) as u16 * 10;
            let (s, f) = match rng.below(3) {
                0 => (10, 0),
                1 => (7, 3),
                _ => (0, 10),
            };
            l.apply(&block(t, ex, bpm, s, f), &c);
            t += (rng.below(80) as i64) * HOUR_MS;
        }
        let ctx = SessionCtx {
            now_ms: t + rng.below(100) as i64 * HOUR_MS,
            blocks_this_session: rng.below(4) as u32,
            probe_done: rng.below(2) == 0,
            offer_no: rng.below(50),
        };
        let offer = next_offer(&l, &ctx, &c);
        let moves = vetted_moves(&offer, &l, &c);
        assert!((2..=3).contains(&moves.len()), "{moves:?}");
        assert_eq!(moves.last().unwrap().mv, Move::Stop);
        assert!(moves.last().unwrap().plan.is_none());
        let mut ms: Vec<Move> = moves.iter().map(|m| m.mv).collect();
        ms.dedup();
        assert_eq!(ms.len(), moves.len());
        for m in &moves {
            seen.insert(format!("{:?}", m.mv));
            if let Some(p) = &m.plan {
                let b = base_of(&l, &p.skill.0);
                assert!(axes(&b, &p.spec) <= 1, "{:?} {b:?} {:?}", m.mv, p.spec);
                if m.mv == Move::Slower {
                    assert_eq!(p.spec.bpm, b.bpm, "slower returns to comfort tempo");
                    assert_eq!(p.spec.exercise, b.exercise);
                }
            }
        }
    }
    for m in ["Loop", "Next", "Stop", "Slower"] {
        assert!(seen.contains(m), "move {m} never produced: {seen:?}");
    }
}

#[test]
fn empty_offer_still_allows_stop() {
    let c = cfg();
    let l = LearnerLedger::new(&c);
    let mut o = next_offer(
        &l,
        &SessionCtx {
            now_ms: 0,
            blocks_this_session: 0,
            probe_done: true,
            offer_no: 0,
        },
        &c,
    );
    o.cands.clear();
    let m = vetted_moves(&o, &l, &c);
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].mv, Move::Stop);
}

#[test]
fn lint_phrases() {
    let ok = [
        "Vamos tocar mais uma vez, com calma.",
        "Bom treino hoje, continue assim.",
        "Que tal repetir o mesmo trecho agora?",
        "Respire e toque devagar.",
        "Let us play it once more, no rush.",
        "Nice steady playing today.",
        "Ready for the next idea whenever you are?",
        "Relax your wrist and strum slowly.",
        "Mais um passo e terminamos por hoje.",
        "Stay with the pulse, you are doing well.",
    ];
    for p in ok {
        assert_eq!(lint(p), Ok(()), "{p}");
    }
    let bad: [(&str, LintError); 10] = [
        ("Você acertou 3 de 4 golpes.", LintError::Digit),
        ("Toque a 80 BPM agora.", LintError::Digit),
        (
            "Isso está errado, tente de novo.",
            LintError::Banned("errado".into()),
        ),
        (
            "That was wrong, try again.",
            LintError::Banned("wrong".into()),
        ),
        (
            "Bom, mas dá para melhorar.",
            LintError::Banned("mas".into()),
        ),
        (
            "Good effort but not quite.",
            LintError::Banned("but".into()),
        ),
        (
            "Error on the third stroke.",
            LintError::Banned("error".into()),
        ),
        ("", LintError::Empty),
        (
            "Esta frase tem muitas palavras para um treinador que deve ser breve e gentil e calmo sempre",
            LintError::TooLong,
        ),
        (
            "This line keeps going on and on with far too many words for a short calm coach hint",
            LintError::TooLong,
        ),
    ];
    for (p, e) in bad {
        assert_eq!(lint(p), Err(e), "{p}");
    }
    // Whole-word match: "mais" and "button" are fine.
    assert_eq!(lint("Mais um botão, button ok."), Ok(()));
    // Exactly 15 words passes, 16 fails.
    let fifteen =
        "um dois tres quatro cinco seis sete oito nove dez onze doze treze catorze quinze";
    assert_eq!(lint(fifteen), Ok(()));
    assert_eq!(
        lint(&format!("{fifteen} dezesseis")),
        Err(LintError::TooLong)
    );
}

#[test]
fn every_template_passes_lint() {
    for id in PhraseId::ALL {
        for lang in [Lang::Pt, Lang::En] {
            assert_eq!(lint(id.text(lang)), Ok(()), "{id:?} {lang:?}");
        }
    }
}

#[test]
fn digest_serializes_no_error_like_field() {
    let b = [block(0, Exercise::OnClickQuarters, 60, 3, 7)];
    let d = Digest::from_blocks(&b);
    assert_eq!((d.blocks, d.strums, d.minutes), (1, 10, 0));
    let text = d.to_json().to_string().to_lowercase();
    for w in ["wrong", "miss", "error", "fail"] {
        assert!(!text.contains(w), "{text}");
    }
    // Failures change nothing in the digest.
    let g = Digest::from_blocks(&[block(0, Exercise::OnClickQuarters, 60, 10, 0)]);
    assert_eq!(d, g);
}

struct Model(Option<Pick>);
impl MoveChooser for Model {
    fn choose(&self, _: &Digest, _: &[VettedMove]) -> Option<Pick> {
        self.0.clone()
    }
}

#[test]
fn rule_coach_and_fallbacks() {
    let c = cfg();
    let l = LearnerLedger::new(&c);
    let ctx = SessionCtx {
        now_ms: 0,
        blocks_this_session: 0,
        probe_done: true,
        offer_no: 0,
    };
    let moves = vetted_moves(&next_offer(&l, &ctx, &c), &l, &c);
    let d = Digest::default();
    let line = resolve(&RuleCoach, &d, &moves).unwrap();
    assert_eq!(line.mv, moves[0]);
    assert_eq!(line.text, None);
    // Model picks index 1 with a clean text.
    let ok = Model(Some(Pick {
        index: 1,
        text: Some("Vamos com calma.".into()),
    }));
    let l1 = resolve(&ok, &d, &moves).unwrap();
    assert_eq!(
        (l1.mv.clone(), l1.text),
        (moves[1].clone(), Some("Vamos com calma.".into()))
    );
    // Bad text, bad index, no answer: all fall back to the first move with a template.
    for m in [
        Model(Some(Pick {
            index: 1,
            text: Some("Wrong, but ok 3".into()),
        })),
        Model(Some(Pick {
            index: 99,
            text: None,
        })),
        Model(None),
    ] {
        let f = resolve(&m, &d, &moves).unwrap();
        assert_eq!((f.mv, f.text), (moves[0].clone(), None));
    }
    assert!(resolve(&RuleCoach, &d, &[]).is_none());
}
