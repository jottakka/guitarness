//! `tonica audio devices | probe | soak | record`: live-audio checks (plan M2, T9 to T11)
//! and the take recorder (Pulso L3).
//!
//! Everything is generic over [`Backend`], so `--fake` runs the same code on the
//! deterministic `FakeBackend` (CI, no hardware). Without `--beep` nothing is ever played.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tonica_audio::{
    AudioEngine, AudioError, Backend, BackendEvent, ClockQuality, ClockSnapshot, CpalBackend,
    EngineConfig, EngineEvent, EngineIo, FakeBackend, FakeConfig, MixCmd, MixEvent, OpenRequest,
    Pump, SoundBank, SoundId, Stats,
};
use tonica_core::chart::Chart;
use tonica_dsp::latency::{LoopbackConfig, Probe, measure_loopback};
use tonica_session::session::{
    Notice, Session, SessionCmd, SessionConfig, SessionEvent, TapeNote, default_profile_path,
};

const HELP: &str = "\
tonica audio: live audio checks

USAGE
  tonica audio devices [--json] [--fake]
  tonica audio probe   [--json] [--in ID] [--out ID] [--seconds N] [--beep] [--buffer FRAMES] [--fake]
  tonica audio soak    --minutes M [--json] [--in ID] [--out ID] [--probe-every S] [--beep]
                       [--buffer FRAMES] [--fake]
  tonica audio record  FOLDER [--seconds N] [--take LABEL] [--chart FILE.json] [--click BPM]
                       [--beep] [--profile FILE] [--json] [--in ID] [--out ID] [--buffer FRAMES]
                       [--fake]

OPTIONS
  --json            machine-readable output on stdout
  --in ID / --out ID  device ids from `audio devices` (default: system defaults)
  --seconds N       probe length, default 3
  --minutes M       soak length (decimals allowed)
  --beep            allow sound: probe plays a quiet click (-20 dBFS) every 0.5 s;
                    soak --probe-every sends a quiet chirp probe every S s to measure L_rt.
                    Without --beep nothing is played at all.
  --probe-every S   soak only, needs --beep
  --buffer FRAMES   fixed buffer size, default: device default
  --fake            run on the fake backend (no hardware, deterministic)
  --speed X         with --fake: virtual seconds per wall second (default 1)

RECORD
  Records the microphone into a tape folder (FOLDER/AAAAMMDD-HHMMSS-xxxx: tape.json,
  events.jsonl, mic.wav) for --seconds (default 10). The take is stored under --take
  (default \"take\"). --chart plays that chart's click, --click BPM a plain click.
  Clicks are silent unless --beep is given. Nothing is judged for the player: the tape
  only keeps what the session heard. Exit 12 if the device stops, 20 if the tape is
  incomplete or could not be written.

EXIT CODES (probe; soak uses the same plus 30)
   0  ok
  10  no input device
  11  no output device
  12  permission denied / stream failed / no callbacks / device lost
  13  unsupported sample rate or format
  14  clock stamps unusable (quality degraded or broken)
  20  other error
  30  soak ran, but a criterion failed (see \"checks\" in the JSON)
   2  bad command line
";

pub fn run(args: &[String]) -> i32 {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return if args.is_empty() { 2 } else { 0 };
    }
    let sub = args[0].as_str();
    let opts = match Opts::parse(&args[1..]) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}\n\n{HELP}");
            return 2;
        }
    };
    if sub != "record"
        && (opts.folder.is_some()
            || opts.take.is_some()
            || opts.chart.is_some()
            || opts.click.is_some()
            || opts.profile.is_some())
    {
        eprintln!("error: that option or argument belongs to `audio record`\n\n{HELP}");
        return 2;
    }
    match sub {
        "devices" => devices(&opts),
        "record" => record_cmd(&opts),
        "probe" | "soak" => {
            if sub == "soak" && opts.minutes.is_none() {
                eprintln!("error: soak needs --minutes M");
                return 2;
            }
            if opts.probe_every.is_some() && !opts.beep {
                eprintln!("error: --probe-every plays sound; add --beep to allow it");
                return 2;
            }
            session_cmd(sub == "soak", &opts)
        }
        _ => {
            eprintln!("error: unknown audio command '{sub}'\n\n{HELP}");
            2
        }
    }
}

struct Opts {
    json: bool,
    fake: bool,
    beep: bool,
    in_id: Option<String>,
    out_id: Option<String>,
    seconds: f64,
    minutes: Option<f64>,
    probe_every: Option<f64>,
    buffer: Option<u32>,
    speed: f64,
    // record only
    folder: Option<PathBuf>,
    seconds_given: bool,
    take: Option<String>,
    chart: Option<PathBuf>,
    click: Option<f64>,
    profile: Option<PathBuf>,
}

impl Opts {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut o = Opts {
            json: false,
            fake: false,
            beep: false,
            in_id: None,
            out_id: None,
            seconds: 3.0,
            minutes: None,
            probe_every: None,
            buffer: None,
            speed: 1.0,
            folder: None,
            seconds_given: false,
            take: None,
            chart: None,
            click: None,
            profile: None,
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut val = |name: &str| -> Result<String, String> {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            fn num(name: &str, s: String) -> Result<f64, String> {
                s.parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .ok_or_else(|| format!("{name}: '{s}' is not a positive number"))
            }
            match a.as_str() {
                "--json" => o.json = true,
                "--fake" => o.fake = true,
                "--beep" => o.beep = true,
                "--in" => o.in_id = Some(val("--in")?),
                "--out" => o.out_id = Some(val("--out")?),
                "--seconds" => {
                    o.seconds = num("--seconds", val("--seconds")?)?;
                    o.seconds_given = true;
                }
                "--take" => o.take = Some(val("--take")?),
                "--chart" => o.chart = Some(PathBuf::from(val("--chart")?)),
                "--click" => o.click = Some(num("--click", val("--click")?)?),
                "--profile" => o.profile = Some(PathBuf::from(val("--profile")?)),
                "--minutes" => o.minutes = Some(num("--minutes", val("--minutes")?)?),
                "--probe-every" => {
                    o.probe_every = Some(num("--probe-every", val("--probe-every")?)?)
                }
                "--speed" => o.speed = num("--speed", val("--speed")?)?,
                "--buffer" => {
                    let s = val("--buffer")?;
                    o.buffer = Some(
                        s.parse::<u32>()
                            .ok()
                            .filter(|n| *n > 0)
                            .ok_or_else(|| format!("--buffer: '{s}' is not a frame count"))?,
                    );
                }
                other if !other.starts_with('-') && o.folder.is_none() => {
                    o.folder = Some(PathBuf::from(other));
                }
                other => return Err(format!("unknown option '{other}'")),
            }
        }
        Ok(o)
    }

    fn open_request(&self) -> OpenRequest {
        OpenRequest {
            input_id: self.in_id.clone(),
            output_id: self.out_id.clone(),
            buffer_frames: self.buffer,
        }
    }
}

/// Maps an audio error to the documented exit code.
fn exit_code(e: &AudioError) -> i32 {
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

fn fail(json: bool, e: &AudioError) -> i32 {
    let code = exit_code(e);
    if json {
        println!(
            "{}",
            json!({"ok": false, "error": e.to_string(), "exit": code})
        );
    } else {
        eprintln!("error: {e} (exit {code})");
    }
    code
}

// ---------------------------------------------------------------- devices

fn devices(o: &Opts) -> i32 {
    let list = if o.fake {
        FakeBackend::new(FakeConfig::default()).devices()
    } else {
        CpalBackend::new().devices()
    };
    let list = match list {
        Ok(l) => l,
        Err(e) => return fail(o.json, &e),
    };
    if o.json {
        let devs: Vec<Value> = list
            .iter()
            .map(|d| {
                json!({"id": d.id, "name": d.name, "input": d.is_input,
                       "output": d.is_output, "default": d.is_default})
            })
            .collect();
        println!("{}", json!({"ok": true, "devices": devs}));
    } else if list.is_empty() {
        println!("no audio devices");
    } else {
        for d in &list {
            let dir = match (d.is_input, d.is_output) {
                (true, true) => "in/out",
                (true, false) => "in",
                (false, true) => "out",
                _ => "-",
            };
            println!(
                "{:<7}{}{}  id={}",
                dir,
                d.name,
                if d.is_default { "  [default]" } else { "" },
                d.id
            );
        }
    }
    0
}

// ---------------------------------------------------------------- session

#[derive(Default)]
struct Acc {
    late: u64,
    rejected: u64,
    steps: u64,
    step_notes: Vec<String>,
    unjudged: u64,
    dropped: u64,
    backend_events: Vec<String>,
    stopped: Option<String>,
    snap: Option<ClockSnapshot>,
    drift_min: Option<f64>,
    drift_max: Option<f64>,
}

impl Acc {
    fn drain(&mut self, io: &mut EngineIo) {
        while let Ok(ev) = io.events.pop() {
            match ev {
                EngineEvent::Mix(MixEvent::Late { .. }) => self.late += 1,
                EngineEvent::Mix(MixEvent::Rejected { .. }) => self.rejected += 1,
                EngineEvent::Step(s) => {
                    self.steps += 1;
                    if self.step_notes.len() < 8 {
                        self.step_notes.push(format!(
                            "{:?} {:?} {:+.3} ms",
                            s.side,
                            s.cause,
                            s.step_s * 1e3
                        ));
                    }
                }
                EngineEvent::Unjudged { .. } => self.unjudged += 1,
                EngineEvent::Dropped { n, .. } => self.dropped += u64::from(n),
                EngineEvent::Backend(b) => self.note_backend("backend", b),
                EngineEvent::Stopped(b) => {
                    self.stopped = Some(format!("{b:?}"));
                    self.note_backend("stopped", b);
                }
                EngineEvent::Clock(c) => {
                    if c.quality() == ClockQuality::Locked {
                        let d = c.drift_ppm();
                        self.drift_min = Some(self.drift_min.map_or(d, |m| m.min(d)));
                        self.drift_max = Some(self.drift_max.map_or(d, |m| m.max(d)));
                    }
                    self.snap = Some(c);
                }
                _ => {}
            }
        }
    }

    fn note_backend(&mut self, what: &str, b: BackendEvent) {
        if self.backend_events.len() < 16 {
            self.backend_events.push(format!("{what}:{b:?}"));
        }
    }
}

/// Chirp-probe scheduler and L_rt measurement for one run.
struct Prober {
    every: f64,
    next_t: f64,
    lead_s: f64,
    fs_in: f64,
    fs_out: f64,
    probe: Probe,
    cfg: LoopbackConfig,
    pending: Vec<(f64, f64)>, // (emit_abs in input frames, scheduled at t)
    buf: Vec<f32>,
    buf_start: u64,
    have_origin: bool,
    sent: u32,
    /// (time in minutes, latency in ms) per heard probe.
    heard: Vec<(f64, f64)>,
    done: u32,
}

const PROBE_GAIN: f32 = 0.1; // -20 dBFS
const SEARCH_S: f64 = 0.4;
const FIRST_PROBE_S: f64 = 1.0;

impl Prober {
    fn new(every: f64, lead_s: f64, fs_in: f64, fs_out: f64) -> Self {
        Self {
            every,
            next_t: FIRST_PROBE_S,
            lead_s,
            fs_in,
            fs_out,
            probe: Probe::new(fs_in),
            cfg: LoopbackConfig {
                search_max_s: SEARCH_S,
                min_found: 1,
                ..LoopbackConfig::default()
            },
            pending: Vec::new(),
            buf: Vec::new(),
            buf_start: 0,
            have_origin: false,
            sent: 0,
            heard: Vec::new(),
            done: 0,
        }
    }

    fn step(&mut self, t: f64, total_s: f64, eng_frames_out: u64, io: &mut EngineIo, acc: &Acc) {
        let mut tmp = [0.0f32; 4096];
        loop {
            let n = io.tap.read(&mut tmp);
            if n == 0 {
                break;
            }
            if !self.have_origin {
                self.buf_start = io.tap.origin();
                self.have_origin = true;
            }
            self.buf.extend_from_slice(&tmp[..n]);
        }
        let window = SEARCH_S + 0.05;
        if self.have_origin
            && t >= self.next_t
            && t + self.lead_s + window + 0.1 < total_s
            && let Some(snap) = acc.snap
        {
            let at = eng_frames_out + (self.lead_s * self.fs_out) as u64;
            let emit = snap.in_of_out(at as f64);
            let tag = 1000 + self.sent;
            if io.mix.send(MixCmd::Play {
                tag,
                sound: SoundId::Probe,
                at,
                gain: PROBE_GAIN,
            }) {
                self.pending.push((emit, t));
                self.sent += 1;
            }
            self.next_t += self.every;
        }
        // Measure probes whose window is fully recorded.
        let end = self.buf_start + self.buf.len() as u64;
        let need = (window * self.fs_in) as u64;
        while let Some(&(emit, t0)) = self.pending.first() {
            if end < emit.max(0.0) as u64 + need {
                break;
            }
            self.pending.remove(0);
            self.done += 1;
            let start = emit.floor() as i64 - self.buf_start as i64;
            if start < 0 {
                continue;
            }
            let start = start as usize;
            let rel = emit - (self.buf_start + start as u64) as f64;
            if let Ok(lb) = measure_loopback(&self.buf[start..], &[rel], &self.probe, &self.cfg)
                && let Some(Some(lat)) = lb.per_probe.first()
            {
                self.heard.push((t0 / 60.0, lat / self.fs_in * 1e3));
            }
        }
        // Drop what no pending probe needs.
        let keep_from = self
            .pending
            .first()
            .map_or(end, |&(e, _)| e.floor().max(0.0) as u64)
            .clamp(self.buf_start, end);
        let cut = (keep_from - self.buf_start) as usize;
        if cut > 0 {
            self.buf.drain(..cut);
            self.buf_start = keep_from;
        }
    }

    fn report(&self, run_min: f64) -> Value {
        let n = self.heard.len();
        let sent = self.done.max(1);
        let lat: Vec<f64> = self.heard.iter().map(|h| h.1).collect();
        let mean = (n > 0).then(|| lat.iter().sum::<f64>() / n as f64);
        let sigma = (n > 1).then(|| {
            let m = mean.unwrap_or(0.0);
            (lat.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt()
        });
        let slope = (n >= 3).then(|| {
            let tm = self.heard.iter().map(|h| h.0).sum::<f64>() / n as f64;
            let lm = mean.unwrap_or(0.0);
            let num: f64 = self.heard.iter().map(|h| (h.0 - tm) * (h.1 - lm)).sum();
            let den: f64 = self.heard.iter().map(|h| (h.0 - tm).powi(2)).sum();
            if den > 0.0 { num / den } else { 0.0 }
        });
        json!({
            "sent": self.done,
            "heard": n,
            "heard_pct": 100.0 * n as f64 / f64::from(sent),
            "mean_ms": mean,
            "sigma_ms": sigma,
            "slope_ms_per_min": slope,
            "slope_valid": run_min >= 2.0,
        })
    }
}

fn quality_str(q: ClockQuality) -> &'static str {
    match q {
        ClockQuality::Acquiring => "acquiring",
        ClockQuality::Locked => "locked",
        ClockQuality::Degraded => "degraded",
        ClockQuality::Broken => "broken",
    }
}

fn drops_total(s: &Stats) -> u64 {
    let d = &s.drops;
    d.mix_cmds + d.events + d.in_events + d.pitch + d.out_stamps + d.mix_events + d.backend + d.tap
}

/// Runs the engine for `total_s` (as measured by `now`) and builds the report.
fn run_session<B: Backend>(
    backend: B,
    o: &Opts,
    total_s: f64,
    now: &dyn Fn() -> f64,
    soak: bool,
    fake: bool,
) -> Result<Value, AudioError> {
    let cfg = EngineConfig {
        open: o.open_request(),
        ..EngineConfig::default()
    };
    let (mut eng, mut io) =
        AudioEngine::start(backend, cfg, SoundBank::new(48_000.0), Pump::Thread)?;
    let info = eng.info().clone();
    let t0 = now();
    let el = || now() - t0;
    let (fs_in, fs_out) = (f64::from(info.fs_in), f64::from(info.fs_out));
    let mut acc = Acc::default();
    let mut prober = o.probe_every.map(|every| {
        io.tap.set(true);
        Prober::new(
            every,
            0.25 * if fake { o.speed.max(1.0) } else { 1.0 },
            fs_in,
            fs_out,
        )
    });
    if o.beep && prober.is_none() {
        // 0.5 s click, quiet.
        let at = (0.2 * fs_out) as u64;
        io.mix.send(MixCmd::Metronome {
            tag: 1,
            start: at,
            period: 0.5 * fs_out,
            accent_every: 0,
            beats: 0,
            gain: PROBE_GAIN,
        });
    }
    let mut next_progress = 60.0;
    loop {
        let t = el();
        if t >= total_s {
            break;
        }
        let alive = eng.poll();
        acc.drain(&mut io);
        if let Some(p) = prober.as_mut() {
            p.step(t, total_s, eng.stats().frames_out, &mut io, &acc);
        }
        if !alive {
            break;
        }
        if soak && t >= next_progress {
            let s = eng.stats();
            eprintln!(
                "{:.0} s: xruns {}/{} steps {}/{} drops {}",
                t,
                s.xruns_in,
                s.xruns_out,
                s.steps_in,
                s.steps_out,
                drops_total(&s)
            );
            next_progress += 60.0;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let elapsed = el();
    let mid = eng.stats();
    let stats = eng.stop();
    acc.drain(&mut io);
    let stats = if stats.callbacks_in >= mid.callbacks_in {
        stats
    } else {
        mid
    };

    let avg_in = stats.frames_in as f64 / stats.callbacks_in.max(1) as f64;
    let avg_out = stats.frames_out as f64 / stats.callbacks_out.max(1) as f64;
    let period_in_ms = avg_in / fs_in * 1e3;
    let period_out_ms = avg_out / fs_out * 1e3;
    let worst_in_ms = stats.worst_in_ns as f64 / 1e6;
    let worst_out_ms = stats.worst_out_ns as f64 / 1e6;
    let pct_in = 100.0 * worst_in_ms / period_in_ms.max(1e-9);
    let pct_out = 100.0 * worst_out_ms / period_out_ms.max(1e-9);
    let drops = drops_total(&stats);
    let run_min = elapsed / 60.0;
    let lp = prober.as_ref().map(|p| p.report(run_min));

    let streams_ok = stats.callbacks_in > 0
        && stats.callbacks_out > 0
        && !stats.device_lost
        && acc.stopped.is_none();
    let clock_ok = !matches!(stats.quality, ClockQuality::Degraded | ClockQuality::Broken);
    let mut exit = 0;
    if !streams_ok {
        exit = 12;
    } else if !clock_ok {
        exit = 14;
    }

    // Soak criteria (T10, T11). Callback timing is not judged on the fake backend
    // (it measures the host scheduler, not the engine).
    let timing_ok = (!fake).then_some(pct_in <= 25.0 && pct_out <= 25.0);
    let loop_checks = lp.as_ref().map(|l| {
        let heard_ok =
            l["heard_pct"].as_f64().unwrap_or(0.0) >= 95.0 && l["sent"].as_u64() > Some(0);
        let sigma_ok = l["sigma_ms"].as_f64().is_none_or(|s| s <= 8.0);
        let slope_ok = if l["slope_valid"].as_bool() == Some(true) {
            l["slope_ms_per_min"]
                .as_f64()
                .is_none_or(|s| s.abs() <= 0.1)
        } else {
            true
        };
        (heard_ok, sigma_ok, slope_ok)
    });
    let counters_ok = stats.xruns_in + stats.xruns_out == 0
        && stats.steps_in + stats.steps_out == 0
        && drops == 0
        && acc.late == 0
        && acc.rejected == 0;
    let pass = streams_ok
        && clock_ok
        && counters_ok
        && timing_ok.unwrap_or(true)
        && loop_checks.is_none_or(|(a, b, c)| a && b && c);
    if exit == 0 && soak && !pass {
        exit = 30;
    }

    Ok(json!({
        "ok": exit == 0,
        "exit": exit,
        "mode": if soak { "soak" } else { "probe" },
        "fake": fake,
        "beep": o.beep,
        "duration_s": elapsed,
        "devices": {"in": info.in_id, "out": info.out_id},
        "rates": {"in_hz": info.fs_in, "out_hz": info.fs_out,
                  "in_channels": info.ch_in, "out_channels": info.ch_out},
        "callbacks": {
            "in": stats.callbacks_in, "out": stats.callbacks_out,
            "frames_in": stats.frames_in, "frames_out": stats.frames_out,
            "avg_block_in": avg_in, "avg_block_out": avg_out,
            "period_in_ms": period_in_ms, "period_out_ms": period_out_ms,
            "worst_in_ms": worst_in_ms, "worst_out_ms": worst_out_ms,
            "worst_in_pct_of_period": pct_in, "worst_out_pct_of_period": pct_out,
            "skipped_in": stats.skipped_in, "skipped_out": stats.skipped_out,
            "nonfinite_in": stats.nonfinite_in, "nonfinite_out": stats.nonfinite_out,
        },
        "xruns": {"in": stats.xruns_in, "out": stats.xruns_out},
        "stamps": {
            "steps_in": stats.steps_in, "steps_out": stats.steps_out,
            "step_notes": acc.step_notes, "unjudged_windows": acc.unjudged,
            "backend_events": acc.backend_events, "device_lost": stats.device_lost,
        },
        "clock": {
            "quality": quality_str(stats.quality),
            "origin_offset_ns": info.origin_offset_ns,
            "drift_ppm_last": acc.snap.map(|s| s.drift_ppm()),
            "drift_ppm_min": acc.drift_min,
            "drift_ppm_max": acc.drift_max,
            "epoch": stats.epoch,
        },
        "drops": {
            "total": drops, "mix_cmds": stats.drops.mix_cmds, "events": stats.drops.events,
            "in_events": stats.drops.in_events, "pitch": stats.drops.pitch,
            "out_stamps": stats.drops.out_stamps, "mix_events": stats.drops.mix_events,
            "backend": stats.drops.backend, "tap": stats.drops.tap,
        },
        "mixer": {"late": acc.late, "rejected": acc.rejected},
        "loop": lp,
        "checks": {
            "streams": streams_ok, "clock": clock_ok, "counters_zero": counters_ok,
            "worst_callback_le_25pct": timing_ok,
            "loop_heard_ge_95pct": loop_checks.map(|c| c.0),
            "loop_sigma_le_8ms": loop_checks.map(|c| c.1),
            "loop_slope_le_0_1ms_per_min": loop_checks.map(|c| c.2),
            "pass": pass,
        },
    }))
}

fn session_cmd(soak: bool, o: &Opts) -> i32 {
    let total_s = if soak {
        o.minutes.unwrap_or(1.0) * 60.0
    } else {
        o.seconds
    };
    let result = if o.fake {
        let backend = FakeBackend::new(FakeConfig::default());
        let h = backend.handle();
        // The pump must not start before the engine opens the streams, but `start`
        // is called inside `run_session`; the pump thread just idles until then.
        let speed = o.speed;
        let rt = h.spawn_realtime(speed);
        let now = move || h.now_s();
        let r = run_session(backend, o, total_s, &now, soak, true);
        drop(rt);
        r
    } else {
        let start = Instant::now();
        let now = move || start.elapsed().as_secs_f64();
        run_session(CpalBackend::new(), o, total_s, &now, soak, false)
    };
    match result {
        Ok(v) => {
            let code = v["exit"].as_i64().unwrap_or(20) as i32;
            if o.json {
                println!("{v}");
            } else {
                print_text(&v);
            }
            code
        }
        Err(e) => fail(o.json, &e),
    }
}

fn print_text(v: &Value) {
    let verdict = if v["ok"] == true { "OK" } else { "FALHOU" };
    println!(
        "{} {}{}: {:.1} s, exit {}",
        v["mode"].as_str().unwrap_or("?"),
        verdict,
        if v["fake"] == true { " (fake)" } else { "" },
        v["duration_s"].as_f64().unwrap_or(0.0),
        v["exit"]
    );
    if let Some(m) = v.as_object() {
        for (k, val) in m {
            if matches!(k.as_str(), "ok" | "exit" | "mode" | "fake" | "duration_s") {
                continue;
            }
            match val {
                Value::Object(o) => {
                    println!("{k}:");
                    for (kk, vv) in o {
                        println!("  {kk}: {vv}");
                    }
                }
                Value::Null => {}
                other => println!("{k}: {other}"),
            }
        }
    }
}

// ---------------------------------------------------------------- record

/// `YYYY-MM-DD` (UTC).
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

fn host_name(fake: bool) -> &'static str {
    if fake {
        "fake"
    } else if cfg!(target_os = "macos") {
        "CoreAudio"
    } else if cfg!(target_os = "windows") {
        "WASAPI"
    } else {
        "ALSA"
    }
}

fn record_error(o: &Opts, code: i32, msg: &str) -> i32 {
    if o.json {
        println!("{}", json!({"ok": false, "error": msg, "exit": code}));
    } else {
        eprintln!("error: {msg} (exit {code})");
    }
    code
}

fn record_cmd(o: &Opts) -> i32 {
    let Some(folder) = o.folder.clone() else {
        eprintln!("error: record needs a FOLDER for the tapes\n\n{HELP}");
        return 2;
    };
    if o.chart.is_some() && o.click.is_some() {
        eprintln!("error: --chart and --click are exclusive (a chart brings its own tempo)");
        return 2;
    }
    if o.speed != 1.0 && !o.fake {
        eprintln!("error: --speed needs --fake");
        return 2;
    }
    let seconds = if o.seconds_given { o.seconds } else { 10.0 };
    let chart: Option<Chart> = match &o.chart {
        Some(p) => Some({
            let text = match std::fs::read_to_string(p) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("error: cannot read {}: {e}", p.display());
                    return 2;
                }
            };
            match Chart::from_json(&text) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {}: bad chart: {e}", p.display());
                    return 2;
                }
            }
        }),
        None => None,
    };
    if !o.fake && std::env::var_os("TONICA_FORBID_DEVICES").is_some() {
        return record_error(
            o,
            12,
            "real audio devices are forbidden (TONICA_FORBID_DEVICES is set)",
        );
    }
    let result = if o.fake {
        let backend = FakeBackend::new(FakeConfig::default());
        let h = backend.handle();
        let rt = h.spawn_realtime(o.speed);
        let now = move || h.now_s();
        let r = run_record(backend, o, &folder, chart, seconds, &now);
        drop(rt);
        r
    } else {
        let start = Instant::now();
        let now = move || start.elapsed().as_secs_f64();
        run_record(CpalBackend::new(), o, &folder, chart, seconds, &now)
    };
    match result {
        Ok(v) => {
            let code = v["exit"].as_i64().unwrap_or(20) as i32;
            if o.json {
                println!("{v}");
            } else if code == 0 {
                println!(
                    "gravado: {} ({:.1} s)",
                    v["tape"].as_str().unwrap_or("?"),
                    v["duration_s"].as_f64().unwrap_or(0.0)
                );
            } else {
                eprintln!(
                    "a gravacao nao ficou completa (exit {code}): {}",
                    v["tape"].as_str().unwrap_or("sem fita")
                );
            }
            code
        }
        Err(e) => fail(o.json, &e),
    }
}

fn run_record<B: Backend>(
    backend: B,
    o: &Opts,
    folder: &std::path::Path,
    chart: Option<Chart>,
    seconds: f64,
    now: &dyn Fn() -> f64,
) -> Result<Value, AudioError> {
    let fake = o.fake;
    let cfg = EngineConfig {
        open: o.open_request(),
        ..EngineConfig::default()
    };
    let (mut eng, io) = AudioEngine::start(backend, cfg, SoundBank::new(48_000.0), Pump::Thread)?;
    let info = eng.info().clone();
    let mut scfg = SessionConfig::new(host_name(fake), civil_date());
    scfg.tapes = Some(folder.to_path_buf());
    scfg.record = true;
    // Clicks are silent unless the player asked to hear them.
    scfg.click_gain = if o.beep { 0.7 } else { 0.0 };
    scfg.profiles = match &o.profile {
        Some(p) => Some(p.clone()),
        None if fake => None,
        None => default_profile_path(),
    };
    let (session, port) = Session::new(io, info.clone(), scfg);
    let thread = session.spawn(Duration::from_millis(5));
    let label = o.take.clone().unwrap_or_else(|| "take".to_string());
    match chart {
        // A real round: the judge runs on it.
        Some(chart) => {
            port.send(SessionCmd::StartRound {
                chart,
                lead_in_beats: if o.beep { 4 } else { 0 },
                take: Some(label.clone()),
            });
        }
        // No chart: nothing is judged. The take label files the tape and, with --click,
        // a plain click track runs under it.
        None => {
            port.send(SessionCmd::Take(label.clone()));
            if let Some(bpm) = o.click {
                // Start delay, count-in and a spare beat on top of the recording length.
                let beats = ((seconds + 6.0) * bpm / 60.0).ceil() as u32;
                port.send(SessionCmd::Click {
                    bpm,
                    beats,
                    accent_every: 4,
                });
            }
        }
    }

    let t0 = now();
    let mut acc = RecAcc::default();
    loop {
        let alive = eng.poll();
        while let Some(e) = port.try_recv() {
            acc.note(e);
        }
        if !alive || acc.paused || now() - t0 >= seconds {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let elapsed = now() - t0;
    // Stopping the session closes the tape.
    drop(thread.stop());
    while let Some(e) = port.try_recv() {
        acc.note(e);
    }
    let stats = eng.stop();
    let device_lost = stats.device_lost || acc.paused;
    let closed = acc.closed;
    let complete = closed.as_ref().is_some_and(|s| s.complete);
    let exit = if device_lost {
        12
    } else if !complete || acc.tape_failed.is_some() {
        20
    } else {
        0
    };
    Ok(json!({
        "ok": exit == 0,
        "exit": exit,
        "mode": "record",
        "fake": fake,
        "beep": o.beep,
        "duration_s": elapsed,
        "take": label,
        "tape": acc.tape_dir.as_ref().map(|d| d.display().to_string()),
        "audio": acc.tape_audio,
        "complete": complete,
        "mic_frames": closed.as_ref().map(|s| s.mic_frames),
        "lost_frames": closed.as_ref().map(|s| s.lost_frames),
        "events": closed.as_ref().map(|s| s.events),
        "judged_events": acc.judged,
        "rates": {"in_hz": info.fs_in, "out_hz": info.fs_out},
        "tape_error": acc.tape_failed,
        "refused": acc.refused,
    }))
}

#[derive(Default)]
struct RecAcc {
    tape_dir: Option<PathBuf>,
    tape_audio: bool,
    tape_failed: Option<String>,
    closed: Option<tonica_session::writer::TapeSummary>,
    paused: bool,
    judged: u64,
    refused: Vec<String>,
}

impl RecAcc {
    fn note(&mut self, e: SessionEvent) {
        match e {
            SessionEvent::Tape(TapeNote::Started { dir, audio }) => {
                self.tape_dir = Some(dir);
                self.tape_audio = audio;
            }
            SessionEvent::Tape(TapeNote::Closed(s)) => self.closed = Some(s),
            SessionEvent::Tape(TapeNote::Failed(m)) => self.tape_failed = Some(m),
            SessionEvent::Paused(_) => self.paused = true,
            SessionEvent::Judge(_) => self.judged += 1,
            SessionEvent::Notice(Notice::Refused(r)) => self.refused.push(format!("{r:?}")),
            _ => {}
        }
    }
}
