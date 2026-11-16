//! Rule-based coach (the verifier/learner design plan T1, Q10). Pure functions, wired to nothing.
//!
//! Rules derive 2 or 3 safe moves from the picker's candidates (the closed set `Move`).
//! A model, if one is ever enabled, may only pick among them and reword one in at most 15
//! words (`MoveChooser`); `lint` guards the wording and `resolve` falls back to `RuleCoach`.
//! No library function generates free text: phrases are static templates.
//!
//! Nothing here exposes a due list, an error count or a downward demotion: `Slower` is a
//! loop at the skill's own comfort tempo, and every plan stays within one axis of the
//! skill's comfort (J4).

use crate::mastery::LearnerLedger;
use crate::schedule::{axis_change, baseline};
use crate::types::{Axis, BlockKind, BlockPlan, BlockRecord, LearnConfig, Offer};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Slower,
    Faster,
    Easier,
    Loop,
    Next,
    Stop,
    ExplainNextStroke,
}

/// An approved move. `plan` is `None` for `Stop`; `ExplainNextStroke` keeps the chosen plan.
#[derive(Clone, Debug, PartialEq)]
pub struct VettedMove {
    pub mv: Move,
    pub plan: Option<BlockPlan>,
    pub phrase: PhraseId,
}

/// Phrase template id: one template per move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhraseId {
    Slower,
    Faster,
    Easier,
    Loop,
    Next,
    Stop,
    ExplainNextStroke,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Pt,
    En,
}

impl PhraseId {
    pub const ALL: [PhraseId; 7] = [
        PhraseId::Slower,
        PhraseId::Faster,
        PhraseId::Easier,
        PhraseId::Loop,
        PhraseId::Next,
        PhraseId::Stop,
        PhraseId::ExplainNextStroke,
    ];

    pub fn for_move(m: Move) -> Self {
        match m {
            Move::Slower => PhraseId::Slower,
            Move::Faster => PhraseId::Faster,
            Move::Easier => PhraseId::Easier,
            Move::Loop => PhraseId::Loop,
            Move::Next => PhraseId::Next,
            Move::Stop => PhraseId::Stop,
            Move::ExplainNextStroke => PhraseId::ExplainNextStroke,
        }
    }

    /// Static template text; every template passes `lint`.
    pub fn text(self, lang: Lang) -> &'static str {
        match (self, lang) {
            (PhraseId::Slower, Lang::Pt) => "Que tal tocar de novo no seu ritmo confortável?",
            (PhraseId::Slower, Lang::En) => "How about playing again at your comfortable pace?",
            (PhraseId::Faster, Lang::Pt) => {
                "Se estiver com vontade, dá para acelerar um pouquinho."
            }
            (PhraseId::Faster, Lang::En) => "If you feel like it, we can speed up a little.",
            (PhraseId::Easier, Lang::Pt) => "Vamos deixar mais leve e relaxar as mãos.",
            (PhraseId::Easier, Lang::En) => "Let us make it lighter and relax your hands.",
            (PhraseId::Loop, Lang::Pt) => "Vamos repetir mais uma vez, sem pressa.",
            (PhraseId::Loop, Lang::En) => "Let us play it once more, no rush.",
            (PhraseId::Next, Lang::Pt) => "Pronto para a próxima ideia quando você quiser.",
            (PhraseId::Next, Lang::En) => "Ready for the next idea whenever you are.",
            (PhraseId::Stop, Lang::Pt) => "Já foi um bom treino. Podemos parar por aqui.",
            (PhraseId::Stop, Lang::En) => "That was a good practice. We can stop here.",
            (PhraseId::ExplainNextStroke, Lang::Pt) => "Quer ver como é o próximo golpe?",
            (PhraseId::ExplainNextStroke, Lang::En) => {
                "Want to see what the next stroke looks like?"
            }
        }
    }
}

/// Moves for `offer`: the chosen candidate first, then at most one more rule-derived move,
/// then `Stop` (always present). 2 or 3 entries whenever the offer has a candidate; an offer
/// without candidates yields `[Stop]`.
pub fn vetted_moves(offer: &Offer, l: &LearnerLedger, cfg: &LearnConfig) -> Vec<VettedMove> {
    let mut out: Vec<VettedMove> = Vec::new();
    let mk = |mv: Move, plan: Option<BlockPlan>| VettedMove {
        mv,
        plan,
        phrase: PhraseId::for_move(mv),
    };
    if let Some(chosen) = offer.cands.get(offer.chosen) {
        let cp = &chosen.plan;
        let base = baseline(l, &cp.skill, cfg);
        // Slower: loop at the skill's own comfort tempo (never below it, so no demotion).
        let slower =
            (cp.spec.exercise == base.exercise && cp.spec.bpm > base.bpm).then(|| BlockPlan {
                spec: crate::types::BlockSpec {
                    bpm: base.bpm,
                    ..cp.spec
                },
                kind: BlockKind::Familiar,
                skill: cp.skill.clone(),
                show_summary: true,
            });
        // Faster: another candidate that is a tempo-up stretch of its own skill.
        let faster = offer.cands.iter().enumerate().find_map(|(i, c)| {
            let b = baseline(l, &c.plan.skill, cfg);
            let up = matches!(
                axis_change(&b, &c.plan.spec),
                Some((Axis::Tempo, old, new)) if new > old
            );
            (i != offer.chosen && c.plan.kind == BlockKind::Stretch && up).then(|| c.plan.clone())
        });
        // Easier: a Recover candidate, if the picker built one.
        let easier = offer
            .cands
            .iter()
            .enumerate()
            .find(|(i, c)| *i != offer.chosen && c.plan.kind == BlockKind::Recover)
            .map(|(_, c)| c.plan.clone());
        // Next: the next candidate (wrapping) with a different spec.
        let n = offer.cands.len();
        let next = (1..n)
            .map(|d| &offer.cands[(offer.chosen + d) % n].plan)
            .find(|p| p.spec != cp.spec)
            .cloned();

        let order: Vec<Move> = match cp.kind {
            BlockKind::Stretch => vec![Move::Slower, Move::Loop, Move::Next, Move::Easier],
            BlockKind::Recover => vec![Move::Loop, Move::Next, Move::ExplainNextStroke],
            BlockKind::Probe => vec![Move::Next, Move::Loop, Move::ExplainNextStroke],
            _ => vec![
                Move::Loop,
                Move::Faster,
                Move::Next,
                Move::Easier,
                Move::ExplainNextStroke,
            ],
        };
        for mv in order {
            if out.len() == 2 {
                break;
            }
            let plan = match mv {
                Move::Slower => slower.clone(),
                Move::Faster => faster.clone(),
                Move::Easier => easier.clone(),
                Move::Next => next.clone(),
                Move::Loop | Move::ExplainNextStroke => Some(cp.clone()),
                Move::Stop => None,
            };
            if let Some(p) = plan {
                out.push(mk(mv, Some(p)));
            }
        }
    }
    out.push(mk(Move::Stop, None));
    out
}

/// Activity summary for the coach. Deliberately has no error, miss or correctness field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Digest {
    pub blocks: u32,
    pub strums: u32,
    pub minutes: u32,
    pub skills_touched: u32,
}

impl Digest {
    pub fn from_blocks(blocks: &[BlockRecord]) -> Self {
        let mut skills: Vec<&str> = Vec::new();
        for b in blocks {
            for e in &b.evidence {
                if !skills.contains(&e.skill.0.as_str()) {
                    skills.push(e.skill.0.as_str());
                }
            }
        }
        let ms: u64 = blocks.iter().map(|b| u64::from(b.dur_ms)).sum();
        Self {
            blocks: blocks.len() as u32,
            strums: blocks.iter().map(|b| b.strums).sum(),
            minutes: (ms / 60_000) as u32,
            skills_touched: skills.len() as u32,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "blocks": self.blocks,
            "strums": self.strums,
            "minutes": self.minutes,
            "skills_touched": self.skills_touched,
        })
    }
}

/// Words a coach line may never contain (compared lowercase, whole word).
const BANNED: [&str; 16] = [
    "errado", "errada", "erro", "erros", "wrong", "error", "errors", "mistake", "mas", "but",
    "porém", "porem", "however", "fail", "failed", "falhou",
];

pub const MAX_WORDS: usize = 15;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LintError {
    Empty,
    Digit,
    Banned(String),
    TooLong,
}

/// Accepts only short, number-free, non-judging text.
pub fn lint(s: &str) -> Result<(), LintError> {
    if s.trim().is_empty() {
        return Err(LintError::Empty);
    }
    if s.chars().any(|c| c.is_numeric()) {
        return Err(LintError::Digit);
    }
    for w in s.split(|c: char| !c.is_alphabetic()) {
        let w = w.to_lowercase();
        if BANNED.contains(&w.as_str()) {
            return Err(LintError::Banned(w));
        }
    }
    if s.split_whitespace().count() > MAX_WORDS {
        return Err(LintError::TooLong);
    }
    Ok(())
}

/// What a chooser returns: an index into the vetted moves and optional reworded text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pick {
    pub index: usize,
    pub text: Option<String>,
}

/// Choice by a model (or anything else). Only the trait exists; nothing here calls a model.
pub trait MoveChooser {
    fn choose(&self, digest: &Digest, moves: &[VettedMove]) -> Option<Pick>;
}

/// Fallback: first move, template phrase.
#[derive(Clone, Copy, Debug, Default)]
pub struct RuleCoach;

impl MoveChooser for RuleCoach {
    fn choose(&self, _digest: &Digest, moves: &[VettedMove]) -> Option<Pick> {
        (!moves.is_empty()).then_some(Pick {
            index: 0,
            text: None,
        })
    }
}

/// Final coach line: a vetted move and either a template id or a linted free text.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub mv: VettedMove,
    pub text: Option<String>,
}

/// Apply `chooser`; invalid index or text failing `lint` falls back to the rule choice
/// (first move, template). `None` only when `moves` is empty.
pub fn resolve(chooser: &dyn MoveChooser, digest: &Digest, moves: &[VettedMove]) -> Option<Line> {
    let rule = |i: usize| {
        moves.get(i).map(|m| Line {
            mv: m.clone(),
            text: None,
        })
    };
    match chooser.choose(digest, moves) {
        Some(Pick { index, text }) if index < moves.len() => match text {
            Some(t) if lint(&t).is_ok() => Some(Line {
                mv: moves[index].clone(),
                text: Some(t),
            }),
            Some(_) => rule(0),
            None => rule(index),
        },
        _ => rule(0),
    }
}
