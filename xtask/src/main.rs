//! Futureboard workspace task runner (cargo-xtask pattern).
//!
//! Two responsibilities:
//!
//! * `build-all` / `check-all` — chain the per-edition cargo aliases from
//!   `.cargo/config.toml` (Cargo aliases cannot chain commands, and the two
//!   editions must build into separate target directories).
//! * `package` — build `FutureboardNative` and stage a clean, runnable
//!   application tree into `out/`, separate from the Cargo `target/` cache.
//!
//! Packaging deliberately lives here, not in `build.rs`: `build.rs` runs inside
//! every compilation and must stay hermetic, whereas packaging is an explicit,
//! post-build workflow that copies files, writes metadata and publishes output.

mod cargo_build;
mod cef;
mod metadata;
mod package;
mod platform;
mod plugins;
mod staging;
mod toolchain;
mod validation;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand};

use platform::Edition;

#[derive(Parser)]
#[command(
    name = "xtask",
    about = "Futureboard workspace task runner",
    disable_help_subcommand = true
)]
struct Cli {
    /// Explicit subcommand (`package`, `build-all`, `check-all`). Omit this
    /// to run `package` directly with the flags below, e.g.
    /// `cargo xtask --package community --plugins all`.
    #[command(subcommand)]
    command: Option<XtaskCommand>,

    #[command(flatten)]
    package: PackageArgs,
}

#[derive(Subcommand)]
enum XtaskCommand {
    /// Build and stage a runnable application into `out/`.
    Package(PackageArgs),

    /// Run `build-ce`, then `build-professional-win` (extra args forwarded).
    BuildAll {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Run `check-ce`, then `check-professional-win` (extra args forwarded).
    CheckAll {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(clap::Args)]
struct PackageArgs {
    /// Cargo profile to build (e.g. `dev`, `release`).
    #[arg(long, default_value = "dev")]
    profile: String,

    /// Cargo target triple (defaults to the host target).
    #[arg(long)]
    target: Option<String>,

    /// Which edition to build and stage. `--package` is accepted as an
    /// alias to match the `cargo build --package` mental model.
    #[arg(long, alias = "package", default_value = "community")]
    edition: Edition,

    /// Root output directory for staged packages.
    #[arg(long, default_value = "out")]
    out: PathBuf,

    /// Build and stage Built-in Plugin dynamic libraries into `Plugins/`.
    /// Accepts `all`, `none`, or a comma-separated list of plugin crate names
    /// (e.g. `rodharerist,equz8`). Off by default.
    #[arg(long, value_name = "SPEC")]
    plugin: Option<String>,

    /// Same as `--plugin`. Bare `--plugins` (no value) means `all`.
    #[arg(long, value_name = "SPEC", num_args = 0..=1, default_missing_value = "all")]
    plugins: Option<String>,

    /// Also copy debug symbols (`.pdb`) into a `symbols/` directory.
    #[arg(long)]
    symbols: bool,

    /// Intentionally package without the shared CEF runtime.
    #[arg(long)]
    no_cef: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(XtaskCommand::Package(args)) => run_package(args),
        Some(XtaskCommand::BuildAll { args }) => {
            run_aliases(&["build-ce", "build-professional-win"], &args)
        }
        Some(XtaskCommand::CheckAll { args }) => {
            run_aliases(&["check-ce", "check-professional-win"], &args)
        }
        None => run_package(cli.package),
    }
}

fn run_package(args: PackageArgs) -> ExitCode {
    let options = package::PackageOptions {
        profile: args.profile,
        target: args.target,
        edition: args.edition,
        out_root: args.out,
        symbols: args.symbols,
        plugins: package::PluginSelection::parse(
            args.plugin.as_deref().or(args.plugins.as_deref()),
            false,
        ),
        stage_cef: !args.no_cef,
    };
    match package::run(&options) {
        Ok(path) => {
            println!("Packaged into {}", path.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_aliases(aliases: &[&str], forwarded: &[String]) -> ExitCode {
    for alias in aliases {
        if let Err(code) = run_cargo_alias(alias, forwarded) {
            return code;
        }
    }
    ExitCode::SUCCESS
}

fn run_cargo_alias(alias: &str, forwarded: &[String]) -> Result<(), ExitCode> {
    // Run from the workspace root so the aliases in .cargo/config.toml resolve
    // regardless of where `cargo xtask` was invoked.
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    eprintln!("[xtask] cargo {} {}", alias, forwarded.join(" "));
    let mut command = Command::new(&cargo);
    command.arg(alias).args(forwarded).current_dir(root);

    // The professional aliases compile `asio-sys`, so they need the same SDK and
    // libclang the packaging path resolves. Without this, `cargo xtask
    // build-all` would still fall back to the %TEMP% download that breaks.
    if alias.contains("professional") {
        match toolchain::prepare_professional(Path::new(root)) {
            Ok(toolchain) => toolchain.apply(&mut command),
            Err(error) => {
                eprintln!("[xtask] {error:#}");
                return Err(ExitCode::FAILURE);
            }
        }
    }

    let status = command.status();
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => {
            eprintln!("[xtask] cargo {alias} failed with {status}");
            let code = status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .unwrap_or(1);
            Err(ExitCode::from(code))
        }
        Err(error) => {
            eprintln!("[xtask] failed to spawn `{cargo} {alias}`: {error}");
            Err(ExitCode::FAILURE)
        }
    }
}
