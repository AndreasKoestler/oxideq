use anyhow::{bail, Context, Result};

use oxideq::{cli, devices, engine, preset};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&args)? {
        cli::Cmd::Help => {
            print!("{}", cli::USAGE);
            Ok(())
        }
        cli::Cmd::Devices => devices::list(&cpal::default_host()),
        cli::Cmd::InstallSink => install_sink(),
        cli::Cmd::Run(a) => run(a),
    }
}

fn install_sink() -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!(
            "install-sink is Linux/PipeWire only.\n\
             On macOS: `brew install blackhole-2ch`, set \"BlackHole 2ch\" as the\n\
             default output, then run:\n\
             oxideq run --preset <file> --input BlackHole --output \"<your DAC>\""
        );
    }
    oxideq::routing::install_sink()
}

fn run(a: cli::RunArgs) -> Result<()> {
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

    let after_start = after_start_hook(&a);
    engine::run(
        &input,
        &output,
        &parsed.preset,
        &engine::EngineConfig {
            buffer_frames: a.buffer_frames,
            channels: 2,
        },
        after_start,
    )
}

/// PRD 3.1: the preamp is the only clipping protection. If boosts exceed
/// it, say so up front — we will never limit dynamically.
fn warn_headroom(p: &preset::Preset) {
    let max_boost = p.bands.iter().map(|b| b.gain_db).fold(0.0f64, f64::max);
    if max_boost + p.preamp_db > 0.0 {
        eprintln!(
            "warning: max boost {max_boost:.1} dB exceeds preamp {:.1} dB — clipping possible",
            p.preamp_db
        );
    }
}

fn after_start_hook(a: &cli::RunArgs) -> Option<Box<dyn FnOnce() + Send>> {
    if a.auto_link {
        // Real implementation lands with Tier 3 (Task 9).
        eprintln!("--auto-link is not implemented yet; ignored");
    }
    None
}
