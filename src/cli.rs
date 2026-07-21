//! CLI definition (clap derive). `Cli::parse()` handles help/version and
//! exits on bad input; tests go through `try_parse_from`.

use clap::{Args, Parser, Subcommand};

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
    /// Equalizer APO / AutoEQ preset file
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
}
