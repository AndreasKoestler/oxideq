//! NFR guard: DSP < 1% of one core, sub-millisecond per block.
//! Run with:  cargo test --release --test perf -- --ignored --nocapture

use std::time::Instant;

use oxideq::dsp::EqChain;
use oxideq::preset;

#[test]
#[ignore = "wall-clock timing: run explicitly with --release"]
fn dsp_meets_cpu_and_latency_budget() {
    const FRAMES: usize = 256;
    const CHANNELS: usize = 2;
    const RATE: usize = 48_000;
    const AUDIO_SECS: usize = 10;

    let parsed = preset::parse(include_str!("../presets/example.txt")).unwrap();
    let mut chain = EqChain::new(&parsed.preset, RATE as f64, CHANNELS).unwrap();
    let mut buf: Vec<f32> = (0..FRAMES * CHANNELS)
        .map(|i| ((i % 97) as f32 / 97.0) - 0.5)
        .collect();

    for _ in 0..1_000 {
        chain.process(&mut buf); // warm-up
    }

    let blocks = RATE * AUDIO_SECS / FRAMES;
    let start = Instant::now();
    for _ in 0..blocks {
        chain.process(&mut buf);
    }
    let elapsed = start.elapsed();

    let cpu_fraction = elapsed.as_secs_f64() / AUDIO_SECS as f64;
    let per_block_us = elapsed.as_secs_f64() * 1e6 / blocks as f64;
    eprintln!(
        "dsp: {:.3}% of one core, {per_block_us:.2} µs per {FRAMES}-frame block",
        cpu_fraction * 100.0
    );
    assert!(
        cpu_fraction < 0.01,
        "CPU NFR violated: {:.3}% ≥ 1% of one core",
        cpu_fraction * 100.0
    );
    assert!(
        per_block_us < 1_000.0,
        "latency NFR violated: {per_block_us:.2} µs per block ≥ 1 ms"
    );
}
