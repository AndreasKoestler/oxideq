//! Lock-free capture → EQ → playback engine.
//!
//! Threading model: cpal owns one input and one output callback thread.
//! They communicate only through a pre-allocated SPSC ring buffer and
//! relaxed atomics. The main thread sleeps and reports stats.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{BufferSize, Device, SampleFormat, Stream, StreamConfig, SupportedBufferSize};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer, Split};

use crate::devices;
use crate::dsp::{Backend, EqChain};
use crate::preset::Preset;

pub struct EngineConfig {
    pub buffer_frames: u32,
    pub channels: u16,
    /// EQ-cascade oversampling factor (1 = off).
    pub oversample: usize,
    /// Biquad backend for the cascade.
    pub backend: Backend,
}

/// Ring capacity in blocks of `buffer_frames` frames.
const RING_BLOCKS: usize = 8;
/// Silence primed into the ring before playback starts, in blocks.
const PREFILL_BLOCKS: usize = 2;
/// Upper bound on samples handled per callback chunk (scratch size).
const MAX_CALLBACK_SAMPLES: usize = 16_384;

#[derive(Default)]
struct Stats {
    underruns: AtomicU64,
    overruns: AtomicU64,
    clipped: AtomicU64,
}

#[must_use]
pub fn clamp_buffer(requested: u32, supported: &SupportedBufferSize) -> BufferSize {
    match supported {
        SupportedBufferSize::Range { min, max } => BufferSize::Fixed(requested.clamp(*min, *max)),
        SupportedBufferSize::Unknown => BufferSize::Default,
    }
}

/// Total pipeline latency in milliseconds: the ring buffering (the prefill plus
/// the two in-flight blocks) added to the EQ cascade's own group delay.
#[must_use]
fn pipeline_latency_ms(frames: u32, rate: u32, os_latency_frames: f64) -> f64 {
    let os_ms = os_latency_frames / f64::from(rate) * 1_000.0;
    (f64::from(frames) * (2 + PREFILL_BLOCKS) as f64) / f64::from(rate) * 1_000.0 + os_ms
}

/// Largest chunk size not exceeding `cap` that is a whole number of frames.
///
/// A chunk boundary inside a frame would run one channel's samples through
/// another channel's filter state, so the cap is rounded down to a frame.
///
/// # Panics
/// Panics if a single frame doesn't fit in `cap` (`channels > cap`): the
/// rounded-down result would be 0, and `slice::chunks(0)` would panic later
/// inside the RT callback — fail here at stream-build time instead, with a
/// message that names the actual problem.
#[must_use]
fn frame_aligned_chunk(cap: usize, channels: usize) -> usize {
    assert!(
        channels <= cap,
        "channel count {channels} exceeds callback scratch capacity {cap}"
    );
    cap - cap % channels
}

/// Number of samples outside the normalized `[-1.0, 1.0]` range.
///
/// `.contains` is clippy-idiomatic and optimizes to the same code as
/// `x < -1.0 || x > 1.0`; the `±1.0` boundary is in range. NaN fails the
/// range test and therefore counts as clipped — intentional: a non-finite
/// sample is faulty output worth warning about.
#[must_use]
fn count_clipped(samples: &[f32]) -> usize {
    samples
        .iter()
        .filter(|x| !(-1.0..=1.0).contains(*x))
        .count()
}

/// Frame count the ring and latency report should assume: the larger of the two
/// devices' block sizes, falling back to `requested` when a device reports
/// `Default` (devices may clamp the requested block size).
#[must_use]
fn block_frames(in_buf: BufferSize, out_buf: BufferSize, requested: u32) -> u32 {
    let fixed = |b: BufferSize| match b {
        BufferSize::Fixed(n) => n,
        BufferSize::Default => requested,
    };
    fixed(in_buf).max(fixed(out_buf))
}

/// The one-line startup banner printed once both streams are running.
#[must_use]
fn startup_line(
    rate: u32,
    ch: usize,
    num_bands: usize,
    backend: Backend,
    oversample: usize,
    frames: u32,
    latency_ms: f64,
) -> String {
    let os_note = if oversample > 1 {
        format!(", {oversample}x oversampled")
    } else {
        String::new()
    };
    let backend_note = match backend {
        Backend::Df1 => String::new(),
        Backend::Df2 => ", DF2 biquads".to_string(),
    };
    format!(
        "oxideq: {rate} Hz, {ch} ch, {num_bands} bands{backend_note}{os_note}, block {frames} frames (~{latency_ms:.1} ms pipeline latency)"
    )
}

/// Warning lines for the last reporting window; empty when every counter is
/// zero.
#[must_use]
fn stats_warnings(underruns: u64, overruns: u64, clipped: u64) -> Vec<String> {
    let mut warnings = Vec::new();
    if underruns + overruns > 0 {
        warnings.push(format!(
            "warning: {underruns} underruns, {overruns} overruns in last 5 s (try a larger --buffer)"
        ));
    }
    if clipped > 0 {
        warnings.push(format!(
            "warning: {clipped} samples clipped in last 5 s — preset preamp may be insufficient"
        ));
    }
    warnings
}

/// Run the capture → EQ → playback engine until the process is killed.
///
/// # Errors
/// Returns an error if device configs cannot be queried, no common sample
/// rate can be negotiated, the EQ chain fails to build, or a stream cannot be
/// built or started.
pub fn run(input: &Device, output: &Device, preset: &Preset, cfg: &EngineConfig) -> Result<()> {
    set_pipewire_node_name();

    let (in_cfg, out_cfg, rate) = negotiated_stream_configs(input, output, cfg)?;
    let frames = block_frames(in_cfg.buffer_size, out_cfg.buffer_size, cfg.buffer_frames);
    let ch = cfg.channels as usize;

    let chain = EqChain::with_backend(preset, f64::from(rate), ch, cfg.oversample, cfg.backend)?;
    let num_bands = chain.num_bands();
    let os_latency_frames = chain.latency_frames();

    let stats = Arc::new(Stats::default());
    let (input_stream, output_stream) =
        build_streams(input, output, in_cfg, out_cfg, chain, &stats, frames)?;
    start_streams(&input_stream, &output_stream)?;

    let latency_ms = pipeline_latency_ms(frames, rate, os_latency_frames);
    println!(
        "{}",
        startup_line(
            rate,
            ch,
            num_bands,
            cfg.backend,
            cfg.oversample,
            frames,
            latency_ms
        )
    );

    report_loop(&stats)
}

/// Default our `PipeWire` node's name to `oxideq` so pw-link / qpwgraph can
/// find it for manual routing — but only when the caller has not set
/// `PIPEWIRE_PROPS` themselves. Overriding it (e.g.
/// `PIPEWIRE_PROPS='{ node.name=oxideq node.autoconnect=false }'`) is how a
/// user stops the session manager auto-wiring oxideq to the default devices —
/// see README "Troubleshooting". (pipewire-alsa reads `PIPEWIRE_PROPS`;
/// harmless elsewhere.)
fn set_pipewire_node_name() {
    if cfg!(target_os = "linux") && std::env::var_os("PIPEWIRE_PROPS").is_none() {
        // SAFETY: `run` calls this as its first statement, on the startup
        // thread before any audio stream or thread is spawned, so no other
        // thread can be reading the environment concurrently.
        unsafe { std::env::set_var("PIPEWIRE_PROPS", "{ node.name = oxideq }") };
    }
}

/// Query both devices, negotiate one shared sample rate, and assemble the
/// capture and playback [`StreamConfig`]s plus the chosen rate.
///
/// A single rate drives the whole pipeline: the ring has no resampler, so
/// capture and playback must open at the same rate or the ring drifts and the
/// audio pitch-shifts. Prints the same non-fatal warnings as before when the
/// input default format is not f32 or the output cannot lock the requested rate.
///
/// # Errors
/// Returns an error if either device config cannot be queried or no common
/// sample rate can be negotiated.
fn negotiated_stream_configs(
    input: &Device,
    output: &Device,
    cfg: &EngineConfig,
) -> Result<(StreamConfig, StreamConfig, u32)> {
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
    let in_cfg = StreamConfig {
        channels: cfg.channels,
        sample_rate: rate,
        buffer_size: clamp_buffer(cfg.buffer_frames, in_default.buffer_size()),
    };
    let out_cfg = StreamConfig {
        channels: cfg.channels,
        sample_rate: rate,
        buffer_size: clamp_buffer(cfg.buffer_frames, out_supported.buffer_size()),
    };
    Ok((in_cfg, out_cfg, rate))
}

/// Create and prime the SPSC ring, then build the capture and playback streams
/// with their RT-safe callbacks. The callbacks allocate nothing: `scratch` and
/// the ring are pre-allocated here and moved in. The callback bodies live in
/// [`process_input_block`] and [`fill_output_block`] so they can be unit-tested
/// without an audio device.
///
/// # Errors
/// Returns an error if either stream cannot be built for the negotiated config.
fn build_streams(
    input: &Device,
    output: &Device,
    in_cfg: StreamConfig,
    out_cfg: StreamConfig,
    mut chain: EqChain,
    stats: &Arc<Stats>,
    frames: u32,
) -> Result<(Stream, Stream)> {
    let ch = in_cfg.channels as usize;
    let block = frames as usize * ch;
    let rb = HeapRb::<f32>::new(block * RING_BLOCKS);
    let (mut prod, mut cons) = rb.split();

    // Prime with silence so the first output callbacks don't underrun.
    let silence = vec![0.0f32; block * PREFILL_BLOCKS];
    prod.push_slice(&silence);

    let chunk_samples = frame_aligned_chunk(MAX_CALLBACK_SAMPLES, ch);
    let mut scratch = vec![0.0f32; MAX_CALLBACK_SAMPLES];
    let in_stats = Arc::clone(stats);
    let input_stream = input
        .build_input_stream(
            in_cfg,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                process_input_block(
                    data,
                    chunk_samples,
                    &mut scratch,
                    &mut chain,
                    &mut prod,
                    &in_stats,
                );
            },
            |e| eprintln!("input stream error: {e}"),
            None,
        )
        .context("building input stream (is the device f32-capable at this rate?)")?;

    let out_stats = Arc::clone(stats);
    let output_stream = output
        .build_output_stream(
            out_cfg,
            move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                fill_output_block(out, &mut cons, &out_stats);
            },
            |e| eprintln!("output stream error: {e}"),
            None,
        )
        .context("building output stream")?;

    Ok((input_stream, output_stream))
}

/// Input-callback body: preamp + EQ each frame-aligned chunk, count clips,
/// push to the ring. RT-safe: allocates nothing.
///
/// `data` is read-only, so each chunk is copied into the pre-allocated
/// `scratch` to EQ it in place; chunking caps the copy at scratch's size for
/// any callback length. Clips are counted locally so the shared atomics are
/// touched at most once per chunk. A chunk the ring cannot fully absorb
/// counts as one overrun.
fn process_input_block<P: Producer<Item = f32>>(
    data: &[f32],
    chunk_samples: usize,
    scratch: &mut [f32],
    chain: &mut EqChain,
    prod: &mut P,
    stats: &Stats,
) {
    for chunk in data.chunks(chunk_samples) {
        let s = &mut scratch[..chunk.len()];
        s.copy_from_slice(chunk);
        chain.process(s);
        let clipped = count_clipped(s);
        if clipped > 0 {
            stats.clipped.fetch_add(clipped as u64, Ordering::Relaxed);
        }
        if prod.push_slice(s) < s.len() {
            stats.overruns.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Output-callback body: pop what the ring holds into `out`; when it comes up
/// short, zero-fill the tail and count one underrun. RT-safe.
fn fill_output_block<C: Consumer<Item = f32>>(out: &mut [f32], cons: &mut C, stats: &Stats) {
    let n = cons.pop_slice(out);
    if n < out.len() {
        out[n..].fill(0.0);
        stats.underruns.fetch_add(1, Ordering::Relaxed);
    }
}

/// Start capture, then playback.
///
/// # Errors
/// Returns an error if either stream fails to start.
fn start_streams(input_stream: &Stream, output_stream: &Stream) -> Result<()> {
    input_stream.play().context("starting input stream")?;
    output_stream.play().context("starting output stream")?;
    Ok(())
}

/// Sleep-and-report loop: every 5 s, drain the counters and print any warnings.
/// Runs until the process is killed.
fn report_loop(stats: &Stats) -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(5));
        let underruns = stats.underruns.swap(0, Ordering::Relaxed);
        let overruns = stats.overruns.swap(0, Ordering::Relaxed);
        let clipped = stats.clipped.swap(0, Ordering::Relaxed);
        for warning in stats_warnings(underruns, overruns, clipped) {
            eprintln!("{warning}");
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

    #[test]
    fn pipeline_latency_matches_hand_computation() {
        // block latency = 480 * (2 + PREFILL_BLOCKS) / 48_000 * 1000
        //               = 480 * 4 / 48_000 * 1000 = 40.0 ms; os_ms = 0.
        let ms = pipeline_latency_ms(480, 48_000, 0.0);
        assert!((ms - 40.0).abs() < 1e-9, "got {ms}");
    }

    #[test]
    fn pipeline_latency_adds_eq_group_delay() {
        // 4_800 os-latency frames at 48 kHz add exactly 100 ms on top.
        let base = pipeline_latency_ms(480, 48_000, 0.0);
        let with_os = pipeline_latency_ms(480, 48_000, 4_800.0);
        assert!((with_os - base - 100.0).abs() < 1e-9, "got {with_os}");
    }

    #[test]
    fn frame_aligned_chunk_rounds_down_to_whole_frames() {
        assert_eq!(frame_aligned_chunk(16_384, 1), 16_384);
        assert_eq!(frame_aligned_chunk(16_384, 2), 16_384);
        // 16_384 % 6 == 4, so it drops down to the nearest 6-multiple.
        assert_eq!(frame_aligned_chunk(16_384, 6), 16_380);
        assert_eq!(frame_aligned_chunk(10, 3), 9);
    }

    #[test]
    #[should_panic(expected = "exceeds callback scratch capacity")]
    fn frame_aligned_chunk_rejects_channels_larger_than_cap() {
        let _ = frame_aligned_chunk(8, 9);
    }

    #[test]
    fn count_clipped_counts_out_of_range_both_signs() {
        assert_eq!(count_clipped(&[]), 0);
        // ±1.0 is in range (inclusive), so nothing clips here.
        assert_eq!(count_clipped(&[0.0, 0.5, -0.5, 1.0, -1.0]), 0);
        assert_eq!(count_clipped(&[1.5, -1.5, 0.0, 2.0]), 3);
        // Non-finite samples are faulty output: NaN counts as clipped.
        assert_eq!(count_clipped(&[f32::NAN, 0.0]), 1);
    }

    #[test]
    fn block_frames_prefers_fixed_and_takes_the_max() {
        use cpal::BufferSize::{Default, Fixed};
        assert_eq!(block_frames(Fixed(256), Fixed(512), 128), 512);
        assert_eq!(block_frames(Fixed(256), Default, 128), 256);
        assert_eq!(block_frames(Default, Default, 128), 128);
    }

    #[test]
    fn startup_line_reflects_backend_and_oversample() {
        let df1 = startup_line(48_000, 2, 10, Backend::Df1, 1, 480, 40.0);
        assert_eq!(
            df1,
            "oxideq: 48000 Hz, 2 ch, 10 bands, block 480 frames (~40.0 ms pipeline latency)"
        );

        let df2_os = startup_line(44_100, 2, 8, Backend::Df2, 4, 256, 21.3);
        assert!(df2_os.contains(", DF2 biquads"));
        assert!(df2_os.contains(", 4x oversampled"));
        assert!(df2_os.contains("44100 Hz"));
    }

    /// Identity chain: 0 dB preamp, no bands, no oversampling — `process`
    /// leaves samples untouched, so ring contents can be compared to input.
    fn identity_chain() -> EqChain {
        EqChain::new(&Preset::default(), 48_000.0, 2, 1).unwrap()
    }

    #[test]
    fn process_input_block_pushes_all_chunks_in_order() {
        let mut chain = identity_chain();
        let (mut prod, mut cons) = HeapRb::<f32>::new(64).split();
        let stats = Stats::default();
        let data: Vec<f32> = (0..20).map(|i| (i as f32 / 32.0) - 0.3).collect();
        let mut scratch = [0.0f32; 8];

        // chunk_samples 8 < data.len() 20 forces three chunks (8+8+4).
        process_input_block(&data, 8, &mut scratch, &mut chain, &mut prod, &stats);

        let mut out = [0.0f32; 20];
        assert_eq!(cons.pop_slice(&mut out), 20);
        for (i, (got, want)) in out.iter().zip(&data).enumerate() {
            assert!((got - want).abs() < 1e-6, "sample {i}: {got} != {want}");
        }
        assert_eq!(stats.overruns.load(Ordering::Relaxed), 0);
        assert_eq!(stats.clipped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn process_input_block_counts_one_overrun_per_rejected_chunk() {
        let mut chain = identity_chain();
        // Ring holds 12: first 8-sample chunk fits, second fits only 4 of 8,
        // third (4 samples) is fully rejected — two overruns.
        let (mut prod, _cons) = HeapRb::<f32>::new(12).split();
        let stats = Stats::default();
        let data = [0.1f32; 20];
        let mut scratch = [0.0f32; 8];

        process_input_block(&data, 8, &mut scratch, &mut chain, &mut prod, &stats);

        assert_eq!(stats.overruns.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn process_input_block_counts_clipped_samples_after_eq() {
        let mut chain = identity_chain();
        let (mut prod, _cons) = HeapRb::<f32>::new(64).split();
        let stats = Stats::default();
        // Identity chain passes the out-of-range samples through unchanged.
        let data = [0.5f32, 1.5, -2.0, 0.0, 1.0, -1.0];
        let mut scratch = [0.0f32; 8];

        process_input_block(&data, 8, &mut scratch, &mut chain, &mut prod, &stats);

        assert_eq!(stats.clipped.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn fill_output_block_zero_fills_and_counts_underrun_when_ring_is_short() {
        let (mut prod, mut cons) = HeapRb::<f32>::new(16).split();
        let stats = Stats::default();
        prod.push_slice(&[0.25f32; 4]);
        // Sentinel garbage: the popped prefix must be overwritten, the
        // starved tail zeroed.
        let mut out = [9.9f32; 8];

        fill_output_block(&mut out, &mut cons, &stats);

        assert_eq!(&out[..4], &[0.25f32; 4]);
        assert_eq!(&out[4..], &[0.0f32; 4]);
        assert_eq!(stats.underruns.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn fill_output_block_pops_exactly_without_underrun() {
        let (mut prod, mut cons) = HeapRb::<f32>::new(16).split();
        let stats = Stats::default();
        prod.push_slice(&[0.5f32; 8]);
        let mut out = [9.9f32; 8];

        fill_output_block(&mut out, &mut cons, &stats);

        assert_eq!(out, [0.5f32; 8]);
        assert_eq!(stats.underruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn stats_warnings_reports_only_nonzero_categories() {
        assert!(stats_warnings(0, 0, 0).is_empty());

        let xruns = stats_warnings(3, 2, 0);
        assert_eq!(xruns.len(), 1);
        assert!(xruns[0].contains("3 underruns, 2 overruns"));

        let clip = stats_warnings(0, 0, 7);
        assert_eq!(clip.len(), 1);
        assert!(clip[0].contains("7 samples clipped"));

        assert_eq!(stats_warnings(1, 1, 1).len(), 2);
    }
}
