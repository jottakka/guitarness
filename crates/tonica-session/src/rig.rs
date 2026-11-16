//! Fake test bench with a virtual player (S4).
//!
//! [`FakeRig`] runs the real audio engine on the [`FakeBackend`] with the manual pump, so
//! virtual time moves only when the test says so. A virtual player ([`PlayerModel`])
//! strums along the clicks of a chart: the sounds come from the `tonica-sim` string and
//! strum models and enter the microphone through [`FakeHandle::play_external`]. For every
//! hit the rig returns the ground truth ([`TruthStrum`]): what the player did and when,
//! on the same frame counters the detector reports.
//!
//! Timing follows the player model of the calibration tests (`tonica-audio/tests/calib.rs`)
//! without changing its arithmetic: a click leaves the loudspeaker at `out_time_s(f)`,
//! reaches the microphone `latency` later, and the strum starts `tendency + error` after
//! that, minus the attack inside the sound:
//!
//! ```text
//! at = out_time_s(f) + latency + tendency + error − attack
//! ```
//!
//! Nothing here opens a real device: [`real_devices_allowed`] is the switch the tests of the
//! other crates and the CLI check before they would.

use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::sync::Arc;

use tonica_audio::{
    AudioEngine, ChartClock, EngineConfig, EngineIo, FakeBackend, FakeConfig, FakeHandle,
    FakeTruth, Pump, SoundBank,
};
use tonica_core::chart::{Chart, EventKind};
use tonica_dsp::synth::Mulberry32;
use tonica_sim::fx::rms;
use tonica_sim::strum::{Dir, StrumSpec, render_take, voicing};

/// Virtual time per step of the loop: one engine step every 5 ms.
pub const SLICE_S: f64 = 0.005;
/// Time inside every rendered sound at which the first string is struck.
const ATTACK_S: f64 = 0.02;
/// Sounds are queued until their time is this close, to stay under the fake's voice limit.
const LOOKAHEAD_S: f64 = 0.25;
/// A queued sound the fake refuses for this long after its time is dropped (and counted).
const STALE_S: f64 = 0.05;
/// Distinct rendered sounds per kind (muted, open); hit `i` plays variant `i % VARIANTS`.
const VARIANTS: usize = 4;
/// RMS of every rendered sound over the 50 ms after its attack (the calibration level).
const SOUND_RMS: f64 = 0.1;

/// Whether a test or tool may open a real audio device. False when the variable
/// `TONICA_FORBID_DEVICES` is set, whatever its value.
pub fn real_devices_allowed() -> bool {
    std::env::var_os("TONICA_FORBID_DEVICES").is_none()
}

/// What the chart expects at a hit, as far as the virtual player cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitKind {
    /// An open strum.
    Strum,
    /// A muted strum (a chuck).
    Mute,
    /// An air stroke: the hand moves, nothing sounds, the player never strums it.
    Ghost,
}

/// One expected hit: `t_s` seconds after chart time zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartHit {
    /// Hit number in chart order (the id of the verdict that will judge it).
    pub id: u32,
    /// Chart time, seconds.
    pub t_s: f64,
    /// Kind of the hit.
    pub kind: HitKind,
}

/// Something that lists the hits of a chart in time order.
pub trait HitSource {
    /// The hits, in chart order.
    fn hits(&self) -> Vec<ChartHit>;
}

impl HitSource for Chart {
    /// The played events of the first tier, in order. `id` is the index in
    /// [`Chart::events`] (rests keep their index, and are not hits). Single notes sound
    /// like open strums; mutes like chucks; ghosts are silent.
    fn hits(&self) -> Vec<ChartHit> {
        self.events()
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let kind = match e.kind {
                    EventKind::Strum { .. } | EventKind::Note { .. } => HitKind::Strum,
                    EventKind::Mute { .. } => HitKind::Mute,
                    EventKind::Ghost { .. } => HitKind::Ghost,
                    EventKind::Rest => return None,
                };
                Some(ChartHit {
                    id: i as u32,
                    t_s: self.seconds_of_tick(e.tick),
                    kind,
                })
            })
            .collect()
    }
}

impl HitSource for [ChartHit] {
    fn hits(&self) -> Vec<ChartHit> {
        self.to_vec()
    }
}

impl HitSource for Vec<ChartHit> {
    fn hits(&self) -> Vec<ChartHit> {
        self.clone()
    }
}

/// How the virtual player plays. Everything is drawn from `seed`: the same model on the
/// same hits gives the same sounds, and changing one rate never moves the other draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerModel {
    /// Mean of the timing error, ms after the click as heard. Negative is early.
    pub tendency_ms: f64,
    /// Standard deviation of the timing error, ms. The errors of one chart are the
    /// quantiles of the normal distribution in random order unless
    /// [`PlayerOptions::stratified`] is off, so the stated spread holds whatever the seed.
    pub sigma_ms: f64,
    /// Probability that a hit gets no strum at all (a missed beat), 0 to 1.
    pub miss: f64,
    /// Probability that a played hit gets a second strum 40 to 120 ms later (a double
    /// hit), 0 to 1.
    pub double: f64,
    /// Seed of every draw.
    pub seed: u32,
}

impl Default for PlayerModel {
    /// A player who strums on the click, exactly, and never misses.
    fn default() -> Self {
        Self {
            tendency_ms: 0.0,
            sigma_ms: 0.0,
            miss: 0.0,
            double: 0.0,
            seed: 1,
        }
    }
}

/// Less common knobs of the virtual player.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerOptions {
    /// Probability that a played open strum is muted instead, 0 to 1.
    pub muted: f64,
    /// Stratified timing errors (see [`PlayerModel::sigma_ms`]); false draws them
    /// independently.
    pub stratified: bool,
    /// The first `skip` hits get no strum (a late start).
    pub skip: u32,
}

impl Default for PlayerOptions {
    fn default() -> Self {
        Self {
            muted: 0.0,
            stratified: true,
            skip: 0,
        }
    }
}

/// What the virtual player did at one hit (or on top of it), on the true clocks.
///
/// Times in input frames are nominal frames counted on the input stream, the same counter as
/// `OnsetNote::t_in` and `ClockSnapshot::in_of_out`.
#[derive(Clone, Debug, PartialEq)]
pub struct TruthStrum {
    /// Id of the hit; `None` for the second strum of a double hit.
    pub id: Option<u32>,
    /// Kind of the hit it belongs to.
    pub kind: HitKind,
    /// Output frame of the click, `anchor + t_s · fs_out`, rounded.
    pub out_frame: u64,
    /// True if the player strummed at all (false: missed or skipped).
    pub played: bool,
    /// True for the second strum of a double hit.
    pub extra: bool,
    /// True if the strum sounds muted.
    pub muted: bool,
    /// Input frame the click is captured at: `in_of_out(out_frame)`, the reference `g_k`.
    pub g_in: f64,
    /// Input frame at which the click arrives at the microphone through the path.
    pub arrival_in: f64,
    /// True input frame of the onset of the strum; `None` if not played.
    pub onset_in: Option<f64>,
    /// True error: `(onset_in − arrival_in)` in ms of nominal input frames, that is
    /// tendency plus draw (plus the second-strum gap for an extra). Negative is early.
    pub dt_ms: Option<f64>,
}

/// One strum waiting for its time.
struct Pending {
    at_s: f64,
    samples: Arc<[f32]>,
}

/// The rendered sounds of a virtual player at one input rate.
struct Sounds {
    muted: Vec<Arc<[f32]>>,
    open: Vec<Arc<[f32]>>,
}

impl Sounds {
    fn render(fs_in: f64) -> Self {
        let make = |muted: bool| {
            let mut rng = Mulberry32::new(if muted { 77 } else { 91 });
            (0..VARIANTS)
                .map(|k| {
                    let spec = StrumSpec {
                        t0: ATTACK_S,
                        voicing: voicing(["E", "A", "G", "D"][k]),
                        dir: if k % 2 == 0 { Dir::Down } else { Dir::Up },
                        spread: 0.006 + 0.003 * k as f64,
                        gain: 1.0,
                        muted,
                        strings: [true; 6],
                        attack: 0.0005,
                    };
                    let mut audio =
                        render_take(fs_in, &[spec], (0.35 * fs_in) as usize, &mut rng).audio;
                    let a = (ATTACK_S * fs_in) as usize;
                    let level = rms(&audio[a..a + (0.05 * fs_in) as usize]).max(1.0e-9);
                    audio
                        .iter_mut()
                        .for_each(|v| *v = (f64::from(*v) * SOUND_RMS / level) as f32);
                    Arc::from(audio)
                })
                .collect()
        };
        Self {
            muted: make(true),
            open: make(false),
        }
    }
}

/// Inverse of the standard normal distribution (P. J. Acklam's rational approximation).
fn probit(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    let tail = |q: f64| {
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    const LOW: f64 = 0.024_25;
    if p < LOW {
        tail((-2.0 * p.ln()).sqrt())
    } else if p > 1.0 - LOW {
        -tail((-2.0 * (1.0 - p).ln()).sqrt())
    } else {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    }
}

fn gauss(r: &mut Mulberry32) -> f64 {
    let u1 = r.next_f64().max(1.0e-12);
    let u2 = r.next_f64();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

/// `n` timing errors of standard deviation `sigma_s`: the `n` quantiles of the normal
/// distribution in a random order (`stratified`), or independent draws.
fn timing_errors(n: usize, sigma_s: f64, seed: u32, stratified: bool) -> Vec<f64> {
    let mut rng = Mulberry32::new(seed);
    if !stratified {
        return (0..n).map(|_| sigma_s * gauss(&mut rng)).collect();
    }
    let mut z: Vec<f64> = (0..n)
        .map(|i| sigma_s * probit((i as f64 + 0.5) / n as f64))
        .collect();
    for i in (1..n).rev() {
        let j = (rng.next_f64() * (i + 1) as f64) as usize;
        z.swap(i, j.min(i));
    }
    z
}

/// The real audio engine on the fake backend, driven in virtual time, with a virtual player.
///
/// The fields are public: the session takes `io`, the tests read `handle` and `engine`.
pub struct FakeRig {
    /// Control of the fake devices (virtual clock, truth, injected faults).
    pub handle: FakeHandle,
    /// The engine, in `Pump::Manual`.
    pub engine: AudioEngine<FakeBackend>,
    /// What the engine gives its client.
    pub io: EngineIo,
    pending: VecDeque<Pending>,
    sounds: Sounds,
    dropped: u32,
}

impl FakeRig {
    /// Starts the engine on a fake backend built from `fc`, default engine settings.
    ///
    /// # Panics
    /// If the engine cannot start (the configuration is invalid).
    pub fn new(fc: FakeConfig) -> Self {
        Self::with_engine(fc, EngineConfig::default())
    }

    /// Like [`FakeRig::new`] with engine settings.
    pub fn with_engine(fc: FakeConfig, ec: EngineConfig) -> Self {
        let fs_in = f64::from(fc.fs_in);
        let fs_out = f64::from(fc.fs_out);
        let backend = FakeBackend::new(fc);
        let handle = backend.handle();
        let (engine, io) = AudioEngine::start(backend, ec, SoundBank::new(fs_out), Pump::Manual)
            .expect("the fake streams open");
        Self {
            handle,
            engine,
            io,
            pending: VecDeque::new(),
            sounds: Sounds::render(fs_in),
            dropped: 0,
        }
    }

    /// Virtual time since the streams opened, seconds.
    pub fn now_s(&self) -> f64 {
        self.handle.now_s()
    }

    /// The exact frame/time relations of the open streams.
    pub fn truth(&self) -> Option<FakeTruth> {
        self.handle.truth()
    }

    /// Strums the fake refused (too late or no free voice) and the rig gave up on.
    pub fn dropped_strums(&self) -> u32 {
        self.dropped
    }

    /// Strums still waiting for their time.
    pub fn pending_strums(&self) -> usize {
        self.pending.len()
    }

    /// Moves virtual time forward by `seconds`, in steps of 5 ms: queued strums go into the
    /// microphone as their time comes, the devices run and the engine steps. Events are left
    /// in `io` for the caller.
    pub fn advance(&mut self, seconds: f64) {
        let end = self.handle.now_s() + seconds;
        loop {
            let left = end - self.handle.now_s();
            if left <= 1.0e-12 {
                break;
            }
            self.release_due();
            self.handle.pump(left.min(SLICE_S));
            self.engine.pump();
        }
    }

    /// Hands the strums that are close enough to the fake.
    fn release_due(&mut self) {
        let now = self.handle.now_s();
        while let Some(front) = self.pending.front() {
            if front.at_s > now + LOOKAHEAD_S {
                break;
            }
            if self
                .handle
                .play_external(front.at_s, Arc::clone(&front.samples), 1.0)
                .is_some()
            {
                self.pending.pop_front();
            } else if front.at_s < now - STALE_S {
                self.pending.pop_front();
                self.dropped += 1;
            } else {
                break;
            }
        }
    }

    /// Plays the chart's hits with the virtual player and returns the truth, one entry per
    /// hit (played or not) followed in time order by the extra strums of double hits.
    ///
    /// `anchor` is the output frame of chart time zero. It must lie far enough ahead for the
    /// first strum to be after "now". The truth is read once, at this call: a path latency
    /// or an xrun injected later is not in it.
    pub fn schedule_player(
        &mut self,
        anchor: u64,
        chart: &(impl HitSource + ?Sized),
        p: &PlayerModel,
    ) -> Vec<TruthStrum> {
        self.schedule_player_with(anchor, chart, p, &PlayerOptions::default())
    }

    /// [`FakeRig::schedule_player`] with the less common knobs.
    pub fn schedule_player_with(
        &mut self,
        anchor: u64,
        chart: &(impl HitSource + ?Sized),
        p: &PlayerModel,
        o: &PlayerOptions,
    ) -> Vec<TruthStrum> {
        let hits = chart.hits();
        let Some(truth) = self.handle.truth() else {
            return Vec::new();
        };
        let clock = ChartClock::new(anchor, truth.fs_out);
        let errors = timing_errors(hits.len().max(1), p.sigma_ms / 1.0e3, p.seed, o.stratified);
        // A second stream for the yes/no draws, so no rate moves the timing errors.
        let mut rng = Mulberry32::new(p.seed ^ 0x9e37_79b9);
        let mut out = Vec::with_capacity(hits.len());
        let mut extras = Vec::new();
        for (i, hit) in hits.iter().enumerate() {
            let (u_miss, u_double, u_mute, u_gap) = (
                rng.next_f64(),
                rng.next_f64(),
                rng.next_f64(),
                rng.next_f64(),
            );
            let f = clock.frame_of_seconds(hit.t_s);
            let frame = f.round().max(0.0) as u64;
            let f = frame as f64;
            let g_in = truth.in_of_out(f);
            let arrival_in = truth.arrival_in(f);
            let played = (i as u32) >= o.skip && u_miss >= p.miss && hit.kind != HitKind::Ghost;
            let muted = matches!(hit.kind, HitKind::Mute | HitKind::Ghost) || u_mute < o.muted;
            let mut entry = TruthStrum {
                id: Some(hit.id),
                kind: hit.kind,
                out_frame: frame,
                played,
                extra: false,
                muted,
                g_in,
                arrival_in,
                onset_in: None,
                dt_ms: None,
            };
            if played {
                let error_s = p.tendency_ms / 1.0e3 + errors[i];
                self.queue(&truth, &mut entry, error_s, i);
                if u_double < p.double {
                    let gap_s = 0.04 + 0.08 * u_gap;
                    let mut extra = TruthStrum {
                        id: None,
                        extra: true,
                        ..entry.clone()
                    };
                    self.queue(&truth, &mut extra, error_s + gap_s, i + 1);
                    extras.push(extra);
                }
            }
            out.push(entry);
        }
        extras.sort_by(|a, b| a.onset_in.partial_cmp(&b.onset_in).expect("finite"));
        out.extend(extras);
        let mut v: Vec<Pending> = self.pending.drain(..).collect();
        v.sort_by(|a, b| a.at_s.partial_cmp(&b.at_s).expect("finite"));
        self.pending = v.into();
        out
    }

    /// Queues the strum of `entry`, `error_s` after its click as heard, and fills in the truth.
    fn queue(&mut self, truth: &FakeTruth, entry: &mut TruthStrum, error_s: f64, variant: usize) {
        let f = entry.out_frame as f64;
        let arrival_s = truth.out_time_s(f) + truth.latency_s;
        let at_s = arrival_s + error_s - ATTACK_S;
        let bank = if entry.muted {
            &self.sounds.muted
        } else {
            &self.sounds.open
        };
        let samples = Arc::clone(&bank[variant % VARIANTS]);
        let onset_in = truth.in_frame_at(arrival_s + error_s);
        entry.onset_in = Some(onset_in);
        entry.dt_ms = Some((onset_in - entry.arrival_in) / truth.fs_in * 1.0e3);
        self.pending.push_back(Pending { at_s, samples });
    }

    /// Makes the WAV file the microphone: the path is switched off (gain 0) and the samples,
    /// mono at the input rate, are added to the microphone from the next input block on.
    /// Noise configured in the [`FakeConfig`] stays; use zero noise for a bit-exact copy.
    /// 16-bit and float files are read; a stereo file gives its first channel.
    ///
    /// # Errors
    /// If the file cannot be read, or its rate differs from the input rate.
    pub fn feed_wav(&mut self, path: &Path) -> io::Result<()> {
        let bad = |e: hound::Error| io::Error::new(io::ErrorKind::InvalidData, e);
        let mut reader = hound::WavReader::open(path).map_err(bad)?;
        let spec = reader.spec();
        let fs_in = self
            .handle
            .truth()
            .map(|t| t.fs_in)
            .ok_or_else(|| io::Error::other("the streams are closed"))?;
        if f64::from(spec.sample_rate) != fs_in {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("WAV at {} Hz, input at {fs_in} Hz", spec.sample_rate),
            ));
        }
        let ch = usize::from(spec.channels.max(1));
        let all: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => reader
                .samples::<f32>()
                .collect::<Result<_, _>>()
                .map_err(bad)?,
            hound::SampleFormat::Int => {
                let scale = (1u64 << (spec.bits_per_sample - 1)) as f32;
                reader
                    .samples::<i32>()
                    .map(|s| s.map(|v| v as f32 / scale))
                    .collect::<Result<_, _>>()
                    .map_err(bad)?
            }
        };
        let samples: Vec<f32> = all.into_iter().step_by(ch).collect();
        let mut start: Option<u64> = None;
        self.handle.set_path_gain(0.0);
        self.handle
            .set_external_fn(Some(Box::new(move |q0: u64, mic: &mut [f32]| {
                let s = *start.get_or_insert(q0);
                let from = (q0 - s) as usize;
                if from >= samples.len() {
                    return;
                }
                let n = mic.len().min(samples.len() - from);
                for (m, x) in mic[..n].iter_mut().zip(&samples[from..from + n]) {
                    *m += *x;
                }
            })));
        Ok(())
    }
}
