//! Local command-line frontend.

use std::{env, ffi::OsStr, process::ExitCode};

use investigation_core::PROGRAM_NAME;

const HELP: &str =
    "Record and resume evidence-based investigations.\n\nUsage: investigation [--help | --version]";

fn run(args: &[impl AsRef<OsStr>]) -> Result<&'static str, ()> {
    match args {
        [] => Ok(HELP),
        [arg] => match arg.as_ref().to_str() {
            Some("-h" | "--help") => Ok(HELP),
            Some("-V" | "--version") => Ok(concat!("investigation ", env!("CARGO_PKG_VERSION"))),
            _ => Err(()),
        },
        _ => Err(()),
    }
}

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    match run(&args) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(()) => {
            eprintln!("Usage: {PROGRAM_NAME} [--help | --version]");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn accepts_help_and_version_and_rejects_unknown_arguments() {
        assert_eq!(super::run(&["--help"]), Ok(super::HELP));
        assert!(
            super::run(&["--version"])
                .unwrap()
                .starts_with("investigation ")
        );
        assert!(super::run(&["--missing"]).is_err());
        assert!(super::run(&["--help", "--version"]).is_err());
    }
}
