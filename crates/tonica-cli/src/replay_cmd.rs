//! `tonica replay`: run tapes through today's judge again (Pulso plan L1).
//!
//! Level `Events` feeds the recorded engine events to the judge; `--audio` also runs the
//! recorded microphone through the onset detector. The default text is short, in Brazilian
//! Portuguese and has no numbers; `--detalhes` adds the counts and the differences.
//! Exit codes: 0 no difference, 30 a difference or a failed gate, 20 a tape that cannot be
//! read or replayed, 2 bad command line.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tonica_audio::{EngineEvent, MixEvent};
use tonica_core::gates::{GateCheck, GateState, GateTable};
use tonica_core::verdict::JudgeOut;
use tonica_session::format::{SessionCmdLog, TapeEvent};
use tonica_session::reader::Tape;
use tonica_session::replay::{Diff, ReplayLevel, ReplayResult, evidence, replay};

const HELP: &str = "\
tonica replay: judge recorded tapes again with today's code

USAGE
  tonica replay TAPE... [--audio] [--report] [--gates FILE] [--render-out WAV]
                [--disputes] [--export-dispute N WAV] [--json] [--detalhes]

  TAPE is a tape folder (it holds tape.json) or a folder of tape folders.

OPTIONS
  --audio             also run the recorded microphone through the onset detector
  --report            print the confusion report (counts, rates, gate checks)
  --gates FILE        use FILE as the gate table when it exists, then write the table the
                      evidence justifies back to FILE (implies --report)
  --render-out WAV    write the click schedule of one tape as a WAV (the tape stores the
                      schedule of the output, not the sound)
  --disputes          list the verdicts the player contested
  --export-dispute N WAV
                      write the microphone, 1 s before and after the N-th contested verdict
                      (N counts from 1, as --disputes lists them), to WAV
  --json              machine-readable report on stdout (no player text)
  --detalhes          also print counts and every difference

EXIT CODES
   0  no difference (and no gate failed)
  30  a difference, or a gate that is not locked fails its rule
  20  a tape cannot be read or replayed, or a file cannot be written
   2  bad command line
";

/// Half-width of the window `--export-dispute` writes, seconds.
const DISPUTE_HALF_S: f64 = 1.0;

struct Opts {
    tapes: Vec<PathBuf>,
    audio: bool,
    report: bool,
    gates: Option<PathBuf>,
    render_out: Option<PathBuf>,
    disputes: bool,
    export: Option<(usize, PathBuf)>,
    json: bool,
    details: bool,
}

impl Opts {
    fn parse(args: &[String]) -> Result<Opts, String> {
        let mut o = Opts {
            tapes: Vec::new(),
            audio: false,
            report: false,
            gates: None,
            render_out: None,
            disputes: false,
            export: None,
            json: false,
            details: false,
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut value = |name: &str| {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            match a.as_str() {
                "--audio" => o.audio = true,
                "--report" => o.report = true,
                "--disputes" => o.disputes = true,
                "--json" => o.json = true,
                "--detalhes" => o.details = true,
                "--gates" => o.gates = Some(PathBuf::from(value("--gates")?)),
                "--render-out" => o.render_out = Some(PathBuf::from(value("--render-out")?)),
                "--export-dispute" => {
                    let n: usize = value("--export-dispute")?
                        .parse()
                        .map_err(|_| "--export-dispute N: N is a number from 1".to_string())?;
                    if n == 0 {
                        return Err("--export-dispute N: N counts from 1".into());
                    }
                    let wav = value("--export-dispute")?;
                    o.export = Some((n, PathBuf::from(wav)));
                }
                s if s.starts_with("--") => return Err(format!("unknown option '{s}'")),
                s => o.tapes.push(PathBuf::from(s)),
            }
        }
        if o.tapes.is_empty() {
            return Err("give at least one tape folder".into());
        }
        if o.gates.is_some() {
            o.report = true;
        }
        if o.render_out.is_some() && o.tapes.len() != 1 {
            return Err("--render-out takes exactly one tape".into());
        }
        Ok(o)
    }
}

pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return 0;
    }
    let o = match Opts::parse(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}\n\n{HELP}");
            return 2;
        }
    };
    go(&o)
}

/// A failure that ends the command.
fn fail(o: &Opts, code: i32, msg: &str, pt: &str) -> i32 {
    if o.json {
        println!("{}", json!({ "ok": false, "exit": code, "error": msg }));
    } else {
        eprintln!("{pt}");
        eprintln!("({msg})");
    }
    code
}

/// Tape folders named by `paths`: a folder with `tape.json` is a tape, any other folder
/// contributes its sub-folders that are tapes.
fn find_tapes(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for p in paths {
        if p.join("tape.json").is_file() {
            out.push(p.clone());
            continue;
        }
        let rd = std::fs::read_dir(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut subs: Vec<PathBuf> = rd
            .filter_map(Result::ok)
            .map(|d| d.path())
            .filter(|d| d.join("tape.json").is_file())
            .collect();
        subs.sort();
        if subs.is_empty() {
            return Err(format!("{}: no tape here", p.display()));
        }
        out.extend(subs);
    }
    Ok(out)
}

/// A contested verdict.
struct Dispute {
    tape: usize,
    round: usize,
    /// Event id of the verdict the player contested, when there was one.
    id: Option<u32>,
    /// Input frame the window is centred on.
    centre_in: f64,
}

/// The disputes of one tape, in tape order. The centre is the onset of the last verdict
/// when it has one, else the input frame of the last clock line before the command.
fn disputes_of(tape: &Tape, index: usize) -> Vec<Dispute> {
    let mut out = Vec::new();
    let mut round = 0usize;
    let mut seen_anchor = false;
    let mut last_id = None;
    let mut last_onset = None;
    let mut last_clock = 0.0;
    for (_, ev) in &tape.events {
        match ev {
            TapeEvent::Anchor { .. } => {
                if seen_anchor {
                    round += 1;
                }
                seen_anchor = true;
                last_id = None;
                last_onset = None;
            }
            TapeEvent::Out(JudgeOut::Verdict(v)) => {
                last_id = Some(v.id);
                last_onset = v.onset_in;
            }
            TapeEvent::Engine(EngineEvent::Clock(c)) => last_clock = c.parts().n_in,
            TapeEvent::Cmd(SessionCmdLog::Dispute) => out.push(Dispute {
                tape: index,
                round,
                id: last_id,
                centre_in: last_onset.unwrap_or(last_clock),
            }),
            _ => {}
        }
    }
    out
}

fn describe(o: &Option<JudgeOut>) -> String {
    match o {
        None => "none".into(),
        Some(JudgeOut::Verdict(v)) => format!(
            "verdict id={} attack={:?} timing={:?} dt_ms={:?} fin={}",
            v.id, v.attack, v.timing, v.dt_ms, v.fin
        ),
        Some(other) => format!("{other:?}"),
    }
}

fn diff_json(d: &Diff) -> Value {
    json!({
        "round": d.round,
        "id": d.id,
        "recorded": describe(&d.recorded),
        "replayed": describe(&d.replayed),
    })
}

fn tape_json(path: &Path, r: &ReplayResult) -> Value {
    let onsets = r.onsets.as_ref().map(|c| {
        json!({
            "recorded": c.recorded, "found": c.found, "matched": c.matched,
            "missing": c.missing, "extra": c.extra, "worst_ms": c.worst_ms,
            "exact": c.is_exact(),
        })
    });
    json!({
        "path": path.display().to_string(),
        "complete": r.tape_complete,
        "rounds": r.rounds.len(),
        "outputs": r.rounds.iter().map(|x| x.recorded.len()).sum::<usize>(),
        "diffs": r.diffs.iter().map(diff_json).collect::<Vec<_>>(),
        "onsets": onsets,
        "notes": r.notes,
        "exact": r.is_exact(),
    })
}

fn go(o: &Opts) -> i32 {
    let paths = match find_tapes(&o.tapes) {
        Ok(p) => p,
        Err(e) => {
            return fail(
                o,
                2,
                &e,
                "Não achei fita nesse caminho. Passe a pasta de uma fita.",
            );
        }
    };
    let mut tapes = Vec::new();
    for p in &paths {
        match Tape::open(p) {
            Ok(t) => tapes.push(t),
            Err(e) => {
                return fail(
                    o,
                    20,
                    &format!("{}: {e}", p.display()),
                    "Não consegui abrir uma das fitas.",
                );
            }
        }
    }

    // Replay every tape.
    let level = if o.audio {
        ReplayLevel::Audio
    } else {
        ReplayLevel::Events
    };
    let base = match &o.gates {
        Some(f) if f.is_file() => match std::fs::read_to_string(f)
            .map_err(|e| e.to_string())
            .and_then(|s| GateTable::from_json(&s).map_err(|e| format!("{e:?}")))
        {
            Ok(g) => g,
            Err(e) => {
                return fail(
                    o,
                    20,
                    &format!("{}: {e}", f.display()),
                    "Não consegui ler o arquivo de portões.",
                );
            }
        },
        _ => GateTable::builtin(),
    };
    let mut results = Vec::new();
    for (p, t) in paths.iter().zip(&tapes) {
        match replay(t, level, &base) {
            Ok(r) => results.push(r),
            Err(e) => {
                return fail(
                    o,
                    20,
                    &format!("{}: {e}", p.display()),
                    if o.audio {
                        "Uma fita não pôde ser reproduzida com o áudio do microfone."
                    } else {
                        "Uma fita não pôde ser reproduzida."
                    },
                );
            }
        }
    }
    let differs = results.iter().any(|r| !r.is_exact());
    let incomplete = results.iter().filter(|r| !r.tape_complete).count();

    // Report and gates.
    let mut gate_failed = false;
    let mut report_text = None;
    let mut report_json = None;
    let mut gates_written = None;
    if o.report {
        let ev = evidence(&tapes, &base);
        gate_failed = ev
            .report
            .types
            .iter()
            .any(|t| t.gate == GateCheck::Fail && base.state(t.vtype) != GateState::Locked);
        report_text = Some(ev.to_text());
        report_json = serde_json::from_str::<Value>(&ev.report.to_json()).ok();
        if let Some(f) = &o.gates {
            let proposed = ev.report.proposed_gates(&base);
            if let Err(e) = std::fs::write(f, proposed.to_json()) {
                return fail(
                    o,
                    20,
                    &format!("{}: {e}", f.display()),
                    "Não consegui gravar o arquivo de portões.",
                );
            }
            gates_written = Some(f.display().to_string());
        }
    }

    // Disputes.
    let mut disputes = Vec::new();
    if o.disputes || o.export.is_some() {
        for (i, t) in tapes.iter().enumerate() {
            disputes.extend(disputes_of(t, i));
        }
    }
    let mut exported = None;
    if let Some((n, wav)) = &o.export {
        let Some(d) = disputes.get(n - 1) else {
            return fail(
                o,
                2,
                &format!("there is no contested verdict number {n}"),
                "Não existe essa contestação. Veja a lista com --disputes.",
            );
        };
        match export_window(&tapes[d.tape], d.centre_in, wav) {
            Ok(frames) => exported = Some((wav.display().to_string(), frames)),
            Err(e) => {
                return fail(
                    o,
                    20,
                    &e,
                    "Não consegui gravar o trecho de áudio dessa contestação.",
                );
            }
        }
    }

    // Rendered output.
    let mut rendered = None;
    if let Some(wav) = &o.render_out {
        match render_clicks(&tapes[0], wav) {
            Ok(beats) => rendered = Some((wav.display().to_string(), beats)),
            Err(e) => {
                return fail(o, 20, &e, "Não consegui recriar o som da saída dessa fita.");
            }
        }
    }

    let code = if differs || gate_failed { 30 } else { 0 };
    if o.json {
        let mut v = json!({
            "ok": code == 0,
            "exit": code,
            "level": if o.audio { "audio" } else { "events" },
            "tapes": paths.iter().zip(&results).map(|(p, r)| tape_json(p, r)).collect::<Vec<_>>(),
            "incomplete_tapes": incomplete,
            "gate_failed": gate_failed,
        });
        if let Some(r) = report_json {
            v["report"] = r;
        }
        if let Some(g) = gates_written {
            v["gates_written"] = json!(g);
        }
        if o.disputes {
            v["disputes"] = Value::Array(
                disputes
                    .iter()
                    .enumerate()
                    .map(|(i, d)| {
                        json!({
                            "n": i + 1,
                            "tape": paths[d.tape].display().to_string(),
                            "round": d.round,
                            "id": d.id,
                            "centre_in_frame": d.centre_in,
                        })
                    })
                    .collect(),
            );
        }
        if let Some((p, frames)) = exported {
            v["exported"] = json!({ "path": p, "frames": frames });
        }
        if let Some((p, beats)) = rendered {
            v["rendered"] = json!({ "path": p, "beats": beats });
        }
        println!("{v}");
        return code;
    }

    // Player text: short, pt-BR, no numbers.
    if !differs {
        println!("Tudo igual: o juiz de hoje diz o mesmo que foi gravado.");
    } else {
        println!("Há diferença entre a fita e o juiz de hoje.");
    }
    if incomplete > 0 {
        println!("Há fita incompleta; ela fica fora dos portões.");
    }
    if gate_failed {
        println!("Um portão não passou na conta e precisa de mais evidência.");
    }
    if o.disputes {
        if disputes.is_empty() {
            println!("Nenhuma contestação nas fitas.");
        } else {
            println!("Há contestações nas fitas; veja os detalhes com --detalhes.");
        }
    }
    if let Some((p, _)) = &exported {
        println!("Trecho da contestação gravado em {p}.");
    }
    if let Some((p, _)) = &rendered {
        println!("Som da saída recriado em {p}.");
    }
    if let Some(g) = &gates_written {
        println!("Portões gravados em {g}.");
    }
    if let Some(t) = &report_text {
        println!("\n{t}");
    }
    if o.details {
        for (p, r) in paths.iter().zip(&results) {
            println!(
                "{}: rodadas {}, completa {}",
                p.display(),
                r.rounds.len(),
                r.tape_complete
            );
            for d in &r.diffs {
                println!(
                    "  rodada {} id {:?}: gravado {} | agora {}",
                    d.round,
                    d.id,
                    describe(&d.recorded),
                    describe(&d.replayed)
                );
            }
            if let Some(c) = &r.onsets {
                println!(
                    "  ataques: gravados {}, achados {}, iguais {}, faltam {}, sobram {}, pior desvio {:.3} ms",
                    c.recorded,
                    c.found,
                    c.matched,
                    c.missing.len(),
                    c.extra.len(),
                    c.worst_ms
                );
            }
            for n in &r.notes {
                println!("  nota: {n}");
            }
        }
        for (i, d) in disputes.iter().enumerate() {
            println!(
                "contestação {}: {} rodada {} id {:?} quadro de entrada {:.0}",
                i + 1,
                paths[d.tape].display(),
                d.round,
                d.id,
                d.centre_in
            );
        }
    }
    code
}

/// Writes the microphone around `centre_in` (input frames), `DISPUTE_HALF_S` each side.
fn export_window(tape: &Tape, centre_in: f64, wav: &Path) -> Result<usize, String> {
    let fs = tape.header.fs_in;
    let half = (DISPUTE_HALF_S * f64::from(fs)).round() as u64;
    let c = centre_in.max(0.0).round() as u64;
    let from = c.saturating_sub(half);
    let to = c + half;
    let samples = tape.audio(from, to).map_err(|e| format!("{e}"))?;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: fs,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w =
        hound::WavWriter::create(wav, spec).map_err(|e| format!("{}: {e}", wav.display()))?;
    for s in &samples {
        w.write_sample(*s).map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())?;
    Ok(samples.len())
}

/// Writes a click at every metronome beat the tape's mixer reported, at its output frame.
/// The tape keeps the schedule, not the sound, so this is the timing of the output with
/// a plain click (1 kHz, 30 ms), not the click the player heard.
fn render_clicks(tape: &Tape, wav: &Path) -> Result<usize, String> {
    let beats: Vec<u64> = tape
        .events
        .iter()
        .filter_map(|(_, e)| match e {
            TapeEvent::Engine(EngineEvent::Mix(MixEvent::Beat { at, .. })) => Some(*at),
            _ => None,
        })
        .collect();
    let Some(&last) = beats.iter().max() else {
        return Err("the tape has no output schedule (no beat events)".into());
    };
    let fs = tape.header.fs_out;
    let ch = tape.header.ch_out.max(1);
    let click_len = (0.030 * f64::from(fs)) as usize;
    let total = last as usize + click_len + (0.5 * f64::from(fs)) as usize;
    let mut mono = vec![0.0f32; total];
    for &at in &beats {
        for i in 0..click_len {
            let t = i as f64 / f64::from(fs);
            let env = 1.0 - i as f64 / click_len as f64;
            mono[at as usize + i] +=
                (0.5 * env * (2.0 * std::f64::consts::PI * 1000.0 * t).sin()) as f32;
        }
    }
    let spec = hound::WavSpec {
        channels: ch,
        sample_rate: fs,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w =
        hound::WavWriter::create(wav, spec).map_err(|e| format!("{}: {e}", wav.display()))?;
    for s in &mono {
        for _ in 0..ch {
            w.write_sample(*s).map_err(|e| e.to_string())?;
        }
    }
    w.finalize().map_err(|e| e.to_string())?;
    Ok(beats.len())
}
