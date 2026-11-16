//! `tonica calibrate`: the sound check (Pulso plan L2, P19).
//!
//! Drives a [`Calibrator`] on the real devices or, with `--fake`, on the deterministic
//! fake backend (virtual time, no hardware). The default text is short, in Brazilian
//! Portuguese and has no numbers; `--detalhes` adds the measurements. Sound is played
//! only with `--beep` on real devices, as in `tonica audio`. Nothing here is a failure
//! screen: a result that cannot be judged is "vamos medir de novo depois".

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tonica_audio::calib::{Basis, BeatStep};
use tonica_audio::{
    AudioEngine, AudioError, Backend, CalibConfig, Calibrator, CpalBackend, EngineConfig,
    FakeBackend, FakeConfig, FakeHandle, Outcome, Profile, ProfileKey, Prompt, Pump, SoundBank,
    Warning, find_profile, profiles_from_json, profiles_to_json, upsert_profile,
};

const HELP: &str = "\
tonica calibrate: sound check (measures the latency of the loudspeaker-to-microphone loop)

USAGE
  tonica calibrate [--beep] [--detalhes] [--short] [--repeat N] [--json]
                   [--in ID] [--out ID] [--profile FILE] [--fake [--speed X]]

OPTIONS
  --beep          allow sound on the real devices (the check plays quiet beeps and clicks).
                  Without it nothing is played and the command stops. Not needed with --fake.
  --detalhes      also print the measurements (ms, dBFS) after the text
  --short         the short check (about 4 s): fewer beeps, then the full one only if needed
  --repeat N      run the whole check N times (default 1); the report gives the range of L_rt
  --json          machine-readable report on stdout (no player text)
  --in ID / --out ID  device ids from `tonica audio devices` (default: system defaults)
  --profile FILE  profile file to read and update (default: ~/.tonica/profiles.json;
                  with --fake nothing is saved unless this is given)
  --fake          run on the fake backend (no hardware; a room with 35 ms of loop latency)
  --speed X       with --fake: virtual seconds per wall second (default: as fast as possible)

Set TONICA_FORBID_DEVICES to refuse real devices (exit 12 before anything is opened).

EXIT CODES
   0  done (including \"rhythm shown, not judged yet\")
  10  no input device        11  no output device
  12  permission denied, microphone silent, stream failed, devices forbidden or interrupted
  13  unsupported sample rate or format
  14  clock never settled
  20  other error
   2  bad command line
";

/// Longest a single check may take, seconds of (virtual) time.
const MAX_RUN_S: f64 = 150.0;
/// Time between steps of the calibrator, seconds.
const SLICE_S: f64 = 0.005;
/// Microphone noise of the fake room (-60 dBFS RMS).
const FAKE_NOISE: f32 = 1.0e-3;

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
    if !o.fake {
        if std::env::var_os("TONICA_FORBID_DEVICES").is_some() {
            return report_error(
                &o,
                12,
                "real audio devices are forbidden (TONICA_FORBID_DEVICES is set)",
                "Os dispositivos de som reais estão desligados aqui. Nada foi aberto.",
            );
        }
        if !o.beep {
            return report_error(
                &o,
                2,
                "the sound check plays beeps; add --beep to allow sound",
                "A passagem de som toca bipes baixinhos. Quando quiser, rode de novo com --beep.",
            );
        }
    }
    session(&o)
}

struct Opts {
    json: bool,
    fake: bool,
    beep: bool,
    details: bool,
    short: bool,
    repeat: u32,
    in_id: Option<String>,
    out_id: Option<String>,
    profile: Option<PathBuf>,
    speed: Option<f64>,
}

impl Opts {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut o = Opts {
            json: false,
            fake: false,
            beep: false,
            details: false,
            short: false,
            repeat: 1,
            in_id: None,
            out_id: None,
            profile: None,
            speed: None,
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut val = |name: &str| -> Result<String, String> {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            match a.as_str() {
                "--json" => o.json = true,
                "--fake" => o.fake = true,
                "--beep" => o.beep = true,
                "--detalhes" => o.details = true,
                "--short" => o.short = true,
                "--in" => o.in_id = Some(val("--in")?),
                "--out" => o.out_id = Some(val("--out")?),
                "--profile" => o.profile = Some(PathBuf::from(val("--profile")?)),
                "--repeat" => {
                    let s = val("--repeat")?;
                    o.repeat = s
                        .parse::<u32>()
                        .ok()
                        .filter(|n| (1..=50).contains(n))
                        .ok_or_else(|| format!("--repeat: '{s}' is not a count from 1 to 50"))?;
                }
                "--speed" => {
                    let s = val("--speed")?;
                    o.speed = Some(
                        s.parse::<f64>()
                            .ok()
                            .filter(|v| v.is_finite() && *v > 0.0)
                            .ok_or_else(|| format!("--speed: '{s}' is not a positive number"))?,
                    );
                }
                other => return Err(format!("unknown option '{other}'")),
            }
        }
        if o.speed.is_some() && !o.fake {
            return Err("--speed needs --fake".into());
        }
        Ok(o)
    }

    /// Where profiles are read from and written to. The fake backend saves only when a
    /// file was named, so it never touches the player's real profile.
    fn profile_path(&self) -> Option<PathBuf> {
        if let Some(p) = &self.profile {
            return Some(p.clone());
        }
        if self.fake {
            return None;
        }
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
        Some(PathBuf::from(home).join(".tonica").join("profiles.json"))
    }
}

// ------------------------------------------------------------------ errors and exit codes

fn report_error(o: &Opts, code: i32, tech: &str, player: &str) -> i32 {
    if o.json {
        println!("{}", json!({"ok": false, "error": tech, "exit": code}));
    } else {
        println!("{player}");
        if o.details {
            eprintln!("({tech}; exit {code})");
        }
    }
    code
}

fn audio_exit(e: &AudioError) -> i32 {
    match e {
        AudioError::NoDevice(what) => {
            let w = what.to_lowercase();
            if w.contains("output") && !w.contains("input") {
                11
            } else {
                10
            }
        }
        AudioError::UnsupportedRate(_) | AudioError::UnsupportedFormat(_) => 13,
        AudioError::Backend(msg) => {
            let m = msg.to_lowercase();
            if m.contains("permission")
                || m.contains("denied")
                || m.contains("stream")
                || m.contains("access")
            {
                12
            } else {
                20
            }
        }
    }
}

fn audio_error(o: &Opts, e: &AudioError) -> i32 {
    let code = audio_exit(e);
    let player = match code {
        10 => "Não achei um microfone. Conecte um e tente de novo quando quiser.",
        11 => "Não achei uma saída de som. Conecte uma e tente de novo quando quiser.",
        12 => "Não consegui abrir o som. Veja se o app tem permissão para o microfone.",
        13 => "Esse dispositivo usa um formato de som que ainda não conheço.",
        _ => "Não deu para abrir o som agora. Tente de novo quando quiser.",
    };
    report_error(o, code, &e.to_string(), player)
}

// ------------------------------------------------------------------ one check

struct RunResult {
    outcome: Outcome,
    details: tonica_audio::Details,
    key: ProfileKey,
    /// Prompts shown, in order, as player text.
    shown: Vec<&'static str>,
}

fn civil_date() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn host_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "CoreAudio"
    } else if cfg!(target_os = "windows") {
        "WASAPI"
    } else {
        "ALSA"
    }
}

/// What the player reads for a prompt. No numbers, no failure wording.
fn prompt_text(p: Prompt) -> &'static str {
    match p {
        Prompt::Checking => "Conferindo o que já sabemos sobre o seu som. É só ouvir.",
        Prompt::Quiet => "Fique em silêncio por um instante.",
        Prompt::TooLoud => {
            "Está bem alto por aqui. Afaste-se um pouco do alto-falante e vamos ouvir o silêncio de novo."
        }
        Prompt::WillBeep { .. } => "Vão tocar uns bipes. Você não precisa tocar nada.",
        Prompt::WillBeepLouder { .. } => {
            "Mais uma vez, um pouquinho mais alto. Você não precisa tocar nada."
        }
        Prompt::StrumMuted { count_in, .. } => {
            if count_in > 0 {
                "Escute os cliques."
            } else {
                "Abafe as cordas com a mão e passe a palheta no tempo de cada clique."
            }
        }
        Prompt::Done => "Pronto! O som está ajustado.",
        Prompt::RhythmLater => {
            "O ritmo aparece, mas ainda não vai ser avaliado. Vamos medir de novo depois."
        }
        Prompt::MicBlocked => {
            "Não estou ouvindo o microfone. Veja se ele está liberado e fora do mudo; quando quiser, é só passar o som de novo."
        }
        Prompt::Interrupted => "O som mudou ou parou. Quando quiser, é só passar o som de novo.",
    }
}

/// How one check advances time.
trait Clockwork {
    /// Moves time forward by about one slice. Returns seconds elapsed since the start.
    fn tick<B: Backend>(&mut self, eng: &mut AudioEngine<B>) -> (f64, bool);
}

/// Virtual time on the fake backend, pumped by hand.
struct Virtual {
    h: FakeHandle,
    start: f64,
    speed: Option<f64>,
}

impl Clockwork for Virtual {
    fn tick<B: Backend>(&mut self, eng: &mut AudioEngine<B>) -> (f64, bool) {
        self.h.pump(SLICE_S);
        eng.pump();
        if let Some(s) = self.speed {
            std::thread::sleep(Duration::from_secs_f64(SLICE_S / s));
        }
        (self.h.now_s() - self.start, true)
    }
}

/// Wall time on the real devices (the engine thread does the analysis).
struct Wall {
    start: Instant,
}

impl Clockwork for Wall {
    fn tick<B: Backend>(&mut self, eng: &mut AudioEngine<B>) -> (f64, bool) {
        std::thread::sleep(Duration::from_secs_f64(SLICE_S));
        let alive = eng.poll();
        (self.start.elapsed().as_secs_f64(), alive)
    }
}

fn run_check<B: Backend, C: Clockwork>(
    backend: B,
    clock: &mut C,
    o: &Opts,
    run_index: u32,
    saved: &[Profile],
    host: &str,
) -> Result<RunResult, AudioError> {
    let ec = EngineConfig {
        open: tonica_audio::OpenRequest {
            input_id: o.in_id.clone(),
            output_id: o.out_id.clone(),
            buffer_frames: None,
        },
        ..EngineConfig::default()
    };
    let pump = if o.fake { Pump::Manual } else { Pump::Thread };
    let (mut eng, mut io) = AudioEngine::start(backend, ec, SoundBank::new(48_000.0), pump)?;
    let key = ProfileKey::new(host, eng.info());
    let date = civil_date();
    let mut cfg = if o.short {
        CalibConfig::short(key.clone(), date)
    } else {
        CalibConfig::new(key.clone(), date)
    };
    cfg.beat_step = BeatStep::WhenNoLoop;
    cfg.seed = cfg.seed.wrapping_add(run_index);
    // Later repeats measure from scratch; only the first one may be the quick check.
    let profile = if run_index == 0 {
        find_profile(saved, &key).cloned()
    } else {
        None
    };
    let mut cal = Calibrator::new(cfg, profile);
    let mut shown: Vec<&'static str> = Vec::new();
    let mut cancelled = false;
    loop {
        let (t, alive) = clock.tick(&mut eng);
        let up = cal.step_io(&mut io);
        if up.changed || shown.is_empty() {
            let text = prompt_text(up.prompt);
            if shown.last() != Some(&text) {
                shown.push(text);
                if !o.json {
                    println!("{text}");
                }
            }
        }
        if up.done && cal.flushed() {
            break;
        }
        if !cancelled && (!alive || t > MAX_RUN_S) {
            cal.cancel(&mut io.mix, &mut io.tap);
            cancelled = true;
        }
    }
    let outcome = cal.outcome().cloned().unwrap_or(Outcome::Interrupted);
    if matches!(outcome, Outcome::Kept(_) | Outcome::Calibrated(_)) {
        eng.clear_calibration_stale();
    }
    let details = cal.details().clone();
    eng.stop();
    Ok(RunResult {
        outcome,
        details,
        key,
        shown,
    })
}

fn one_run(o: &Opts, run_index: u32, saved: &[Profile]) -> Result<RunResult, AudioError> {
    if o.fake {
        let mut fc = FakeConfig::default();
        fc.path.noise_rms = FAKE_NOISE;
        fc.seed = fc.seed.wrapping_add(run_index);
        let backend = FakeBackend::new(fc);
        let mut clock = Virtual {
            h: backend.handle(),
            start: 0.0,
            speed: o.speed,
        };
        clock.start = clock.h.now_s();
        run_check(backend, &mut clock, o, run_index, saved, "fake")
    } else {
        let mut clock = Wall {
            start: Instant::now(),
        };
        run_check(
            CpalBackend::new(),
            &mut clock,
            o,
            run_index,
            saved,
            host_name(),
        )
    }
}

// ------------------------------------------------------------------ report

fn basis_name(b: Basis) -> &'static str {
    match b {
        Basis::Loopback => "loopback",
        Basis::Estimate => "estimate",
        Basis::None => "none",
    }
}

fn outcome_name(o: &Outcome) -> &'static str {
    match o {
        Outcome::Kept(_) => "kept",
        Outcome::Calibrated(_) => "calibrated",
        Outcome::Unstable => "unstable",
        Outcome::MicBlocked => "mic_blocked",
        Outcome::Interrupted => "interrupted",
    }
}

fn outcome_exit(o: &Outcome) -> i32 {
    match o {
        Outcome::Kept(_) | Outcome::Calibrated(_) => 0,
        Outcome::Unstable => 14,
        Outcome::MicBlocked | Outcome::Interrupted => 12,
    }
}

fn run_basis(r: &RunResult) -> Basis {
    r.outcome.profile().map_or(Basis::None, Profile::basis)
}

fn run_json(r: &RunResult) -> Value {
    let p = r.outcome.profile();
    let d = &r.details;
    json!({
        "outcome": outcome_name(&r.outcome),
        "basis": basis_name(run_basis(r)),
        "judgeable": p.is_some_and(|p| p.judgeable),
        "l_rt_ms": p.and_then(|p| p.l_rt_ms),
        "mad_ms": p.and_then(|p| p.mad_ms),
        "tendency_ms": p.and_then(|p| p.tendency_ms),
        "combined_ms": p.and_then(|p| p.combined_ms),
        "judge_offset_ms": p.and_then(Profile::judge_offset_ms),
        "noise_floor_dbfs": d.noise_floor_dbfs,
        "rounds": d.rounds.len(),
        "seconds": d.seconds,
        "clock_quality": format!("{:?}", d.clock_quality).to_lowercase(),
        "drift_ppm": d.drift_ppm,
        "warnings": d.warnings.iter().map(|w| format!("{w:?}")).collect::<Vec<_>>(),
    })
}

fn fmt_opt(v: Option<f64>, unit: &str) -> String {
    v.map_or_else(|| "-".to_string(), |x| format!("{x:.2} {unit}"))
}

fn print_details(index: usize, r: &RunResult) {
    let d = &r.details;
    let p = r.outcome.profile();
    println!("Detalhes da medida {}:", index + 1);
    println!("  resultado: {}", outcome_name(&r.outcome));
    println!("  base: {}", basis_name(run_basis(r)));
    println!(
        "  latência do ciclo (L_rt): {} (MAD {})",
        fmt_opt(d.l_rt_ms, "ms"),
        fmt_opt(d.l_rt_mad_ms, "ms")
    );
    println!("  tendência do toque: {}", fmt_opt(d.tendency_ms, "ms"));
    println!(
        "  combinado (sem laço): {} (MAD {})",
        fmt_opt(d.combined_ms, "ms"),
        fmt_opt(d.combined_mad_ms, "ms")
    );
    println!(
        "  deslocamento usado no juiz: {}",
        fmt_opt(p.and_then(Profile::judge_offset_ms), "ms")
    );
    println!(
        "  ruído de fundo: {}, pico: {}",
        fmt_opt(d.noise_floor_dbfs, "dBFS"),
        fmt_opt(d.input_peak_dbfs, "dBFS")
    );
    println!(
        "  rodadas de bipes: {}, batidas ouvidas {} de {} usadas",
        d.rounds.len(),
        d.strums_heard,
        d.strums_used
    );
    println!(
        "  relógio: {:?}, deriva {:.1} ppm, duração {:.1} s",
        d.clock_quality, d.drift_ppm, d.seconds
    );
    if d.warnings.is_empty() {
        println!("  avisos: nenhum");
    } else {
        let w: Vec<String> = d.warnings.iter().map(warning_text).collect();
        println!("  avisos: {}", w.join("; "));
    }
}

fn warning_text(w: &Warning) -> String {
    format!("{w:?}")
}

/// The closing line for the player, from what the calibration can judge with.
fn closing_text(r: &RunResult) -> &'static str {
    match (&r.outcome, run_basis(r)) {
        (Outcome::Kept(_), Basis::Loopback | Basis::Estimate) => {
            "Tudo certo: o que medimos antes continua valendo."
        }
        (Outcome::Calibrated(_), Basis::Loopback) => {
            "Tudo certo: já sei quanto tempo o som leva até o microfone."
        }
        (Outcome::Calibrated(_), Basis::Estimate) => {
            "Ajustado pelo seu toque: o ritmo já pode ser avaliado."
        }
        (Outcome::Unstable, _) => {
            "O som ainda está se acomodando. O ritmo aparece sem avaliação; vamos medir de novo depois."
        }
        _ => prompt_text(Prompt::RhythmLater),
    }
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    })
}

fn load_profiles(path: Option<&Path>) -> Vec<Profile> {
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| profiles_from_json(&t))
        .unwrap_or_default()
}

fn save_profiles(path: &Path, profiles: &[Profile]) -> std::io::Result<()> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, profiles_to_json(profiles))
}

fn session(o: &Opts) -> i32 {
    let path = o.profile_path();
    let mut profiles = load_profiles(path.as_deref());
    let mut runs: Vec<RunResult> = Vec::new();
    for i in 0..o.repeat {
        if !o.json && o.repeat > 1 {
            println!(
                "{}",
                if i == 0 {
                    "Vamos medir mais de uma vez."
                } else {
                    "Mais uma vez."
                }
            );
        }
        match one_run(o, i, &profiles) {
            Ok(r) => runs.push(r),
            Err(e) => return audio_error(o, &e),
        }
        if !runs.last().is_some_and(|r| r.outcome.profile().is_some()) {
            break;
        }
    }

    // Save the last profile that exists.
    let mut saved_to: Option<PathBuf> = None;
    let mut save_error: Option<String> = None;
    if let (Some(path), Some(p)) = (
        path.as_deref(),
        runs.iter().rev().find_map(|r| r.outcome.profile()),
    ) {
        upsert_profile(&mut profiles, p.clone());
        match save_profiles(path, &profiles) {
            Ok(()) => saved_to = Some(path.to_path_buf()),
            Err(e) => save_error = Some(e.to_string()),
        }
    }

    let mut l_rts: Vec<f64> = runs.iter().filter_map(|r| r.details.l_rt_ms).collect();
    let range = (l_rts.len() > 1).then(|| {
        let max = l_rts.iter().copied().fold(f64::MIN, f64::max);
        let min = l_rts.iter().copied().fold(f64::MAX, f64::min);
        max - min
    });
    let l_rt = median(&mut l_rts);
    let last = runs.last().expect("at least one run");
    let exit = runs
        .iter()
        .map(|r| outcome_exit(&r.outcome))
        .max()
        .unwrap_or(0);

    if o.json {
        println!(
            "{}",
            json!({
                "ok": exit == 0,
                "exit": exit,
                "fake": o.fake,
                "short": o.short,
                "repeat": o.repeat,
                "basis": basis_name(run_basis(last)),
                "judgeable": last.outcome.profile().is_some_and(|p| p.judgeable),
                "l_rt_ms": l_rt,
                "range_ms": range,
                "saved": saved_to.as_ref().map(|p| p.display().to_string()),
                "save_error": save_error,
                "runs": runs.iter().map(run_json).collect::<Vec<_>>(),
            })
        );
        return exit;
    }

    println!("{}", closing_text(last));
    if let Some(r) = range {
        if r <= 2.0 {
            println!("As medidas deram resultados bem parecidos.");
        } else {
            println!("As medidas variaram um pouco; vamos medir de novo depois.");
        }
    }
    if save_error.is_some() {
        println!("Não consegui guardar o resultado agora; ele vale só para esta vez.");
    }
    if o.details {
        println!();
        for (i, r) in runs.iter().enumerate() {
            print_details(i, r);
        }
        if let Some(r) = range {
            println!("faixa de L_rt entre as medidas: {r:.2} ms");
        }
        match (&saved_to, &save_error) {
            (Some(p), _) => println!("perfil salvo em: {}", p.display()),
            (None, Some(e)) => println!("perfil não salvo: {e}"),
            (None, None) => println!("perfil não salvo (sem arquivo de perfil)"),
        }
        println!("chave: {:?}", last.key);
        println!("textos mostrados: {}", last.shown.len());
    }
    exit
}
