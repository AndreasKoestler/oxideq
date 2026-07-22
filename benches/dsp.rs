//! DSP throughput bench. Interpretation:
//! a 256-frame stereo block at 48 kHz is 5.333 ms of audio, so the
//! <1%-of-one-core NFR requires < 53.3 us per iteration here.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use std::hint::black_box;

use oxideq::dsp::EqChain;
use oxideq::preset;

const FRAMES: usize = 256;
const CHANNELS: usize = 2;

fn bench_process(c: &mut Criterion) {
    let parsed = preset::parse(include_str!("../presets/example.txt")).unwrap();
    assert_eq!(
        parsed.preset.bands.len(),
        10,
        "bench expects the 10-band fixture"
    );

    let mut g = c.benchmark_group("dsp");
    g.throughput(Throughput::Elements(FRAMES as u64));
    for factor in [1usize, 4, 16] {
        let mut chain = EqChain::new(&parsed.preset, 48_000.0, CHANNELS, factor).unwrap();
        // Non-silent, non-constant input so denormal handling costs are visible.
        let mut buf: Vec<f32> = (0..FRAMES * CHANNELS)
            .map(|i| ((i % 97) as f32 / 97.0) - 0.5)
            .collect();
        g.bench_function(format!("process_256frame_stereo_10band_{factor}x"), |b| {
            b.iter(|| chain.process(black_box(&mut buf)));
        });
    }
    g.finish();
}

criterion_group!(benches, bench_process);
criterion_main!(benches);
