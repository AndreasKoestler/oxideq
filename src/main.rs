use anyhow::{Context, Result};
use clap::Parser;

use oxideq::{cli, devices, dsp, engine, preset};

fn main() -> Result<()> {
    match cli::Cli::parse().cmd {
        cli::Cmd::Devices(a) => devices::list(&cpal::default_host(), a.all),
        cli::Cmd::Run(a) => run(&a),
    }
}

fn run(a: &cli::RunArgs) -> Result<()> {
    let text = std::fs::read_to_string(&a.preset)
        .with_context(|| format!("reading preset {}", a.preset))?;
    let parsed = preset::parse(&text)?;
    for w in &parsed.warnings {
        eprintln!("preset: {w}");
    }
    warn_headroom(&parsed.preset);

    let host = cpal::default_host();
    let input = devices::find(&host, devices::Direction::Input, a.input.as_deref())?;
    let output = devices::find(&host, devices::Direction::Output, a.output.as_deref())?;

    engine::run(
        &input,
        &output,
        &parsed.preset,
        &engine::EngineConfig {
            buffer_frames: a.buffer_frames,
            channels: 2,
            oversample: a.oversample,
            backend: a.backend.into(),
        },
    )
}

/// The preamp is the only clipping protection — we never limit dynamically.
/// Warn up front if the cascade's *summed* peak gain exceeds 0 dBFS: a
/// full-scale input near that frequency would clip. Evaluated at 48 kHz — the
/// peak of an EQ curve is set by its low/mid bands, so it barely moves with the
/// actual run rate. Falls back silently if coefficients can't be built here
/// (the engine surfaces that error properly when it opens the stream).
fn warn_headroom(p: &preset::Preset) {
    let Ok(peak_db) = dsp::peak_gain_db(p, 48_000.0) else {
        return;
    };
    if peak_db > 0.0 {
        eprintln!(
            "warning: preset peaks at {peak_db:+.1} dBFS (preamp {:.1} dB) — clipping possible; \
             lower Preamp by ~{peak_db:.1} dB for full headroom",
            p.preamp_db
        );
    }
}
