//! Hand-rolled CLI parsing — the dependency budget (PRD 4.2) does not
//! include a CLI crate, and four subcommands don't justify one.

use anyhow::{bail, Context, Result};

#[derive(Debug, PartialEq)]
pub enum Cmd {
    Run(RunArgs),
    Devices,
    InstallSink,
    Help,
}

#[derive(Debug, PartialEq)]
pub struct RunArgs {
    pub preset: String,
    pub input: Option<String>,
    pub output: Option<String>,
    pub buffer_frames: u32,
    pub auto_link: bool,
}

impl Default for RunArgs {
    fn default() -> Self {
        Self {
            preset: String::new(),
            input: None,
            output: None,
            buffer_frames: 256,
            auto_link: false,
        }
    }
}

pub const USAGE: &str = "\
oxideq — bit-perfect parametric EQ pipeline

USAGE:
  oxideq run --preset <file> [--input <name>] [--output <name>]
             [--buffer <frames>] [--auto-link]
  oxideq devices              list audio devices
  oxideq install-sink         create the PipeWire virtual sink (Linux)
  oxideq help

  --input/--output match device names case-insensitively by substring.
  --buffer sets the requested block size in frames (default 256).
  --auto-link wires sink monitor -> oxideq -> --output via pw-link (Linux).
";

pub fn parse(args: &[String]) -> Result<Cmd> {
    let mut it = args.iter().map(String::as_str);
    match it.next() {
        None | Some("help") | Some("--help") | Some("-h") => Ok(Cmd::Help),
        Some("devices") => Ok(Cmd::Devices),
        Some("install-sink") => Ok(Cmd::InstallSink),
        Some("run") => {
            let mut a = RunArgs::default();
            while let Some(flag) = it.next() {
                match flag {
                    "--preset" => a.preset = it.next().context("--preset needs a value")?.into(),
                    "--input" => a.input = Some(it.next().context("--input needs a value")?.into()),
                    "--output" => {
                        a.output = Some(it.next().context("--output needs a value")?.into())
                    }
                    "--buffer" => {
                        a.buffer_frames = it
                            .next()
                            .context("--buffer needs a value")?
                            .parse()
                            .context("--buffer must be a frame count")?
                    }
                    "--auto-link" => a.auto_link = true,
                    other => bail!("unknown flag {other:?}\n\n{USAGE}"),
                }
            }
            if a.preset.is_empty() {
                bail!("run requires --preset <file>\n\n{USAGE}");
            }
            Ok(Cmd::Run(a))
        }
        Some(other) => bail!("unknown command {other:?}\n\n{USAGE}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn no_args_is_help() {
        assert!(matches!(parse(&[]).unwrap(), Cmd::Help));
        assert!(matches!(parse(&s(&["--help"])).unwrap(), Cmd::Help));
    }

    #[test]
    fn devices_and_install_sink() {
        assert!(matches!(parse(&s(&["devices"])).unwrap(), Cmd::Devices));
        assert!(matches!(
            parse(&s(&["install-sink"])).unwrap(),
            Cmd::InstallSink
        ));
    }

    #[test]
    fn run_with_all_flags() {
        let cmd = parse(&s(&[
            "run",
            "--preset",
            "p.txt",
            "--input",
            "OxidEQ",
            "--output",
            "DAC",
            "--buffer",
            "512",
            "--auto-link",
        ]))
        .unwrap();
        let Cmd::Run(a) = cmd else {
            panic!("expected Run")
        };
        assert_eq!(a.preset, "p.txt");
        assert_eq!(a.input.as_deref(), Some("OxidEQ"));
        assert_eq!(a.output.as_deref(), Some("DAC"));
        assert_eq!(a.buffer_frames, 512);
        assert!(a.auto_link);
    }

    #[test]
    fn run_defaults() {
        let Cmd::Run(a) = parse(&s(&["run", "--preset", "p.txt"])).unwrap() else {
            panic!("expected Run")
        };
        assert_eq!(a.buffer_frames, 256);
        assert_eq!(a.input, None);
        assert_eq!(a.output, None);
        assert!(!a.auto_link);
    }

    #[test]
    fn run_without_preset_is_an_error() {
        assert!(parse(&s(&["run"])).is_err());
    }

    #[test]
    fn unknown_flag_is_an_error() {
        assert!(parse(&s(&["run", "--preset", "p.txt", "--frob"])).is_err());
        assert!(parse(&s(&["frobnicate"])).is_err());
    }
}
