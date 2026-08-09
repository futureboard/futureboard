use std::path::PathBuf;

use apak::{
    CreateOptions, InstallOptions, InstallRoots, PackOptions, PackageTarget, SignedInstallOptions,
    SignedPackOptions, create_package_template, ensure_secret_file, install_package,
    install_signed_package, load_signing_key, pack_signed_template, pack_template,
    read_package_info, read_signed_package_info, write_template,
};
use clap::{Parser, Subcommand, ValueEnum};

use crate::platform::{ELEVATED_WARNING_CLI, is_process_elevated};

#[derive(Parser, Debug)]
#[command(name = "apak", version, about = ".apak audio package installer")]
pub struct ApakArgs {
    #[command(subcommand)]
    command: ApakCommand,
}

#[derive(Subcommand, Debug)]
enum ApakCommand {
    Init {
        #[arg(default_value = ".")]
        directory: PathBuf,
    },
    Create {
        directory: PathBuf,
        #[arg(long = "type", value_enum)]
        package_type: ApakPackageType,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "0.1.0")]
        version: String,
        #[arg(long, default_value = "Local")]
        publisher: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, default_value = "Proprietary")]
        license: String,
    },
    Pack {
        source: PathBuf,
        output: PathBuf,
        #[arg(long, value_name = "FILE", conflicts_with = "secret_file")]
        signing_key: Option<PathBuf>,
        #[arg(
            long,
            value_name = "FILE",
            help = "Build a legacy encrypted v1 package instead of a signed package"
        )]
        secret_file: Option<PathBuf>,
    },
    Install {
        package: PathBuf,
        #[arg(long, value_name = "FILE", help = "Open a legacy encrypted v1 package")]
        secret_file: Option<PathBuf>,
    },
    Info {
        package: PathBuf,
        #[arg(long, value_name = "FILE", help = "Open a legacy encrypted v1 package")]
        secret_file: Option<PathBuf>,
    },
    Roots,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum ApakPackageType {
    #[value(alias = "samples")]
    Sample,
    #[value(alias = "presets")]
    Preset,
    #[value(alias = "plugins")]
    Plugin,
    #[value(alias = "themes")]
    Theme,
    #[value(alias = "services")]
    Service,
    #[value(alias = "extensions", alias = "extention", alias = "extentions")]
    Extension,
}

impl From<ApakPackageType> for PackageTarget {
    fn from(value: ApakPackageType) -> Self {
        match value {
            ApakPackageType::Sample => Self::Sample,
            ApakPackageType::Preset => Self::Preset,
            ApakPackageType::Plugin => Self::Plugin,
            ApakPackageType::Theme => Self::Theme,
            ApakPackageType::Service => Self::Service,
            ApakPackageType::Extension => Self::Extentions,
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "makeapak", version, about = "Build a .apak package")]
pub struct MakeApakArgs {
    source: PathBuf,
    output: PathBuf,
    #[arg(long, value_name = "FILE", conflicts_with = "secret_file")]
    signing_key: Option<PathBuf>,
    #[arg(
        long,
        value_name = "FILE",
        help = "Build a legacy encrypted v1 package instead of a signed package"
    )]
    secret_file: Option<PathBuf>,
}

pub fn run_apak_cli() -> i32 {
    warn_if_elevated();
    match run_apak_command(ApakArgs::parse()) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("apak: {error}");
            1
        }
    }
}

pub fn run_makeapak_cli() -> i32 {
    warn_if_elevated();
    match run_makeapak_command(MakeApakArgs::parse()) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("makeapak: {error}");
            1
        }
    }
}

fn warn_if_elevated() {
    if is_process_elevated() {
        eprintln!("{ELEVATED_WARNING_CLI}");
    }
}

fn run_apak_command(args: ApakArgs) -> apak::Result<()> {
    match args.command {
        ApakCommand::Init { directory } => {
            write_template(&directory)?;
            println!("Template written to {}", directory.display());
        }
        ApakCommand::Create {
            directory,
            package_type,
            id,
            name,
            version,
            publisher,
            description,
            license,
        } => {
            create(
                directory,
                package_type.into(),
                id,
                name,
                version,
                publisher,
                description,
                license,
            )?;
        }
        ApakCommand::Pack {
            source,
            output,
            signing_key,
            secret_file,
        } => {
            pack(source, output, signing_key, secret_file)?;
        }
        ApakCommand::Install {
            package,
            secret_file,
        } => {
            install(package, secret_file)?;
        }
        ApakCommand::Info {
            package,
            secret_file,
        } => {
            info(package, secret_file)?;
        }
        ApakCommand::Roots => {
            let roots = InstallRoots::default_user()?;
            println!("Samples: {}", roots.samples.display());
            println!("Presets: {}", roots.presets.display());
            println!("Extensions: {}", roots.extentions.display());
        }
    }
    Ok(())
}

fn run_makeapak_command(args: MakeApakArgs) -> apak::Result<()> {
    pack(args.source, args.output, args.signing_key, args.secret_file)
}

#[allow(clippy::too_many_arguments)]
fn create(
    directory: PathBuf,
    target: PackageTarget,
    id: Option<String>,
    name: Option<String>,
    version: String,
    publisher: String,
    description: String,
    license: String,
) -> apak::Result<()> {
    let name = resolve_package_name(&directory, name)?;
    let id = id.unwrap_or_else(|| format!("local.{}", package_id_slug(&name)));

    create_package_template(CreateOptions {
        destination: directory.clone(),
        target,
        id: id.clone(),
        name: name.clone(),
        version,
        publisher,
        description,
        license,
    })?;

    println!(
        "Created {target} package template at {}",
        directory.display()
    );
    println!("Package: {name} ({id})");
    println!(
        "Add package files to {}",
        directory.join("assets").display()
    );
    println!(
        "Pack with: apak pack \"{}\" \"{}.apak\"",
        directory.display(),
        package_id_slug(&name)
    );
    Ok(())
}

fn resolve_package_name(
    directory: &std::path::Path,
    explicit_name: Option<String>,
) -> apak::Result<String> {
    if let Some(name) = explicit_name {
        return Ok(name);
    }
    directory
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            apak::ApakError::InvalidTemplate(format!(
                "could not derive a package name from {} (use --name)",
                directory.display()
            ))
        })
}

fn package_id_slug(name: &str) -> String {
    let mut slug = String::new();
    let mut separator_pending = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            if separator_pending && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
            separator_pending = false;
        } else {
            separator_pending = true;
        }
    }
    if slug.is_empty() {
        "package".to_string()
    } else {
        slug
    }
}

fn pack(
    source: PathBuf,
    output: PathBuf,
    signing_key: Option<PathBuf>,
    secret_file: Option<PathBuf>,
) -> apak::Result<()> {
    let (report, format) = if let Some(secret_file) = secret_file {
        if ensure_secret_file(&secret_file)? {
            println!("Generated legacy APAK secret: {}", secret_file.display());
        }
        (
            pack_template(PackOptions {
                source_dir: source,
                output_path: output,
                secret_file,
            })?,
            "legacy encrypted v1",
        )
    } else {
        let signing_key = load_signing_key(signing_key.as_deref())?;
        (
            pack_signed_template(SignedPackOptions {
                source_dir: source,
                output_path: output,
                signing_key,
            })?,
            "Ed25519 signed v2",
        )
    };

    print_summary(&report.summary);
    println!("Format: {format}");
    println!("Assets: {}", report.asset_count);
    println!("Output: {}", report.output_path.display());
    println!("Bytes: {}", report.byte_len);
    Ok(())
}

fn info(package: PathBuf, secret_file: Option<PathBuf>) -> apak::Result<()> {
    let (summary, signature_verified) = if let Some(secret_file) = secret_file {
        (read_package_info(&package, &secret_file)?, false)
    } else {
        (
            read_signed_package_info(&package, &crate::bundled_verifying_key()?)?,
            true,
        )
    };
    print_summary(&summary);
    if signature_verified {
        println!("Signature: verified");
    }
    Ok(())
}

fn install(package: PathBuf, secret_file: Option<PathBuf>) -> apak::Result<()> {
    let roots = InstallRoots::default_user()?;
    let (report, signature_verified) = if let Some(secret_file) = secret_file {
        (
            install_package(InstallOptions {
                package_path: package,
                secret_file,
                roots,
            })?,
            false,
        )
    } else {
        (
            install_signed_package(SignedInstallOptions {
                package_path: package,
                verifying_key: crate::bundled_verifying_key()?,
                roots,
            })?,
            true,
        )
    };
    print_summary(&report.summary);
    if signature_verified {
        println!("Signature: verified");
    }
    println!("Installed files: {}", report.installed_files.len());
    for path in report.installed_files {
        println!("  {}", path.display());
    }
    Ok(())
}

fn print_summary(summary: &apak::PackageSummary) {
    println!("Package: {} ({})", summary.name, summary.id);
    println!("Version: {}", summary.version);
    println!("Target: {}", summary.target);
    println!("Publisher: {}", summary.publisher);
    println!("License: {}", summary.license);
    if !summary.description.trim().is_empty() {
        println!("Description: {}", summary.description);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_command_accepts_plural_plugin_type() {
        let args = ApakArgs::try_parse_from([
            "apak",
            "create",
            "my-plugin",
            "--type",
            "plugins",
            "--id",
            "com.example.my-plugin",
        ])
        .expect("create args");

        match args.command {
            ApakCommand::Create {
                directory,
                package_type,
                id,
                ..
            } => {
                assert_eq!(directory, PathBuf::from("my-plugin"));
                assert_eq!(package_type, ApakPackageType::Plugin);
                assert_eq!(id.as_deref(), Some("com.example.my-plugin"));
            }
            command => panic!("expected create command, got {command:?}"),
        }
    }

    #[test]
    fn pack_command_defaults_to_signed_format() {
        let args =
            ApakArgs::try_parse_from(["apak", "pack", "source", "output.apak"]).expect("pack args");

        match args.command {
            ApakCommand::Pack {
                signing_key,
                secret_file,
                ..
            } => {
                assert!(signing_key.is_none());
                assert!(secret_file.is_none());
            }
            command => panic!("expected pack command, got {command:?}"),
        }
    }

    #[test]
    fn pack_command_rejects_signed_and_legacy_keys_together() {
        let error = ApakArgs::try_parse_from([
            "apak",
            "pack",
            "source",
            "output.apak",
            "--signing-key",
            "signed.key",
            "--secret-file",
            ".apak.secret",
        ])
        .expect_err("key options must conflict");

        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn explicit_name_does_not_require_a_directory_name() {
        assert_eq!(
            resolve_package_name(std::path::Path::new("."), Some("My Plugin".to_string()))
                .expect("explicit name"),
            "My Plugin"
        );
    }

    #[test]
    fn package_id_slug_is_cli_safe() {
        assert_eq!(package_id_slug("My Great_Plugin!"), "my-great-plugin");
        assert_eq!(package_id_slug("ปลั๊กอิน"), "package");
    }
}
