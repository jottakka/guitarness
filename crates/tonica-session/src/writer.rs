//! Tape writer (owned by S1).
//!
//! One thread, `tonica-tape`, does all the disk work: it receives [`TapeEvent`]s over a
//! channel, empties the raw ring ([`RawOutput`]) every 10 ms into `mic.wav`, and every
//! 0.5 s flushes `events.jsonl` and the WAV and rewrites `mic.runs.json` and `tape.json`
//! (through a temporary file and a rename). Nothing here is called from an audio callback:
//! [`TapeWriter::event`] is for the session thread, and the raw ring is filled by the
//! input callback with plain memory writes.
//!
//! The channel is unbounded on purpose: dropping an event would make the tape lie, and a
//! stalled disk is better shown as a growing queue (a few hundred bytes per event) than
//! as a hole. [`TapeWriter::failed`] tells the session when an I/O error stopped the
//! tape.
//!
//! The caller switches the raw ring on (`TapHandle::set_raw(true)` before the first
//! callback that should be taped) and passes its consumer (`TapHandle::take_raw()`) to
//! [`TapeWriter::create`]. With `None` the tape has events only. A tape is opt-in
//! ([`crate::format::TAPES_DEFAULT_ON`] is `false`).

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tonica_audio::EngineEvent;
use tonica_audio::capture::RawOutput;
use tonica_audio::stats::Ring;

use tonica_core::verdict::{JudgeOut, Why};

use crate::format::{
    MicRun, TakeSpan, TapeCounts, TapeEvent, TapeHeader, event_line, header_to_text, runs_to_text,
};

/// How often the thread empties the raw ring.
const DRAIN_EVERY: Duration = Duration::from_millis(10);
/// How often the files are flushed and the small files rewritten.
const FLUSH_EVERY: Duration = Duration::from_millis(500);
/// Events the queue holds before [`TapeWriter::event`] gives up (and flags the tape).
pub const QUEUE_CAP: usize = 65_536;
/// Messages handled per turn of the thread loop, so a flood cannot starve the raw ring.
const MSG_BUDGET: usize = 1024;
/// A gap larger than this many seconds of input is not zero-filled: the audio stops.
const MAX_FILL_S: f64 = 600.0;

/// What a closed tape contains.
#[derive(Clone, Debug, PartialEq)]
pub struct TapeSummary {
    /// The tape folder.
    pub dir: PathBuf,
    /// Lines written to `events.jsonl`.
    pub events: u64,
    /// Samples in `mic.wav`.
    pub mic_frames: u64,
    /// Of those, zero-filled (lost).
    pub lost_frames: u64,
    /// Raw blocks the ring dropped.
    pub dropped_raw_blocks: u64,
    /// A contested verdict is on the tape.
    pub disputed: bool,
    /// Closed cleanly with nothing lost (`tape.json` says `complete`).
    pub complete: bool,
    /// Total size of the folder, bytes.
    pub bytes: u64,
}

enum Msg {
    Event(TapeEvent),
    Header(Box<dyn FnOnce(&mut TapeHeader) + Send>),
    Finish,
}

/// The raw ring a closed tape gave back, for the next tape of the same tap.
pub struct KeptRaw {
    raw: RawOutput,
}

impl KeptRaw {
    /// Throws away every block the ring still holds (audio from between two tapes).
    pub fn discard_pending(&mut self) {
        let mut buf = Vec::new();
        while self.raw.read_block(&mut buf).is_some() {}
    }
}

/// Writes one tape on its own thread.
pub struct TapeWriter {
    dir: PathBuf,
    tx: Option<SyncSender<Msg>>,
    overflow: Arc<AtomicBool>,
    join: Option<JoinHandle<(io::Result<TapeSummary>, Option<RawOutput>)>>,
    failed: Arc<AtomicBool>,
}

impl TapeWriter {
    /// Opens a tape in `dir` (created if needed; it must not hold a tape already) and
    /// starts the thread. `header` is written at once with `complete = false`; `raw` is
    /// the consumer of the raw ring, or `None` for a tape without audio.
    pub fn create(dir: &Path, header: TapeHeader, raw: Option<RawOutput>) -> io::Result<Self> {
        Self::create_with_queue(dir, header, raw, QUEUE_CAP)
    }

    /// As [`TapeWriter::create`] with an event queue of `queue` slots. When the queue is
    /// full [`TapeWriter::event`] drops the event, sets [`TapeWriter::overflowed`] and the
    /// tape ends incomplete.
    pub fn create_with_queue(
        dir: &Path,
        header: TapeHeader,
        raw: Option<RawOutput>,
        queue: usize,
    ) -> io::Result<Self> {
        Self::create_inner(dir, header, raw.map(|r| (r, 0)), queue)
    }

    /// As [`TapeWriter::create`] with the raw ring an earlier tape gave back
    /// ([`TapeWriter::finish_keep_raw`]). Blocks the ring dropped before (during the earlier
    /// tape) are not this tape's.
    pub fn create_with_kept(dir: &Path, header: TapeHeader, kept: KeptRaw) -> io::Result<Self> {
        let dropped0 = kept.raw.dropped_blocks();
        Self::create_inner(dir, header, Some((kept.raw, dropped0)), QUEUE_CAP)
    }

    fn create_inner(
        dir: &Path,
        header: TapeHeader,
        raw: Option<(RawOutput, u64)>,
        queue: usize,
    ) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        // `create_new`: never overwrite the events of another tape.
        let events = File::options()
            .write(true)
            .create_new(true)
            .open(dir.join("events.jsonl"))?;
        let mut header = header;
        header.complete = false;
        header.counts = TapeCounts::default();
        header.takes.clear();
        header.has_audio = raw.is_some();
        let audio = match raw {
            Some((raw, dropped0)) => Some(Audio::open(dir, header.fs_in, raw, dropped0)?),
            None => None,
        };
        let mut st = State {
            dir: dir.to_path_buf(),
            header,
            events: BufWriter::new(events),
            seq: 0,
            audio,
            err: None,
            skipped: 0,
            lossy: false,
            overflow: Arc::new(AtomicBool::new(false)),
        };
        st.write_small_files(false)?;
        let overflow = st.overflow.clone();
        let failed = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel::<Msg>(queue.max(1));
        let flag = failed.clone();
        let join = std::thread::Builder::new()
            .name("tonica-tape".into())
            .spawn(move || run(st, rx, &flag))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            tx: Some(tx),
            overflow,
            join: Some(join),
            failed,
        })
    }

    /// The tape folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Queues one event. Never blocks; not for audio callbacks.
    pub fn event(&self, ev: TapeEvent) {
        if let Some(tx) = &self.tx
            && let Err(TrySendError::Full(_)) = tx.try_send(Msg::Event(ev))
        {
            self.overflow.store(true, Ordering::Relaxed);
        }
    }

    /// True if the queue was ever full and an event was dropped: the tape is incomplete.
    pub fn overflowed(&self) -> bool {
        self.overflow.load(Ordering::Relaxed)
    }

    /// Changes the header before the tape is closed (a new profile, the video offset, a
    /// new gate table). The change is written at the next flush.
    pub fn update_header(&self, f: impl FnOnce(&mut TapeHeader) + Send + 'static) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Msg::Header(Box::new(f)));
        }
    }

    /// True once an I/O error stopped the tape (the files so far stay readable).
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// Closes the tape: drains what is left, flushes, finalises the WAV, writes the
    /// final `tape.json` (`complete` only if nothing was lost) and joins the thread.
    /// Returns the first I/O error the tape met, if any.
    pub fn finish(mut self) -> io::Result<TapeSummary> {
        self.close().0
    }

    /// As [`TapeWriter::finish`], and gives back the consumer of the raw ring (`None` for a
    /// tape without audio), so the next tape of the same tap can have audio too
    /// ([`TapeWriter::create_with_kept`]). The caller switches the ring off first.
    pub fn finish_keep_raw(mut self) -> (io::Result<TapeSummary>, Option<KeptRaw>) {
        let (res, raw) = self.close();
        (res, raw.map(|raw| KeptRaw { raw }))
    }

    fn close(&mut self) -> (io::Result<TapeSummary>, Option<RawOutput>) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Msg::Finish);
        }
        match self.join.take() {
            Some(j) => j
                .join()
                .unwrap_or_else(|_| (Err(io::Error::other("tape thread panicked")), None)),
            None => (Err(io::Error::other("tape already closed")), None),
        }
    }
}

impl Drop for TapeWriter {
    fn drop(&mut self) {
        if self.join.is_some() {
            let _ = self.close();
        }
    }
}

/// The audio half: `mic.wav` and the runs.
struct Audio {
    raw: RawOutput,
    /// Of `raw.dropped_blocks()` (the ring's own count), those that came before this tape:
    /// 0 for a fresh ring, the count at the start for a ring an earlier tape gave back.
    dropped0: u64,
    wav: Option<hound::WavWriter<BufWriter<File>>>,
    fs_in: u32,
    runs: Vec<MicRun>,
    /// Detector time of the next expected sample.
    next_det: Option<u64>,
    frames: u64,
    lost_frames: u64,
    /// Something happened that makes the tape incomplete.
    lossy: bool,
    buf: Vec<f32>,
}

impl Audio {
    fn open(dir: &Path, fs_in: u32, raw: RawOutput, dropped0: u64) -> io::Result<Self> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: fs_in,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(dir.join("mic.wav"))?;
        let wav = hound::WavWriter::new(BufWriter::new(file), spec).map_err(hound_err)?;
        Ok(Self {
            dropped0,
            raw,
            wav: Some(wav),
            fs_in,
            runs: Vec::new(),
            next_det: None,
            frames: 0,
            lost_frames: 0,
            lossy: false,
            buf: Vec::new(),
        })
    }

    /// Raw blocks dropped since this tape took the ring.
    fn dropped(&self) -> u64 {
        self.raw.dropped_blocks().saturating_sub(self.dropped0)
    }

    fn push_samples(&mut self, s: &[f32]) -> io::Result<()> {
        if let Some(w) = self.wav.as_mut() {
            for &x in s {
                w.write_sample(x).map_err(hound_err)?;
            }
        }
        self.frames += s.len() as u64;
        Ok(())
    }

    fn push_zeros(&mut self, n: u64) -> io::Result<()> {
        let zeros = [0.0f32; 4096];
        let mut left = n;
        while left > 0 {
            let k = left.min(zeros.len() as u64) as usize;
            self.push_samples(&zeros[..k])?;
            left -= k as u64;
        }
        Ok(())
    }

    /// Takes every whole block the ring has.
    fn drain(&mut self) -> io::Result<()> {
        let mut buf = std::mem::take(&mut self.buf);
        let res = self.drain_with(&mut buf);
        self.buf = buf;
        res
    }

    fn drain_with(&mut self, buf: &mut Vec<f32>) -> io::Result<()> {
        while let Some(mark) = self.raw.read_block(buf) {
            if self.lossy && self.next_det.is_none() {
                continue; // the audio was stopped
            }
            let n = buf.len() as u64;
            match self.next_det {
                Some(expect) if mark.det_first < expect => {
                    // The detector clock went back (a reset): contiguity is gone. Stop the
                    // audio here, keep what exists, and let the tape say it is incomplete.
                    self.lossy = true;
                    self.next_det = None;
                    continue;
                }
                Some(expect) if mark.det_first > expect => {
                    let gap = mark.det_first - expect;
                    if gap as f64 > MAX_FILL_S * f64::from(self.fs_in) {
                        self.lossy = true;
                        self.next_det = None;
                        continue;
                    }
                    self.lossy = true;
                    self.push_zeros(gap)?;
                    self.lost_frames += gap;
                    self.runs.push(MicRun {
                        det_first: expect,
                        in_first: mark.in_first.saturating_sub(gap),
                        frames: gap,
                        lost: true,
                    });
                }
                _ => {}
            }
            let offset = i128::from(mark.in_first) - i128::from(mark.det_first);
            match self.runs.last_mut() {
                Some(last)
                    if !last.lost
                        && i128::from(last.in_first) - i128::from(last.det_first) == offset
                        && last.det_first + last.frames == mark.det_first =>
                {
                    last.frames += n;
                }
                _ => self.runs.push(MicRun {
                    det_first: mark.det_first,
                    in_first: mark.in_first,
                    frames: n,
                    lost: false,
                }),
            }
            self.push_samples(buf)?;
            self.next_det = Some(mark.det_first + n);
        }
        Ok(())
    }
}

fn hound_err(e: hound::Error) -> io::Error {
    match e {
        hound::Error::IoError(e) => e,
        other => io::Error::other(other.to_string()),
    }
}

struct State {
    dir: PathBuf,
    header: TapeHeader,
    events: BufWriter<File>,
    seq: u64,
    audio: Option<Audio>,
    /// First I/O error; after it nothing more is written.
    err: Option<io::Error>,
    skipped: u64,
    /// Something that replay needs was lost (a dropped queue item, a full event queue).
    lossy: bool,
    overflow: Arc<AtomicBool>,
}

/// Writes `text` to `path` through a temporary file and a rename. With `durable` the
/// temporary file is synced before the rename and the folder after it (where the platform
/// lets a folder be opened: not on Windows, where the rename itself is journaled).
fn write_atomic(path: &Path, text: &str, durable: bool) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        if durable {
            f.sync_all()?;
        }
    }
    fs::rename(&tmp, path)?;
    if durable {
        sync_dir(path.parent());
    }
    Ok(())
}

fn sync_dir(dir: Option<&Path>) {
    #[cfg(unix)]
    if let Some(d) = dir
        && let Ok(f) = File::open(d)
    {
        let _ = f.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn sync_path(path: &Path) -> io::Result<()> {
    File::options().write(true).open(path)?.sync_all()
}

/// Rings whose loss changes what a replay sees. Tuner frames and the raw tap do not.
fn replay_ring(r: Ring) -> bool {
    !matches!(r, Ring::Pitch | Ring::Tap)
}

impl State {
    fn event(&mut self, ev: &TapeEvent) -> io::Result<()> {
        let seq = self.seq;
        let Some(mut line) = event_line(seq, ev) else {
            self.skipped += 1;
            return Ok(());
        };
        line.push('\n');
        self.events.write_all(line.as_bytes())?;
        self.seq += 1;
        if let TapeEvent::Engine(EngineEvent::Dropped { ring, n }) = ev
            && *n > 0
            && replay_ring(*ring)
        {
            self.lossy = true;
        }
        match ev {
            TapeEvent::Take { label } => self.header.takes.push(TakeSpan {
                label: label.clone(),
                seq_first: seq,
                seq_last: seq,
            }),
            other => {
                if let Some(t) = self.header.takes.last_mut() {
                    t.seq_last = seq;
                }
                // Disputed when a verdict was taken back for it, not for the command
                // alone (a dispute with nothing to withdraw changes nothing).
                if matches!(
                    other,
                    TapeEvent::Out(JudgeOut::Withdrawn {
                        why: Why::Disputed,
                        ..
                    })
                ) {
                    self.header.disputed = true;
                }
            }
        }
        Ok(())
    }

    fn counts(&self) -> TapeCounts {
        let (mic_frames, lost_frames, dropped) = self
            .audio
            .as_ref()
            .map_or((0, 0, 0), |a| (a.frames, a.lost_frames, a.dropped()));
        TapeCounts {
            events: self.seq,
            mic_frames,
            lost_frames,
            dropped_raw_blocks: dropped,
            skipped: self.skipped,
        }
    }

    fn write_small_files(&mut self, durable: bool) -> io::Result<()> {
        self.header.counts = self.counts();
        if let Some(a) = &self.audio {
            write_atomic(
                &self.dir.join("mic.runs.json"),
                &runs_to_text(a.fs_in, &a.runs),
                durable,
            )?;
        }
        write_atomic(
            &self.dir.join("tape.json"),
            &header_to_text(&self.header),
            durable,
        )
    }

    /// Drain, flush, rewrite.
    fn flush(&mut self) -> io::Result<()> {
        if let Some(a) = self.audio.as_mut() {
            a.drain()?;
            if let Some(w) = a.wav.as_mut() {
                w.flush().map_err(hound_err)?;
            }
        }
        self.events.flush()?;
        self.write_small_files(false)
    }

    fn finalize(&mut self) -> io::Result<TapeSummary> {
        if let Some(e) = self.err.take() {
            // Leave what is on disk: `complete` stays false. Best effort to close the WAV.
            if let Some(w) = self.audio.as_mut().and_then(|a| a.wav.take()) {
                let _ = w.finalize();
            }
            let _ = self.write_small_files(false);
            return Err(e);
        }
        if let Some(a) = self.audio.as_mut() {
            a.drain()?;
        }
        // Order: events and audio reach the disk, then the runs, and only then the header
        // that says `complete`.
        self.events.flush()?;
        self.events.get_ref().sync_all()?;
        if let Some(w) = self.audio.as_mut().and_then(|a| a.wav.take()) {
            w.finalize().map_err(hound_err)?;
            sync_path(&self.dir.join("mic.wav"))?;
        }
        let counts = self.counts();
        let lossy = self.lossy
            || self.overflow.load(Ordering::Relaxed)
            || self
                .audio
                .as_ref()
                .is_some_and(|a| a.lossy || a.dropped() > 0);
        self.header.complete = !lossy;
        self.write_small_files(true)?;
        Ok(TapeSummary {
            dir: self.dir.clone(),
            events: counts.events,
            mic_frames: counts.mic_frames,
            lost_frames: counts.lost_frames,
            dropped_raw_blocks: counts.dropped_raw_blocks,
            disputed: self.header.disputed,
            complete: self.header.complete,
            bytes: dir_size(&self.dir),
        })
    }
}

/// Total size of the regular files of `dir`, bytes.
pub(crate) fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else {
        return 0;
    };
    rd.filter_map(Result::ok)
        .map(|e| match e.metadata() {
            Ok(m) if m.is_file() => m.len(),
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            _ => 0,
        })
        .sum()
}

fn run(
    mut st: State,
    rx: mpsc::Receiver<Msg>,
    failed: &AtomicBool,
) -> (io::Result<TapeSummary>, Option<RawOutput>) {
    let mut last_flush = Instant::now();
    loop {
        let mut finish = false;
        match rx.recv_timeout(DRAIN_EVERY) {
            Ok(m) => {
                finish |= handle(&mut st, m, failed);
                let mut n = 1;
                while !finish && n < MSG_BUDGET {
                    let Ok(m) = rx.try_recv() else { break };
                    finish |= handle(&mut st, m, failed);
                    n += 1;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => finish = true,
        }
        if finish {
            break;
        }
        if st.err.is_none()
            && let Some(a) = st.audio.as_mut()
            && let Err(e) = a.drain()
        {
            fail(&mut st, e, failed);
        }
        if last_flush.elapsed() >= FLUSH_EVERY {
            last_flush = Instant::now();
            if st.err.is_none()
                && let Err(e) = st.flush()
            {
                fail(&mut st, e, failed);
            }
        }
    }
    let res = st.finalize();
    (res, st.audio.take().map(|a| a.raw))
}

fn fail(st: &mut State, e: io::Error, failed: &AtomicBool) {
    if st.err.is_none() {
        st.err = Some(e);
    }
    failed.store(true, Ordering::Relaxed);
}

/// Returns true for `Finish`.
fn handle(st: &mut State, m: Msg, failed: &AtomicBool) -> bool {
    match m {
        Msg::Finish => true,
        Msg::Header(f) => {
            f(&mut st.header);
            false
        }
        Msg::Event(ev) => {
            if st.err.is_none() {
                if let TapeEvent::Engine(EngineEvent::Pitch(_)) = ev {
                    st.skipped += 1;
                } else if let Err(e) = st.event(&ev) {
                    fail(st, e, failed);
                }
            }
            false
        }
    }
}
