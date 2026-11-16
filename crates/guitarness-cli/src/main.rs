//! `guitarness` command line: analyse WAV files with the real pipeline, and test live audio.
//! Usage: guitarness analyze <file.wav> [--fft|--direct]
//!        guitarness replay <tape> ...
//!        guitarness verify <tape>... [--json]
//!        guitarness audio devices|probe|soak ...   (see `guitarness audio --help`)

mod audio_cmd;
mod calibrate_cmd;
mod replay_cmd;
mod tune_cmd;
mod verify_cmd;

use guitarness_dsp::{AcfMode, Event, Pipeline, Via};

#[cfg(feature = "rt-audit")]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("analyze") if args.len() >= 3 => {
            analyze(&args[2], args.iter().any(|a| a == "--direct"))
        }
        Some("audio") => std::process::exit(audio_cmd::run(&args[2..])),
        Some("calibrate") => std::process::exit(calibrate_cmd::run(&args[2..])),
        Some("replay") => std::process::exit(replay_cmd::run(&args[2..])),
        Some("tune") => std::process::exit(tune_cmd::run(&args[2..])),
        Some("verify") => std::process::exit(verify_cmd::run(&args[2..])),
        _ => {
            eprintln!("usage: guitarness analyze <file.wav> [--direct]");
            eprintln!(
                "       guitarness replay <tape>... [--audio] [--report] [--json]   (guitarness replay --help)"
            );
            eprintln!(
                "       guitarness verify <tape>... [--gates FILE] [--json] [--detalhes]   (guitarness verify --help)"
            );
            eprintln!(
                "       guitarness audio devices|probe|soak [...]   (guitarness audio --help)"
            );
            std::process::exit(2);
        }
    }
}

fn analyze(path: &str, direct: bool) {
    let mut r = match hound::WavReader::open(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot open {path}: {e}");
            std::process::exit(1);
        }
    };
    let spec = r.spec();
    let ch = spec.channels as usize;
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let scale = (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.unwrap_or(0) as f32 / scale)
                .collect()
        }
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
    };
    let mono: Vec<f32> = raw
        .chunks(ch)
        .map(|c| c.iter().sum::<f32>() / ch as f32)
        .collect();
    let sr = spec.sample_rate as f64;
    let mode = if direct {
        AcfMode::Direct
    } else {
        AcfMode::Fft
    };
    let mut pipe = Pipeline::demo(sr, mode, 256);
    let mut frames = 0usize;
    let start = std::time::Instant::now();
    for (b, block) in mono.chunks(256).enumerate() {
        pipe.process(
            block,
            (b * 256) as f64 / sr,
            |_| frames += 1,
            |e| match e {
                Event::Note {
                    t,
                    midi,
                    cents,
                    via,
                    ..
                } => {
                    let v = if via == Via::Target { "alvo" } else { "mpm" };
                    println!(
                        "{t:9.4}  nota {midi:3} ({:>3}) {cents:+5.1} cents  via {v}",
                        name(midi)
                    );
                }
                Event::Attack { t, level } => println!("{t:9.4}  ataque  nível {level:.4}"),
            },
        );
    }
    let el = start.elapsed();
    eprintln!(
        "{frames} quadros em {:.1} ms ({:.1} µs/quadro, inclui filtros e rastreador), {:.1} s de áudio",
        el.as_secs_f64() * 1e3,
        el.as_secs_f64() * 1e6 / frames.max(1) as f64,
        mono.len() as f64 / sr
    );
}

fn name(midi: i32) -> String {
    const N: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!("{}{}", N[midi.rem_euclid(12) as usize], midi / 12 - 1)
}
