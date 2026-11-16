//! Tape reader (owned by S1).
//!
//! [`Tape::open`] reads `tape.json`, `events.jsonl` and `mic.runs.json` into memory and
//! leaves `mic.wav` on disk; [`Tape::audio`] and friends read windows of it.
//!
//! A tape that was not closed (the app died) opens all the same: `complete` is `false`,
//! the last `events.jsonl` line may be cut (counted in [`Tape::bad_lines`]), the WAV
//! length comes from the file size (its header may be stale), and the takes and the
//! `disputed` flag are rebuilt from the lines.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::format::SessionCmdLog;
use crate::format::{
    Decoded, MicRun, TakeSpan, TapeError, TapeEvent, TapeHeader, decode_line, header_from_text,
    runs_from_text,
};
use crate::writer::dir_size;

/// Longest window [`Tape::audio`] returns, seconds of input.
const MAX_WINDOW_S: f64 = 3600.0;
/// A tape that was not closed and was written this recently may still be live: `prune`
/// leaves it alone.
const LIVE_WITHIN: Duration = Duration::from_secs(120);

/// A piece of the microphone: contiguous samples from input frame `in_first`.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioPiece {
    /// Input frame of the first sample.
    pub in_first: u64,
    /// The samples.
    pub samples: Vec<f32>,
    /// The raw ring lost these samples: they are zeros.
    pub lost: bool,
}

/// A tape in memory.
#[derive(Debug)]
pub struct Tape {
    /// `tape.json`.
    pub header: TapeHeader,
    /// `(seq, event)` in file order, `seq` ascending.
    pub events: Vec<(u64, TapeEvent)>,
    /// Closed cleanly, nothing lost, and the audio is all there. Only such tapes enter a
    /// gate.
    pub complete: bool,
    /// Lines of a type this reader does not know.
    pub unknown_lines: u32,
    /// Lines that did not parse or lacked a field (a cut last line of a crashed tape is
    /// one).
    pub bad_lines: u32,
    runs: Vec<MicRun>,
    wav: Option<WavInfo>,
    dir: PathBuf,
}

#[derive(Debug, Clone, Copy)]
struct WavInfo {
    /// Byte offset of the first sample.
    data_start: u64,
    /// Samples the reader uses.
    frames: u64,
    /// Bytes after the last of those samples.
    trailing: u64,
}

fn rd_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn rd_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

/// Finds the data chunk of a mono 32-bit float WAV. The sample count comes from the file
/// size when `trust_size` is false (a tape that was not closed has a stale header),
/// otherwise from the header, capped by the file.
fn read_wav_info(path: &Path, fs_in: u32, trust_header: bool) -> Result<WavInfo, TapeError> {
    let mut f = File::open(path)?;
    let file_len = f.metadata()?.len();
    let mut head = vec![0u8; 4096.min(file_len as usize)];
    f.read_exact(&mut head)?;
    if head.len() < 12 || &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return Err(TapeError::Format("mic.wav is not a WAV file".into()));
    }
    let mut at = 12usize;
    let mut fmt_ok = false;
    loop {
        let (Some(id), Some(len)) = (head.get(at..at + 4), rd_u32(&head, at + 4)) else {
            return Err(TapeError::Format("mic.wav has no data chunk".into()));
        };
        let body = at + 8;
        if id == b"fmt " {
            let tag = rd_u16(&head, body).unwrap_or(0);
            let ch = rd_u16(&head, body + 2).unwrap_or(0);
            let rate = rd_u32(&head, body + 4).unwrap_or(0);
            let bits = rd_u16(&head, body + 14).unwrap_or(0);
            if !(tag == 3 || tag == 0xFFFE) || ch != 1 || bits != 32 || rate != fs_in {
                return Err(TapeError::Format(
                    "mic.wav is not mono f32 at the input rate".into(),
                ));
            }
            fmt_ok = true;
        } else if id == b"data" {
            if !fmt_ok {
                return Err(TapeError::Format("mic.wav has no fmt chunk".into()));
            }
            let data_start = body as u64;
            let avail = file_len.saturating_sub(data_start);
            let bytes = if trust_header && u64::from(len) <= avail {
                u64::from(len)
            } else {
                avail
            };
            return Ok(WavInfo {
                data_start,
                frames: bytes / 4,
                trailing: avail - bytes / 4 * 4,
            });
        }
        at = body + len as usize + (len as usize & 1);
    }
}

impl Tape {
    /// Opens the tape in `dir`. Errors: no `tape.json` (`Io`), a wrong `v`
    /// (`Version`), a file that is not what the format says (`Format`).
    pub fn open(dir: &Path) -> Result<Tape, TapeError> {
        let text = fs::read_to_string(dir.join("tape.json"))?;
        let mut header = header_from_text(&text)?;
        let closed = header.complete;

        let mut events = Vec::new();
        let (mut unknown, mut bad) = (0u32, 0u32);
        let mut events_present = true;
        match fs::read(dir.join("events.jsonl")) {
            Ok(bytes) => {
                let mut last: Option<u64> = None;
                for raw in bytes.split(|b| *b == b'\n') {
                    if raw.is_empty() {
                        continue;
                    }
                    let Ok(line) = std::str::from_utf8(raw) else {
                        bad += 1;
                        continue;
                    };
                    match decode_line(line) {
                        Decoded::Event(seq, ev) => {
                            if last.is_some_and(|l| seq <= l) {
                                bad += 1;
                            } else {
                                last = Some(seq);
                                events.push((seq, ev));
                            }
                        }
                        Decoded::Unknown => unknown += 1,
                        Decoded::Bad => bad += 1,
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => events_present = false,
            Err(e) => return Err(e.into()),
        }

        // Anything that does not add up damages the tape: it then stays out of the gates.
        let mut damaged = bad > 0 || unknown > 0 || !events_present || !events_contiguous(&events);
        if closed && header.counts.events != events.len() as u64 {
            damaged = true;
        }
        if !closed {
            header.takes = takes_of(&events);
            header.disputed |= events
                .iter()
                .any(|(_, e)| matches!(e, TapeEvent::Cmd(SessionCmdLog::Dispute)));
            header.counts.events = events.len() as u64;
        }

        let mut runs = Vec::new();
        let mut wav = None;
        let runs_path = dir.join("mic.runs.json");
        let wav_path = dir.join("mic.wav");
        let (have_runs, have_wav) = (runs_path.is_file(), wav_path.is_file());
        if header.has_audio && !(have_runs && have_wav) {
            damaged = true;
        }
        if have_runs && have_wav {
            let (rate, r) = runs_from_text(&fs::read_to_string(&runs_path)?)?;
            if rate != header.fs_in {
                return Err(TapeError::Format(
                    "mic.runs.json rate differs from tape.json".into(),
                ));
            }
            let info = read_wav_info(&wav_path, header.fs_in, closed)?;
            match runs_total(&r) {
                None => damaged = true, // overflow, overlap, a hole or an empty run: no audio
                Some(total) => {
                    runs = r;
                    if total > info.frames {
                        // The WAV is shorter than the runs say: cut the runs to what exists.
                        damaged = true;
                        let mut left = info.frames;
                        for r in &mut runs {
                            r.frames = r.frames.min(left);
                            left -= r.frames;
                        }
                        runs.retain(|r| r.frames > 0);
                    } else if closed && (total != info.frames || info.trailing > 0) {
                        damaged = true;
                    }
                    let kept: u64 = runs.iter().map(|r| r.frames).sum();
                    let lost: u64 = runs.iter().filter(|r| r.lost).map(|r| r.frames).sum();
                    if closed
                        && (header.counts.mic_frames != kept || header.counts.lost_frames != lost)
                    {
                        damaged = true;
                    }
                    header.counts.mic_frames = kept;
                    header.counts.lost_frames = lost;
                    wav = Some(info);
                }
            }
        }
        if damaged {
            header.complete = false;
        }
        let complete = header.complete;
        Ok(Tape {
            header,
            events,
            complete,
            unknown_lines: unknown,
            bad_lines: bad,
            runs,
            wav,
            dir: dir.to_path_buf(),
        })
    }

    /// The tape folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The runs of `mic.wav`, in file order. They cover the file without gaps: the
    /// sample at index `i` of the WAV has detector time `runs[0].det_first + i`.
    pub fn runs(&self) -> &[MicRun] {
        &self.runs
    }

    /// True if the tape has a microphone recording.
    pub fn has_audio(&self) -> bool {
        self.wav.is_some()
    }

    fn read_samples(&self, first: u64, n: u64) -> Result<Vec<f32>, TapeError> {
        let info = self.wav.ok_or(TapeError::NoAudio)?;
        let mut f = File::open(self.dir.join("mic.wav"))?;
        f.seek(SeekFrom::Start(info.data_start + first * 4))?;
        let mut bytes = vec![0u8; (n * 4) as usize];
        f.read_exact(&mut bytes)?;
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect())
    }

    /// The samples whose input frame lies in `[in_from, in_to)`, as pieces in time order:
    /// one per run the window touches. Across an input jump there are two pieces and no
    /// samples for the frames between them (there are none on the tape).
    pub fn audio_pieces(&self, in_from: u64, in_to: u64) -> Result<Vec<AudioPiece>, TapeError> {
        if self.wav.is_none() {
            return Err(TapeError::NoAudio);
        }
        if in_to < in_from {
            return Err(TapeError::Range("end before start".into()));
        }
        if (in_to - in_from) as f64 > MAX_WINDOW_S * f64::from(self.header.fs_in) {
            return Err(TapeError::Range("window too long".into()));
        }
        let mut out = Vec::new();
        let mut base = 0u64;
        for r in &self.runs {
            let (s, e) = (r.in_first, r.in_first + r.frames);
            let (a, b) = (s.max(in_from), e.min(in_to));
            if a < b {
                out.push(AudioPiece {
                    in_first: a,
                    samples: self.read_samples(base + (a - s), b - a)?,
                    lost: r.lost,
                });
            }
            base += r.frames;
        }
        out.sort_by_key(|p| p.in_first);
        Ok(out)
    }

    /// The window `[in_from, in_to)` in input frames, exactly `in_to - in_from` samples:
    /// frame `in_from + i` is element `i`. Frames the tape has no sample for (a jump of
    /// the input clock, or before/after the recording) are zeros. A zero-filled lost
    /// block is zeros too; ask [`Tape::audio_pieces`] to tell them apart.
    pub fn audio(&self, in_from: u64, in_to: u64) -> Result<Vec<f32>, TapeError> {
        let pieces = self.audio_pieces(in_from, in_to)?;
        let mut out = vec![0.0f32; (in_to - in_from) as usize];
        for p in pieces {
            let at = (p.in_first - in_from) as usize;
            out[at..at + p.samples.len()].copy_from_slice(&p.samples);
        }
        Ok(out)
    }

    /// The whole recording in detector time (index `i` is detector time `det_origin() +
    /// i`): what the live detector saw, with lost blocks as zeros. This is what a replay
    /// feeds to an `OnsetDetector`.
    pub fn mic(&self) -> Result<Vec<f32>, TapeError> {
        let n: u64 = self.runs.iter().map(|r| r.frames).sum();
        if self.wav.is_none() {
            return Err(TapeError::NoAudio);
        }
        self.read_samples(0, n)
    }

    /// Detector time of the first sample of `mic.wav`.
    pub fn det_origin(&self) -> Option<u64> {
        self.runs.first().map(|r| r.det_first)
    }

    /// Input frame (fractional) of detector time `det`, through the runs. `None` when no
    /// run holds it.
    pub fn in_of_det(&self, det: f64) -> Option<f64> {
        if !det.is_finite() || det < 0.0 {
            return None;
        }
        // Onsets are retro-dated and can sit a little before a run's first sample, so a
        // time in the run before the first, or past the last, is mapped with the nearest.
        let idx = self
            .runs
            .iter()
            .position(|r| det < (r.det_first + r.frames) as f64)
            .or(self.runs.len().checked_sub(1))?;
        let r = &self.runs[idx];
        Some(r.in_first as f64 + (det - r.det_first as f64))
    }
}

/// Total frames of `runs` if they are sound: no empty run, no overflow, detector time
/// contiguous from run to run. `None` otherwise.
fn runs_total(runs: &[MicRun]) -> Option<u64> {
    let mut total = 0u64;
    let mut next_det: Option<u64> = None;
    for r in runs {
        if r.frames == 0 {
            return None;
        }
        let det_end = r.det_first.checked_add(r.frames)?;
        r.in_first.checked_add(r.frames)?;
        if next_det.is_some_and(|n| n != r.det_first) {
            return None;
        }
        next_det = Some(det_end);
        total = total.checked_add(r.frames)?;
    }
    Some(total)
}

/// `seq` runs 0, 1, 2, ... with no gap.
fn events_contiguous(events: &[(u64, TapeEvent)]) -> bool {
    events.iter().enumerate().all(|(i, (s, _))| *s == i as u64)
}

fn takes_of(events: &[(u64, TapeEvent)]) -> Vec<TakeSpan> {
    let mut takes: Vec<TakeSpan> = Vec::new();
    for (seq, ev) in events {
        match ev {
            TapeEvent::Take { label } => takes.push(TakeSpan {
                label: label.clone(),
                seq_first: *seq,
                seq_last: *seq,
            }),
            _ => {
                if let Some(t) = takes.last_mut() {
                    t.seq_last = *seq;
                }
            }
        }
    }
    takes
}

/// Reads only `tape.json` of the folder.
pub fn read_header(dir: &Path) -> Result<TapeHeader, TapeError> {
    header_from_text(&fs::read_to_string(dir.join("tape.json"))?)
}

/// Deletes the oldest tapes under `root` until the tapes take at most `max_bytes`, and
/// returns the bytes freed. A folder is a tape if it holds a `tape.json`; the rest of
/// `root` is not touched. Never deleted:
///
/// - a disputed tape (`disputed` in `tape.json`; for a tape that was not closed, a
///   `dispute` command line in `events.jsonl`);
/// - a tape that was not closed and was written to in the last two minutes (it may be
///   live);
/// - a folder `tape.json` of which cannot be read (unknown version: not ours to delete).
///
/// Disputed and live tapes count toward the total, so the result may stay above
/// `max_bytes`. Oldest first means by folder name (`AAAAMMDD-HHMMSS-xxxx`).
pub fn prune(root: &Path, max_bytes: u64) -> io::Result<u64> {
    let mut tapes: Vec<(PathBuf, u64, bool)> = Vec::new(); // (dir, bytes, keep)
    let rd = match fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    for entry in rd {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() || !path.join("tape.json").is_file() {
            continue;
        }
        let bytes = dir_size(&path);
        tapes.push((path.clone(), bytes, keep_tape(&path)));
    }
    tapes.sort_by(|a, b| a.0.cmp(&b.0));
    let mut total: u64 = tapes.iter().map(|t| t.1).sum();
    let mut freed = 0;
    for (path, bytes, keep) in &tapes {
        if total <= max_bytes {
            break;
        }
        if *keep {
            continue;
        }
        fs::remove_dir_all(path)?;
        total -= bytes;
        freed += bytes;
    }
    Ok(freed)
}

const DISPUTE: &[u8] = b"\"c\":\"dispute\"";

fn keep_tape(dir: &Path) -> bool {
    let Ok(h) = read_header(dir) else {
        return true;
    };
    if h.disputed {
        return true;
    }
    if h.complete {
        return false;
    }
    let events = dir.join("events.jsonl");
    // A line of a closed-or-not tape that carries a dispute: the writer sets the header
    // flag at the next flush, so a crash can leave only the line.
    // Bytes, not text: a crash can cut a multi-byte character, and that must not hide a
    // dispute that is earlier in the file. If the file cannot be read, keep the tape.
    match fs::read(&events) {
        Ok(b) if b.windows(DISPUTE.len()).any(|w| w == DISPUTE) => return true,
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return true,
    }
    fs::metadata(&events)
        .and_then(|m| m.modified())
        .is_ok_and(|t| {
            SystemTime::now()
                .duration_since(t)
                .map_or(true, |age| age < LIVE_WITHIN)
        })
}
