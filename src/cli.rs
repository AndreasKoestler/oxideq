//! CLI definition (clap derive). `Cli::parse()` handles help/version and
//! exits on bad input; tests go through `try_parse_from`.

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::dsp::Backend;

/// CLI mirror of [`Backend`] so `clap` can render `[possible values: …]`
/// and drive shell completion without coupling the `dsp` module to clap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendArg {
    /// Direct Form 1 biquad (default; bit-perfect at `--oversample 1`).
    #[value(name = "df1")]
    Df1,
    /// Direct Form 2 transposed biquad.
    #[value(name = "df2")]
    Df2,
}

impl From<BackendArg> for Backend {
    fn from(arg: BackendArg) -> Self {
        match arg {
            BackendArg::Df1 => Backend::Df1,
            BackendArg::Df2 => Backend::Df2,
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "oxideq", version, about = "Bit-perfect parametric EQ pipeline")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Run the EQ pipeline
    Run(RunArgs),
    /// List audio devices
    Devices,
}

#[derive(Debug, Args, PartialEq)]
pub struct RunArgs {
    /// Equalizer APO / `AutoEQ` preset file
    #[arg(long)]
    pub preset: String,
    /// Input device name substring, case-insensitive (default: system input)
    #[arg(long)]
    pub input: Option<String>,
    /// Output device name substring, case-insensitive (default: system output)
    #[arg(long)]
    pub output: Option<String>,
    /// Requested block size in frames
    #[arg(long = "buffer", value_name = "FRAMES", default_value_t = 256)]
    pub buffer_frames: u32,
    /// Oversample the EQ cascade by this factor (1 = off, bit-perfect)
    #[arg(long, default_value_t = 1, value_parser = parse_oversample)]
    pub oversample: usize,
    /// Biquad realization to run the cascade with
    #[arg(long, value_enum, default_value = "df1", ignore_case = true)]
    pub backend: BackendArg,
}

fn parse_oversample(s: &str) -> Result<usize, String> {
    match s.parse::<usize>() {
        Ok(n @ (1 | 2 | 4 | 8 | 16)) => Ok(n),
        Ok(n) => Err(format!("must be 1, 2, 4, 8, or 16 (got {n})")),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("oxideq").chain(args.iter().copied()))
    }

    #[test]
    fn devices_command() {
        assert!(matches!(parse(&["devices"]).unwrap().cmd, Cmd::Devices));
    }

    #[test]
    fn run_with_all_flags() {
        let Cmd::Run(a) = parse(&[
            "run", "--preset", "p.txt", "--input", "OxidEQ", "--output", "DAC", "--buffer", "512",
        ])
        .unwrap()
        .cmd
        else {
            panic!("expected Run")
        };
        assert_eq!(a.preset, "p.txt");
        assert_eq!(a.input.as_deref(), Some("OxidEQ"));
        assert_eq!(a.output.as_deref(), Some("DAC"));
        assert_eq!(a.buffer_frames, 512);
    }

    #[test]
    fn run_defaults() {
        let Cmd::Run(a) = parse(&["run", "--preset", "p.txt"]).unwrap().cmd else {
            panic!("expected Run")
        };
        assert_eq!(a.buffer_frames, 256);
        assert_eq!(a.input, None);
        assert_eq!(a.output, None);
    }

    #[test]
    fn run_without_preset_is_an_error() {
        assert!(parse(&["run"]).is_err());
    }

    #[test]
    fn unknown_flag_is_an_error() {
        assert!(parse(&["run", "--preset", "p.txt", "--frob"]).is_err());
        assert!(parse(&["frobnicate"]).is_err());
    }

    #[test]
    fn oversample_accepts_powers_of_two_and_defaults_to_one() {
        for n in [1usize, 2, 4, 8, 16] {
            let Cmd::Run(a) = parse(&["run", "--preset", "p.txt", "--oversample", &n.to_string()])
                .unwrap()
                .cmd
            else {
                panic!("expected Run")
            };
            assert_eq!(a.oversample, n);
        }
        let Cmd::Run(a) = parse(&["run", "--preset", "p.txt"]).unwrap().cmd else {
            panic!("expected Run")
        };
        assert_eq!(a.oversample, 1);
    }

    #[test]
    fn oversample_rejects_everything_else() {
        for bad in ["0", "3", "5", "32", "-2", "two"] {
            assert!(
                parse(&["run", "--preset", "p.txt", "--oversample", bad]).is_err(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn backend_accepts_df1_df2_and_defaults_to_df1() {
        for (arg, want) in [
            ("df1", BackendArg::Df1),
            ("df2", BackendArg::Df2),
            ("DF2", BackendArg::Df2), // case-insensitive
        ] {
            let Cmd::Run(a) = parse(&["run", "--preset", "p.txt", "--backend", arg])
                .unwrap()
                .cmd
            else {
                panic!("expected Run")
            };
            assert_eq!(a.backend, want, "--backend {arg}");
        }
        let Cmd::Run(a) = parse(&["run", "--preset", "p.txt"]).unwrap().cmd else {
            panic!("expected Run")
        };
        assert_eq!(a.backend, BackendArg::Df1, "default backend");
    }

    #[test]
    fn backend_rejects_unknown() {
        for bad in ["df3", "direct", "", "1"] {
            assert!(
                parse(&["run", "--preset", "p.txt", "--backend", bad]).is_err(),
                "{bad:?} must be rejected"
            );
        }
    }
}
