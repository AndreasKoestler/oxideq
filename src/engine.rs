//! Lock-free capture → EQ → playback engine.
//!
//! Threading model: cpal owns one input and one output callback thread.
//! They communicate only through a pre-allocated SPSC ring buffer and
//! relaxed atomics. The main thread sleeps and reports stats.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{BufferSize, Device, SampleFormat, StreamConfig, SupportedBufferSize};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;

use crate::devices;
use crate::dsp::EqChain;
use crate::preset::Preset;

pub struct EngineConfig {
    pub buffer_frames: u32,
    pub channels: u16,
}

/// Ring capacity in blocks of `buffer_frames` frames.
const RING_BLOCKS: usize = 8;
/// Silence primed into the ring before playback starts, in blocks.
const PREFILL_BLOCKS: usize = 2;
/// Upper bound on samples handled per callback chunk (scratch size).
const MAX_CALLBACK_SAMPLES: usize = 16_384;

struct Stats {
    underruns: AtomicU64,
    overruns: AtomicU64,
    clipped: AtomicU64,
}

pub fn clamp_buffer(requested: u32, supported: &SupportedBufferSize) -> BufferSize {
    match supported {
        SupportedBufferSize::Range { min, max } => BufferSize::Fixed(requested.clamp(*min, *max)),
        SupportedBufferSize::Unknown => BufferSize::Default,
    }
}

pub fn run(input: &Device, output: &Device, preset: &Preset, cfg: &EngineConfig) -> Result<()> {
    // Name our PipeWire node `oxideq` so pw-link / qpwgraph can find it for
    // manual routing. (pipewire-alsa reads PIPEWIRE_PROPS; harmless elsewhere.)
    if cfg!(target_os = "linux") {
        std::env::set_var("PIPEWIRE_PROPS", "{ node.name = oxideq }");
    }

    let in_default = input
        .default_input_config()
        .context("querying input config")?;
    if in_default.sample_format() != SampleFormat::F32 {
        eprintln!(
            "warning: input default format is {:?}; requesting f32 anyway",
            in_default.sample_format()
        );
    }
    // Detect the source rate at runtime. (cpal 0.18 `sample_rate()` returns
    // a bare u32 — no `.0` to unwrap.)
    let want = in_default.sample_rate();
    let ch = cfg.channels as usize;

    // One rate for the whole pipeline: the ring has no resampler, so
    // capture and playback must open at the same rate or the ring drifts
    // and the audio pitch-shifts.
    let (rate, exact) = devices::negotiate_rate(input, output, want, cfg.channels)?;
    if !exact {
        eprintln!(
            "warning: output cannot lock {want} Hz; whole pipeline runs at {rate} Hz \
             (the OS resamples capture — not bit-perfect)"
        );
    }

    let out_supported = output
        .default_output_config()
        .context("querying output config")?;
    let in_buf = clamp_buffer(cfg.buffer_frames, in_default.buffer_size());
    let out_buf = clamp_buffer(cfg.buffer_frames, out_supported.buffer_size());
    let in_cfg = StreamConfig {
        channels: cfg.channels,
        sample_rate: rate,
        buffer_size: in_buf,
    };
    let out_cfg = StreamConfig {
        channels: cfg.channels,
        sample_rate: rate,
        buffer_size: out_buf,
    };

    // Devices may clamp the requested block size; size the ring and report
    // latency from what they actually use (`Default` → assume the request).
    let fixed = |b: &BufferSize| match *b {
        BufferSize::Fixed(n) => n,
        BufferSize::Default => cfg.buffer_frames,
    };
    let frames = fixed(&in_cfg.buffer_size).max(fixed(&out_cfg.buffer_size));
    let block = frames as usize * ch;
    let rb = HeapRb::<f32>::new(block * RING_BLOCKS);
    let (mut prod, mut cons) = rb.split();

    // Prime with silence so the first output callbacks don't underrun.
    let silence = vec![0.0f32; block * PREFILL_BLOCKS];
    prod.push_slice(&silence);

    let mut chain = EqChain::new(preset, f64::from(rate), ch)?;
    let num_bands = chain.num_bands();
    let stats = Arc::new(Stats {
        underruns: AtomicU64::new(0),
        overruns: AtomicU64::new(0),
        clipped: AtomicU64::new(0),
    });

    // Input callback: preamp + EQ, clip count, push. RT-safe.
    // Chunk cap rounded down to a whole frame: a chunk boundary inside a
    // frame would run one channel's samples through another's filter state.
    let chunk_samples = MAX_CALLBACK_SAMPLES - MAX_CALLBACK_SAMPLES % ch;
    let mut scratch = vec![0.0f32; MAX_CALLBACK_SAMPLES];
    let in_stats = Arc::clone(&stats);
    let input_stream = input
        .build_input_stream(
            in_cfg,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                for chunk in data.chunks(chunk_samples) {
                    // `data` is read-only; copy each chunk into the
                    // pre-allocated scratch to EQ it in place. Chunking caps
                    // the copy at scratch's size for any callback length.
                    let s = &mut scratch[..chunk.len()];
                    s.copy_from_slice(chunk);
                    chain.process(s);
                    // `.contains` is clippy-idiomatic and optimizes to the
                    // same code as `x < -1.0 || x > 1.0`. Count locally,
                    // touch the shared atomic at most once per chunk.
                    let clipped = s.iter().filter(|x| !(-1.0..=1.0).contains(*x)).count();
                    if clipped > 0 {
                        in_stats
                            .clipped
                            .fetch_add(clipped as u64, Ordering::Relaxed);
                    }
                    if prod.push_slice(s) < s.len() {
                        in_stats.overruns.fetch_add(1, Ordering::Relaxed);
                    }
                }
            },
            |e| eprintln!("input stream error: {e}"),
            None,
        )
        .context("building input stream (is the device f32-capable at this rate?)")?;

    // Output callback: pop or zero-fill. RT-safe.
    let out_stats = Arc::clone(&stats);
    let output_stream = output
        .build_output_stream(
            out_cfg,
            move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let n = cons.pop_slice(out);
                if n < out.len() {
                    out[n..].fill(0.0);
                    out_stats.underruns.fetch_add(1, Ordering::Relaxed);
                }
            },
            |e| eprintln!("output stream error: {e}"),
            None,
        )
        .context("building output stream")?;

    input_stream.play().context("starting input stream")?;
    output_stream.play().context("starting output stream")?;

    let latency_ms = (frames as f64 * (2 + PREFILL_BLOCKS) as f64) / f64::from(rate) * 1_000.0;
    println!(
        "oxideq: {rate} Hz, {ch} ch, {num_bands} bands, block {frames} frames (~{latency_ms:.1} ms pipeline latency)"
    );

    loop {
        std::thread::sleep(Duration::from_secs(5));
        let u = stats.underruns.swap(0, Ordering::Relaxed);
        let o = stats.overruns.swap(0, Ordering::Relaxed);
        let c = stats.clipped.swap(0, Ordering::Relaxed);
        if u + o > 0 {
            eprintln!("warning: {u} underruns, {o} overruns in last 5 s (try a larger --buffer)");
        }
        if c > 0 {
            eprintln!(
                "warning: {c} samples clipped in last 5 s — preset preamp may be insufficient"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::SupportedBufferSize;

    #[test]
    fn requested_buffer_inside_range_is_kept() {
        let r = SupportedBufferSize::Range {
            min: 64,
            max: 8_192,
        };
        assert!(matches!(
            clamp_buffer(256, &r),
            cpal::BufferSize::Fixed(256)
        ));
    }

    #[test]
    fn requested_buffer_is_clamped_to_range() {
        let r = SupportedBufferSize::Range {
            min: 64,
            max: 8_192,
        };
        assert!(matches!(clamp_buffer(16, &r), cpal::BufferSize::Fixed(64)));
        assert!(matches!(
            clamp_buffer(100_000, &r),
            cpal::BufferSize::Fixed(8_192)
        ));
    }

    #[test]
    fn unknown_buffer_support_uses_device_default() {
        assert!(matches!(
            clamp_buffer(256, &SupportedBufferSize::Unknown),
            cpal::BufferSize::Default
        ));
    }
}
