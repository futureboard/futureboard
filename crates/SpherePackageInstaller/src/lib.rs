use std::fmt;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};

use aes_gcm::aead::rand_core::{OsRng, RngCore};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use pbkdf2::pbkdf2_hmac;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

const MAGIC: &[u8; 5] = b"APAK\0";
const FORMAT_VERSION: u8 = 1;
const SIGNED_FORMAT_VERSION: u8 = 2;
const SIGNATURE_LEN: usize = 64;
const SIGNED_MESSAGE_DOMAIN: &[u8] = b"Futureboard APAK Ed25519 signed package v2\0";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const KDF_ROUNDS: u32 = 210_000;

#[derive(Debug, thiserror::Error)]
pub enum ApakError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("TOML encode error: {0}")]
    TomlEncode(#[from] toml::ser::Error),
    #[error("archive error: {0}")]
    Archive(String),
    #[error("compression error: {0}")]
    Compression(String),
    #[error("crypto error: {0}")]
    Crypto(String),
    #[error("signature/key error: {0}")]
    Signature(String),
    #[error("invalid .apak package: {0}")]
    InvalidPackage(String),
    #[error("invalid package template: {0}")]
    InvalidTemplate(String),
    #[error("unsupported package target: {0}")]
    UnsupportedTarget(String),
}

pub type Result<T> = std::result::Result<T, ApakError>;

#[derive(Clone)]
pub struct ApakSigningKey(SigningKey);

impl fmt::Debug for ApakSigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApakSigningKey([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ApakVerifyingKey(VerifyingKey);

impl fmt::Debug for ApakVerifyingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApakVerifyingKey([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackageTarget {
    #[serde(alias = "Samples")]
    Sample,
    #[serde(alias = "Presets")]
    Preset,
    #[serde(alias = "Plugins")]
    Plugin,
    #[serde(alias = "Themes")]
    Theme,
    #[serde(alias = "Services")]
    Service,
    #[serde(
        rename = "Extensions",
        alias = "Extension",
        alias = "Extention",
        alias = "Extentions"
    )]
    Extentions,
}

impl fmt::Display for PackageTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sample => f.write_str("Sample"),
            Self::Preset => f.write_str("Preset"),
            Self::Plugin => f.write_str("Plugin"),
            Self::Theme => f.write_str("Theme"),
            Self::Service => f.write_str("Service"),
            Self::Extentions => f.write_str("Extensions"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallToml {
    pub package: PackageSection,
    #[serde(default)]
    pub install: InstallSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSection {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(rename = "type")]
    pub target: PackageTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallSection {
    #[serde(default = "default_overwrite")]
    pub overwrite: bool,
}

impl Default for InstallSection {
    fn default() -> Self {
        Self {
            overwrite: default_overwrite(),
        }
    }
}

fn default_overwrite() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataToml {
    pub metadata: MetadataSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataSection {
    pub publisher: String,
    pub description: String,
    pub license: String,
}

#[derive(Debug, Clone)]
pub struct PackageSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub target: PackageTarget,
    pub publisher: String,
    pub description: String,
    pub license: String,
}

impl PackageSummary {
    fn from_manifests(install: &InstallToml, metadata: &MetadataToml) -> Self {
        Self {
            id: install.package.id.clone(),
            name: install.package.name.clone(),
            version: install.package.version.clone(),
            target: install.package.target,
            publisher: metadata.metadata.publisher.clone(),
            description: metadata.metadata.description.clone(),
            license: metadata.metadata.license.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallRoots {
    pub samples: PathBuf,
    pub presets: PathBuf,
    pub extentions: PathBuf,
}

impl InstallRoots {
    pub fn default_user() -> Result<Self> {
        let documents = dirs::document_dir()
            .ok_or_else(|| ApakError::InvalidPackage("could not resolve Documents".to_string()))?;
        let config = dirs::config_dir()
            .ok_or_else(|| ApakError::InvalidPackage("could not resolve AppData".to_string()))?;

        let user_root = documents.join("Futureboard Studio");
        Ok(Self {
            samples: user_root.join("Samples"),
            presets: user_root.join("Presets"),
            extentions: config.join("Futureboard Studio").join("Extensions"),
        })
    }
}

#[derive(Debug, Clone)]
pub struct PackOptions {
    pub source_dir: PathBuf,
    pub output_path: PathBuf,
    pub secret_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SignedPackOptions {
    pub source_dir: PathBuf,
    pub output_path: PathBuf,
    pub signing_key: ApakSigningKey,
}

#[derive(Debug, Clone)]
pub struct CreateOptions {
    pub destination: PathBuf,
    pub target: PackageTarget,
    pub id: String,
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub description: String,
    pub license: String,
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub package_path: PathBuf,
    pub secret_file: PathBuf,
    pub roots: InstallRoots,
}

#[derive(Debug, Clone)]
pub struct SignedInstallOptions {
    pub package_path: PathBuf,
    pub verifying_key: ApakVerifyingKey,
    pub roots: InstallRoots,
}

#[derive(Debug, Clone)]
pub struct PackReport {
    pub summary: PackageSummary,
    pub output_path: PathBuf,
    pub asset_count: usize,
    pub byte_len: u64,
}

#[derive(Debug, Clone)]
pub struct InstallReport {
    pub summary: PackageSummary,
    pub installed_files: Vec<PathBuf>,
}

pub fn default_secret_file() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".apak.secret")
}

pub fn default_signing_key_file() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("signed.key")
}

pub fn parse_signing_key(value: &str) -> Result<ApakSigningKey> {
    Ok(ApakSigningKey(SigningKey::from_bytes(&decode_raw_key(
        value,
        "signing key",
    )?)))
}

pub fn parse_verifying_key(value: &str) -> Result<ApakVerifyingKey> {
    let bytes = decode_raw_key(value, "verifying key")?;
    let key = VerifyingKey::from_bytes(&bytes)
        .map_err(|error| ApakError::Signature(format!("invalid Ed25519 verifying key: {error}")))?;
    Ok(ApakVerifyingKey(key))
}

pub fn read_signing_key_file(path: &Path) -> Result<ApakSigningKey> {
    let contents = fs::read_to_string(path).map_err(|error| {
        ApakError::Signature(format!(
            "could not read signing key file {}: {error}",
            path.display()
        ))
    })?;
    let value = signing_key_value_from_file(&contents).ok_or_else(|| {
        ApakError::Signature(format!("signing key file {} is empty", path.display()))
    })?;
    parse_signing_key(value).map_err(|error| match error {
        ApakError::Signature(message) => ApakError::Signature(format!(
            "invalid signing key in {}: {message}",
            path.display()
        )),
        other => other,
    })
}

pub fn load_signing_key(explicit_file: Option<&Path>) -> Result<ApakSigningKey> {
    if let Some(path) = explicit_file {
        return read_signing_key_file(path);
    }

    match std::env::var("APAK_SIGNING_KEY") {
        Ok(value) => return parse_signing_key(&value),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(ApakError::Signature(
                "APAK_SIGNING_KEY contains non-Unicode data".to_string(),
            ));
        }
        Err(std::env::VarError::NotPresent) => {}
    }

    let current_dir = std::env::current_dir().map_err(|error| {
        ApakError::Signature(format!(
            "could not resolve the current directory while locating a signing key: {error}"
        ))
    })?;
    let dotenv_path = current_dir.join(".env");
    if let Some(value) = signing_key_value_from_dotenv(&dotenv_path)? {
        return parse_signing_key(&value).map_err(|error| match error {
            ApakError::Signature(message) => ApakError::Signature(format!(
                "invalid APAK_SIGNING_KEY in {}: {message}",
                dotenv_path.display()
            )),
            other => other,
        });
    }

    let default_file = current_dir.join("signed.key");
    if default_file.is_file() {
        return read_signing_key_file(&default_file);
    }

    Err(ApakError::Signature(format!(
        "no signing key found; pass an explicit key file, set APAK_SIGNING_KEY, add APAK_SIGNING_KEY to {}, or create {} containing a 32-byte key encoded as 64-character hex or base64; signing keys are never generated automatically",
        dotenv_path.display(),
        default_file.display()
    )))
}

pub fn verifying_key_value_from_signing_key_value(signing_key: &str) -> Result<String> {
    let signing_key = parse_signing_key(signing_key)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(signing_key.0.verifying_key().to_bytes()))
}

pub fn generate_secret_value() -> String {
    let mut secret = [0u8; KEY_LEN];
    OsRng.fill_bytes(&mut secret);
    base64::engine::general_purpose::STANDARD.encode(secret)
}

pub fn ensure_secret_file(path: &Path) -> Result<bool> {
    if path.exists() {
        let _ = read_secret_file(path)?;
        return Ok(false);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("APAK_SECRET={}\n", generate_secret_value()))?;
    Ok(true)
}

pub fn pack_template(options: PackOptions) -> Result<PackReport> {
    let (summary, compressed, asset_count) = build_compressed_template(&options.source_dir)?;
    let secret = read_secret_file(&options.secret_file)?;
    let package_bytes = encrypt_payload(&compressed, &secret)?;
    write_pack_report(summary, options.output_path, asset_count, &package_bytes)
}

pub fn pack_signed_template(options: SignedPackOptions) -> Result<PackReport> {
    let (summary, compressed, asset_count) = build_compressed_template(&options.source_dir)?;
    let package_bytes = sign_payload(&compressed, &options.signing_key);
    write_pack_report(summary, options.output_path, asset_count, &package_bytes)
}

pub fn read_package_info(package_path: &Path, secret_file: &Path) -> Result<PackageSummary> {
    let payload = decrypt_package(package_path, secret_file)?;
    read_package_summary(&payload)
}

pub fn read_signed_package_info(
    package_path: &Path,
    verifying_key: &ApakVerifyingKey,
) -> Result<PackageSummary> {
    let payload = read_signed_package(package_path, verifying_key)?;
    read_package_summary(&payload)
}

pub fn install_package(options: InstallOptions) -> Result<InstallReport> {
    let payload = decrypt_package(&options.package_path, &options.secret_file)?;
    install_payload(payload, &options.roots)
}

pub fn install_signed_package(options: SignedInstallOptions) -> Result<InstallReport> {
    let payload = read_signed_package(&options.package_path, &options.verifying_key)?;
    install_payload(payload, &options.roots)
}

pub fn create_package_template(options: CreateOptions) -> Result<()> {
    ensure_empty_destination(&options.destination)?;

    let install = InstallToml {
        package: PackageSection {
            id: options.id,
            name: options.name,
            version: options.version,
            target: options.target,
        },
        install: InstallSection::default(),
    };
    validate_install(&install)?;
    let metadata = MetadataToml {
        metadata: MetadataSection {
            publisher: options.publisher,
            description: options.description,
            license: options.license,
        },
    };

    fs::create_dir_all(options.destination.join("assets"))?;
    fs::write(options.destination.join("assets").join(".gitkeep"), "")?;
    fs::write(
        options.destination.join("install.toml"),
        toml::to_string_pretty(&install)?,
    )?;
    fs::write(
        options.destination.join("metadata.toml"),
        toml::to_string_pretty(&metadata)?,
    )?;
    fs::write(options.destination.join("README.md"), TEMPLATE_README)?;
    Ok(())
}

pub fn write_template(destination: &Path) -> Result<()> {
    fs::create_dir_all(destination.join("assets"))?;
    write_if_missing(destination.join("assets").join(".gitkeep"), "")?;
    write_if_missing(destination.join("install.toml"), TEMPLATE_INSTALL_TOML)?;
    write_if_missing(destination.join("metadata.toml"), TEMPLATE_METADATA_TOML)?;
    write_if_missing(destination.join("README.md"), TEMPLATE_README)?;
    Ok(())
}

fn ensure_empty_destination(destination: &Path) -> Result<()> {
    if !destination.exists() {
        return Ok(());
    }
    if !destination.is_dir() {
        return Err(ApakError::InvalidTemplate(format!(
            "destination is not a directory: {}",
            destination.display()
        )));
    }
    if fs::read_dir(destination)?.next().transpose()?.is_some() {
        return Err(ApakError::InvalidTemplate(format!(
            "destination is not empty: {}",
            destination.display()
        )));
    }
    Ok(())
}

fn write_if_missing(path: PathBuf, contents: &str) -> Result<()> {
    if !path.exists() {
        fs::write(path, contents)?;
    }
    Ok(())
}

fn read_install_toml(path: &Path) -> Result<InstallToml> {
    Ok(toml::from_str(&fs::read_to_string(path)?)?)
}

fn read_metadata_toml(path: &Path) -> Result<MetadataToml> {
    Ok(toml::from_str(&fs::read_to_string(path)?)?)
}

fn validate_install(install: &InstallToml) -> Result<()> {
    let fields = [
        ("package.id", install.package.id.as_str()),
        ("package.name", install.package.name.as_str()),
        ("package.version", install.package.version.as_str()),
    ];
    for (field, value) in fields {
        if value.trim().is_empty() {
            return Err(ApakError::InvalidTemplate(format!("{field} is empty")));
        }
    }
    Ok(())
}

fn build_compressed_template(source_dir: &Path) -> Result<(PackageSummary, Vec<u8>, usize)> {
    let install_path = source_dir.join("install.toml");
    let metadata_path = source_dir.join("metadata.toml");
    let assets_dir = source_dir.join("assets");

    if !install_path.is_file() {
        return Err(ApakError::InvalidTemplate(
            "missing install.toml".to_string(),
        ));
    }
    if !metadata_path.is_file() {
        return Err(ApakError::InvalidTemplate(
            "missing metadata.toml".to_string(),
        ));
    }
    if !assets_dir.is_dir() {
        return Err(ApakError::InvalidTemplate(
            "missing assets directory".to_string(),
        ));
    }

    let install = read_install_toml(&install_path)?;
    let metadata = read_metadata_toml(&metadata_path)?;
    validate_install(&install)?;
    let summary = PackageSummary::from_manifests(&install, &metadata);

    let (tar_bytes, asset_count) =
        build_tar_payload(source_dir, &install_path, &metadata_path, &assets_dir)?;
    if asset_count == 0 {
        return Err(ApakError::InvalidTemplate(
            "assets directory contains no files".to_string(),
        ));
    }

    Ok((summary, lzma_compress(&tar_bytes)?, asset_count))
}

fn write_pack_report(
    summary: PackageSummary,
    output_path: PathBuf,
    asset_count: usize,
    package_bytes: &[u8],
) -> Result<PackReport> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output_path, package_bytes)?;
    let byte_len = fs::metadata(&output_path)?.len();

    Ok(PackReport {
        summary,
        output_path,
        asset_count,
        byte_len,
    })
}

fn build_tar_payload(
    source_dir: &Path,
    install_path: &Path,
    metadata_path: &Path,
    assets_dir: &Path,
) -> Result<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut builder = tar::Builder::new(&mut out);
    builder.append_path_with_name(install_path, "install.toml")?;
    builder.append_path_with_name(metadata_path, "metadata.toml")?;

    let placeholder_path = assets_dir.join(".gitkeep");
    let mut files = Vec::new();
    for entry in WalkDir::new(assets_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() && path != placeholder_path {
            files.push(path.to_path_buf());
        }
    }
    files.sort();

    let mut count = 0usize;
    for path in files {
        let rel = path.strip_prefix(source_dir).map_err(|error| {
            ApakError::InvalidTemplate(format!("could not relativize asset path: {error}"))
        })?;
        let rel = normalize_archive_path(rel)?;
        builder.append_path_with_name(&path, rel)?;
        count += 1;
    }
    builder.finish()?;
    drop(builder);
    Ok((out, count))
}

fn normalize_archive_path(path: &Path) -> Result<PathBuf> {
    let safe = safe_components(path)?;
    Ok(safe.iter().collect())
}

fn safe_components(path: &Path) -> Result<Vec<PathBuf>> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(PathBuf::from(part)),
            Component::CurDir => {}
            _ => {
                return Err(ApakError::InvalidPackage(format!(
                    "unsafe path {}",
                    path.display()
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(ApakError::InvalidPackage("empty archive path".to_string()));
    }
    Ok(parts)
}

fn asset_relative_path(path: &Path) -> Result<Option<PathBuf>> {
    let parts = safe_components(path)?;
    let Some(first) = parts.first() else {
        return Ok(None);
    };
    if first.as_os_str() != "assets" {
        return Ok(None);
    }
    if parts.len() == 1 {
        return Ok(None);
    }
    Ok(Some(parts[1..].iter().collect()))
}

fn resolve_install_target(
    install: &InstallToml,
    roots: &InstallRoots,
    asset_rel: &Path,
) -> Result<PathBuf> {
    let parts = safe_components(asset_rel)?;
    let rel: PathBuf = parts.iter().collect();
    match install.package.target {
        PackageTarget::Sample => Ok(roots.samples.join(rel)),
        PackageTarget::Preset => Ok(roots.presets.join(rel)),
        PackageTarget::Plugin => Ok(roots.extentions.join("Plugins").join(rel)),
        PackageTarget::Theme => Ok(roots.extentions.join("Themes").join(rel)),
        PackageTarget::Service => Ok(roots.extentions.join("Services").join(rel)),
        PackageTarget::Extentions => {
            let first = parts
                .first()
                .and_then(|part| part.to_str())
                .unwrap_or_default();
            match first {
                "Themes" | "Plugins" | "Services" => Ok(roots.extentions.join(rel)),
                _ => Err(ApakError::UnsupportedTarget(format!(
                    "Extensions assets must start with Themes, Plugins, or Services: {}",
                    asset_rel.display()
                ))),
            }
        }
    }
}

fn read_package_summary(payload: &[u8]) -> Result<PackageSummary> {
    let (install, metadata) = read_manifests_from_tar(payload)?;
    validate_install(&install)?;
    Ok(PackageSummary::from_manifests(&install, &metadata))
}

fn install_payload(payload: Vec<u8>, roots: &InstallRoots) -> Result<InstallReport> {
    let (install, metadata) = read_manifests_from_tar(&payload)?;
    validate_install(&install)?;
    let summary = PackageSummary::from_manifests(&install, &metadata);

    let mut archive = tar::Archive::new(Cursor::new(payload));
    let mut installed_files = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let Some(asset_rel) = asset_relative_path(&path)? else {
            continue;
        };

        if entry.header().entry_type().is_dir() {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(ApakError::Archive(format!(
                "unsupported archive entry type for {}",
                path.display()
            )));
        }

        let target = resolve_install_target(&install, roots, &asset_rel)?;
        if target.exists() && !install.install.overwrite {
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        entry.unpack(&target)?;
        installed_files.push(target);
    }

    Ok(InstallReport {
        summary,
        installed_files,
    })
}

fn decrypt_package(package_path: &Path, secret_file: &Path) -> Result<Vec<u8>> {
    let bytes = fs::read(package_path)?;
    let compressed = decrypt_payload(&bytes, &read_secret_file(secret_file)?)?;
    lzma_decompress(&compressed)
}

fn read_signed_package(package_path: &Path, verifying_key: &ApakVerifyingKey) -> Result<Vec<u8>> {
    let bytes = fs::read(package_path)?;
    let compressed = verify_signed_payload(&bytes, verifying_key)?;
    lzma_decompress(compressed)
}

fn sign_payload(payload: &[u8], signing_key: &ApakSigningKey) -> Vec<u8> {
    let message = signed_message(payload);
    let signature = signing_key.0.sign(&message);
    let mut out = Vec::with_capacity(MAGIC.len() + 1 + SIGNATURE_LEN + payload.len());
    out.extend_from_slice(MAGIC);
    out.push(SIGNED_FORMAT_VERSION);
    out.extend_from_slice(&signature.to_bytes());
    out.extend_from_slice(payload);
    out
}

fn verify_signed_payload<'a>(
    package: &'a [u8],
    verifying_key: &ApakVerifyingKey,
) -> Result<&'a [u8]> {
    let header_len = MAGIC.len() + 1 + SIGNATURE_LEN;
    if package.len() <= header_len {
        return Err(ApakError::InvalidPackage("file is too small".to_string()));
    }
    if &package[..MAGIC.len()] != MAGIC {
        return Err(ApakError::InvalidPackage("bad magic".to_string()));
    }
    let version = package[MAGIC.len()];
    if version == FORMAT_VERSION {
        return Err(ApakError::InvalidPackage(
            "legacy encrypted v1 package requires --secret-file".to_string(),
        ));
    }
    if version != SIGNED_FORMAT_VERSION {
        return Err(ApakError::InvalidPackage(format!(
            "unsupported signed package version {version}"
        )));
    }

    let signature_start = MAGIC.len() + 1;
    let payload_start = signature_start + SIGNATURE_LEN;
    let mut signature_bytes = [0u8; SIGNATURE_LEN];
    signature_bytes.copy_from_slice(&package[signature_start..payload_start]);
    let signature = Signature::from_bytes(&signature_bytes);
    let payload = &package[payload_start..];
    let message = signed_message(payload);
    verifying_key
        .0
        .verify_strict(&message, &signature)
        .map_err(|_| ApakError::Signature("package signature verification failed".to_string()))?;
    Ok(payload)
}

fn signed_message(payload: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(SIGNED_MESSAGE_DOMAIN);
    hasher.update(MAGIC);
    hasher.update([SIGNED_FORMAT_VERSION]);
    hasher.update(payload);
    hasher.finalize().into()
}

fn encrypt_payload(payload: &[u8], secret: &[u8]) -> Result<Vec<u8>> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce);

    let key = derive_key(secret, &salt);
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|error| ApakError::Crypto(error.to_string()))?;
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce), payload)
        .map_err(|error| ApakError::Crypto(error.to_string()))?;

    let mut out = Vec::with_capacity(MAGIC.len() + 1 + SALT_LEN + NONCE_LEN + encrypted.len());
    out.extend_from_slice(MAGIC);
    out.push(FORMAT_VERSION);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&encrypted);
    Ok(out)
}

fn decrypt_payload(package: &[u8], secret: &[u8]) -> Result<Vec<u8>> {
    let header_len = MAGIC.len() + 1 + SALT_LEN + NONCE_LEN;
    if package.len() <= header_len {
        return Err(ApakError::InvalidPackage("file is too small".to_string()));
    }
    if &package[..MAGIC.len()] != MAGIC {
        return Err(ApakError::InvalidPackage("bad magic".to_string()));
    }
    let version = package[MAGIC.len()];
    if version != FORMAT_VERSION {
        return Err(ApakError::InvalidPackage(format!(
            "unsupported version {version}"
        )));
    }

    let salt_start = MAGIC.len() + 1;
    let nonce_start = salt_start + SALT_LEN;
    let cipher_start = nonce_start + NONCE_LEN;
    let salt = &package[salt_start..nonce_start];
    let nonce = &package[nonce_start..cipher_start];
    let ciphertext = &package[cipher_start..];

    let key = derive_key(secret, salt);
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|error| ApakError::Crypto(error.to_string()))?;
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| ApakError::Crypto("could not decrypt package".to_string()))
}

fn derive_key(secret: &[u8], salt: &[u8]) -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    pbkdf2_hmac::<Sha256>(secret, salt, KDF_ROUNDS, &mut key);
    key
}

fn decode_raw_key(value: &str, key_name: &str) -> Result<[u8; KEY_LEN]> {
    let value = value.trim();
    let decoded = if value.len() == KEY_LEN * 2 {
        let mut bytes = [0u8; KEY_LEN];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let hi = key_hex_nibble(pair[0], key_name)?;
            let lo = key_hex_nibble(pair[1], key_name)?;
            bytes[index] = (hi << 4) | lo;
        }
        return Ok(bytes);
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(value)
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(value))
            .map_err(|_| {
                ApakError::Signature(format!(
                    "{key_name} must be 32 raw bytes encoded as 64-character hex or base64"
                ))
            })?
    };

    decoded.try_into().map_err(|bytes: Vec<u8>| {
        ApakError::Signature(format!(
            "{key_name} decoded to {} bytes; expected 32 bytes",
            bytes.len()
        ))
    })
}

fn key_hex_nibble(byte: u8, key_name: &str) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(ApakError::Signature(format!(
            "{key_name} contains a non-hex character"
        ))),
    }
}

fn signing_key_value_from_file(contents: &str) -> Option<&str> {
    let mut first_value = None;
    for line in contents.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, value)) = line.split_once('=') {
            if name.trim() == "APAK_SIGNING_KEY" {
                return Some(trim_dotenv_value(value));
            }
        }
        first_value.get_or_insert(line);
    }
    first_value
}

fn signing_key_value_from_dotenv(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(path).map_err(|error| {
        ApakError::Signature(format!("could not read {}: {error}", path.display()))
    })?;
    for line in contents.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() == "APAK_SIGNING_KEY" {
            return Ok(Some(trim_dotenv_value(value).to_string()));
        }
    }
    Ok(None)
}

fn trim_dotenv_value(value: &str) -> &str {
    let value = value.trim();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if matches!(
            (bytes[0], bytes[value.len() - 1]),
            (b'"', b'"') | (b'\'', b'\'')
        ) {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn read_secret_file(path: &Path) -> Result<Vec<u8>> {
    let contents = fs::read_to_string(path).map_err(|error| {
        ApakError::Crypto(format!(
            "could not read secret file {}: {error}",
            path.display()
        ))
    })?;
    let value = contents
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .ok_or_else(|| ApakError::Crypto("secret file is empty".to_string()))?;
    let value = value
        .split_once('=')
        .map(|(_, right)| right.trim())
        .unwrap_or(value);
    decode_secret_value(value)
}

fn decode_secret_value(value: &str) -> Result<Vec<u8>> {
    if let Some(bytes) = decode_hex_32(value)? {
        return Ok(bytes);
    }
    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(value) {
        if bytes.len() == KEY_LEN {
            return Ok(bytes);
        }
    }
    Ok(value.as_bytes().to_vec())
}

fn decode_hex_32(value: &str) -> Result<Option<Vec<u8>>> {
    let cleaned: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.len() != KEY_LEN * 2 {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(KEY_LEN);
    let bytes = cleaned.as_bytes();
    for pair in bytes.chunks_exact(2) {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(Some(out))
}

fn hex_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(ApakError::Crypto(
            "secret hex value contains non-hex characters".to_string(),
        )),
    }
}

fn lzma_compress(input: &[u8]) -> Result<Vec<u8>> {
    let mut reader = Cursor::new(input);
    let mut out = Vec::new();
    lzma_rs::lzma_compress(&mut reader, &mut out)
        .map_err(|error| ApakError::Compression(error.to_string()))?;
    Ok(out)
}

fn lzma_decompress(input: &[u8]) -> Result<Vec<u8>> {
    let mut reader = Cursor::new(input);
    let mut out = Vec::new();
    lzma_rs::lzma_decompress(&mut reader, &mut out)
        .map_err(|error| ApakError::Compression(error.to_string()))?;
    Ok(out)
}

fn read_manifests_from_tar(payload: &[u8]) -> Result<(InstallToml, MetadataToml)> {
    let mut archive = tar::Archive::new(Cursor::new(payload));
    let mut install = None;
    let mut metadata = None;

    for entry in archive.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.into_owned();
        let mut text = String::new();
        match path.to_string_lossy().as_ref() {
            "install.toml" => {
                entry.read_to_string(&mut text)?;
                install = Some(toml::from_str(&text)?);
            }
            "metadata.toml" => {
                entry.read_to_string(&mut text)?;
                metadata = Some(toml::from_str(&text)?);
            }
            _ => {}
        }
    }

    let install =
        install.ok_or_else(|| ApakError::InvalidPackage("missing install.toml".to_string()))?;
    let metadata =
        metadata.ok_or_else(|| ApakError::InvalidPackage("missing metadata.toml".to_string()))?;
    Ok((install, metadata))
}

pub const TEMPLATE_INSTALL_TOML: &str = r#"[package]
id = "publisher.package-id"
name = "Package Name"
version = "0.1.0"
type = "Sample"

[install]
overwrite = true
"#;

pub const TEMPLATE_METADATA_TOML: &str = r#"[metadata]
publisher = "Publisher"
description = "Describe the package contents."
license = "Proprietary"
"#;

pub const TEMPLATE_README: &str = r#"# APAK Package

Place the files to package inside `assets`, then run
`apak pack <package-directory> <output.apak>`. APAK signs the package with the configured project key; installers verify it automatically.

Package types:
- Sample: installs into Documents/Futureboard Studio/Samples
- Preset: installs into Documents/Futureboard Studio/Presets
- Plugin: installs into the user Extensions/Plugins directory
- Theme: installs into the user Extensions/Themes directory
- Service: installs into the user Extensions/Services directory
- Extensions: legacy mixed package; assets must start with Themes, Plugins, or Services
"#;

pub const TEMPLATE_ASSETS_README: &str = TEMPLATE_README;

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SIGNING_KEY_HEX: &str =
        "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    const TEST_VERIFYING_KEY_HEX: &str =
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";

    #[test]
    fn sample_package_roundtrips_to_sample_library() {
        let temp = tempfile::tempdir().expect("tempdir");
        let template = temp.path().join("template");
        fs::create_dir_all(template.join("assets/Drums")).expect("assets");
        fs::write(
            template.join("install.toml"),
            r#"[package]
id = "futureboard.test-samples"
name = "Test Samples"
version = "0.1.0"
type = "Sample"

[install]
overwrite = true
"#,
        )
        .expect("install");
        fs::write(
            template.join("metadata.toml"),
            r#"[metadata]
publisher = "Futureboard"
description = "Roundtrip test"
license = "MIT"
"#,
        )
        .expect("metadata");
        fs::write(template.join("assets/Drums/kick.txt"), "kick").expect("asset");
        let secret_file = temp.path().join(".apak.secret");
        assert!(ensure_secret_file(&secret_file).expect("secret generated"));
        assert!(!ensure_secret_file(&secret_file).expect("secret reused"));

        let package_path = temp.path().join("test.apak");
        let report = pack_template(PackOptions {
            source_dir: template,
            output_path: package_path.clone(),
            secret_file: secret_file.clone(),
        })
        .expect("pack");
        assert_eq!(report.asset_count, 1);

        let roots = InstallRoots {
            samples: temp.path().join("Samples"),
            presets: temp.path().join("Presets"),
            extentions: temp.path().join("Extensions"),
        };
        let install = install_package(InstallOptions {
            package_path,
            secret_file,
            roots,
        })
        .expect("install");

        assert_eq!(install.installed_files.len(), 1);
        assert_eq!(
            fs::read_to_string(temp.path().join("Samples/Drums/kick.txt")).expect("installed"),
            "kick"
        );
    }

    #[test]
    fn create_package_template_writes_selected_type() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("plugin-package");

        create_package_template(CreateOptions {
            destination: destination.clone(),
            target: PackageTarget::Plugin,
            id: "futureboard.test-plugin".to_string(),
            name: "Test Plugin".to_string(),
            version: "1.2.3".to_string(),
            publisher: "Futureboard".to_string(),
            description: "Plugin package".to_string(),
            license: "MIT".to_string(),
        })
        .expect("create template");

        let install = read_install_toml(&destination.join("install.toml")).expect("install");
        let metadata = read_metadata_toml(&destination.join("metadata.toml")).expect("metadata");
        assert_eq!(install.package.target, PackageTarget::Plugin);
        assert_eq!(install.package.id, "futureboard.test-plugin");
        assert_eq!(metadata.metadata.publisher, "Futureboard");
        assert!(
            fs::read_to_string(destination.join("install.toml"))
                .expect("install TOML")
                .contains("type = \"Plugin\"")
        );
        assert!(destination.join("assets/.gitkeep").is_file());
        assert!(destination.join("README.md").is_file());
        assert!(!destination.join("assets/README.md").exists());

        let error = pack_template(PackOptions {
            source_dir: destination,
            output_path: temp.path().join("empty.apak"),
            secret_file: temp.path().join("missing.secret"),
        })
        .expect_err("placeholder must not count as an asset");
        assert!(matches!(
            error,
            ApakError::InvalidTemplate(message) if message.contains("no files")
        ));
    }

    #[test]
    fn plugin_package_roundtrips_to_plugin_extensions_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("plugin-package");
        create_package_template(CreateOptions {
            destination: destination.clone(),
            target: PackageTarget::Plugin,
            id: "futureboard.test-plugin".to_string(),
            name: "Test Plugin".to_string(),
            version: "1.0.0".to_string(),
            publisher: "Futureboard".to_string(),
            description: "Plugin roundtrip".to_string(),
            license: "MIT".to_string(),
        })
        .expect("create template");
        fs::create_dir_all(destination.join("assets/Synth")).expect("plugin assets");
        fs::write(destination.join("assets/Synth/plugin.json"), "plugin").expect("plugin asset");

        let secret_file = temp.path().join(".apak.secret");
        ensure_secret_file(&secret_file).expect("secret");
        let package_path = temp.path().join("plugin.apak");
        let pack = pack_template(PackOptions {
            source_dir: destination,
            output_path: package_path.clone(),
            secret_file: secret_file.clone(),
        })
        .expect("pack");
        assert_eq!(pack.asset_count, 1);

        let install = install_package(InstallOptions {
            package_path,
            secret_file,
            roots: InstallRoots {
                samples: temp.path().join("Samples"),
                presets: temp.path().join("Presets"),
                extentions: temp.path().join("Extensions"),
            },
        })
        .expect("install");
        assert_eq!(install.summary.target, PackageTarget::Plugin);
        assert_eq!(install.installed_files.len(), 1);
        assert_eq!(
            fs::read_to_string(temp.path().join("Extensions/Plugins/Synth/plugin.json"))
                .expect("installed plugin"),
            "plugin"
        );
    }

    #[test]
    fn signed_plugin_package_roundtrips_to_plugin_extensions_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("signed-plugin-package");
        create_package_template(CreateOptions {
            destination: destination.clone(),
            target: PackageTarget::Plugin,
            id: "futureboard.signed-plugin".to_string(),
            name: "Signed Plugin".to_string(),
            version: "2.0.0".to_string(),
            publisher: "Futureboard".to_string(),
            description: "Signed plugin roundtrip".to_string(),
            license: "MIT".to_string(),
        })
        .expect("create template");
        fs::create_dir_all(destination.join("assets/Synth")).expect("plugin assets");
        fs::write(
            destination.join("assets/Synth/plugin.json"),
            "signed plugin",
        )
        .expect("plugin asset");

        let verifying_key_value = verifying_key_value_from_signing_key_value(TEST_SIGNING_KEY_HEX)
            .expect("derive verifying key");
        let verifying_key = parse_verifying_key(&verifying_key_value).expect("parse verifying key");
        let package_path = temp.path().join("signed-plugin.apak");
        let pack = pack_signed_template(SignedPackOptions {
            source_dir: destination,
            output_path: package_path.clone(),
            signing_key: parse_signing_key(TEST_SIGNING_KEY_HEX).expect("signing key"),
        })
        .expect("pack signed plugin");
        assert_eq!(pack.asset_count, 1);

        let package_bytes = fs::read(&package_path).expect("signed package");
        assert_eq!(&package_bytes[..MAGIC.len()], MAGIC);
        assert_eq!(package_bytes[MAGIC.len()], SIGNED_FORMAT_VERSION);
        let info = read_signed_package_info(&package_path, &verifying_key)
            .expect("read signed package info");
        assert_eq!(info.id, "futureboard.signed-plugin");
        assert_eq!(info.target, PackageTarget::Plugin);

        let install = install_signed_package(SignedInstallOptions {
            package_path,
            verifying_key,
            roots: InstallRoots {
                samples: temp.path().join("Samples"),
                presets: temp.path().join("Presets"),
                extentions: temp.path().join("Extensions"),
            },
        })
        .expect("install signed plugin");
        assert_eq!(install.installed_files.len(), 1);
        assert_eq!(
            fs::read_to_string(temp.path().join("Extensions/Plugins/Synth/plugin.json"))
                .expect("installed signed plugin"),
            "signed plugin"
        );
        assert!(!temp.path().join("Extensions/Plugins/.gitkeep").exists());
    }

    #[test]
    fn signed_package_rejects_tampered_compressed_payload() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("tampered-package");
        write_template(&destination).expect("write template");
        fs::write(destination.join("assets/payload.txt"), "payload").expect("asset");

        let package_path = temp.path().join("tampered.apak");
        pack_signed_template(SignedPackOptions {
            source_dir: destination,
            output_path: package_path.clone(),
            signing_key: parse_signing_key(TEST_SIGNING_KEY_HEX).expect("signing key"),
        })
        .expect("pack signed package");

        let mut bytes = fs::read(&package_path).expect("signed package");
        let payload_start = MAGIC.len() + 1 + SIGNATURE_LEN;
        let tamper_index = payload_start + (bytes.len() - payload_start) / 2;
        bytes[tamper_index] ^= 0x01;
        fs::write(&package_path, bytes).expect("tampered package");

        let verifying_key = parse_verifying_key(TEST_VERIFYING_KEY_HEX).expect("verifying key");
        let error = read_signed_package_info(&package_path, &verifying_key)
            .expect_err("tampering must be rejected before decompression");
        assert!(matches!(
            error,
            ApakError::Signature(message) if message.contains("verification failed")
        ));
    }

    #[test]
    fn signing_and_verifying_keys_parse_and_derive() {
        let signing_hex = parse_signing_key(TEST_SIGNING_KEY_HEX).expect("hex signing key");
        assert_eq!(
            signing_hex.0.to_bytes(),
            decode_raw_key(TEST_SIGNING_KEY_HEX, "key").unwrap()
        );

        let signing_key_base64 =
            base64::engine::general_purpose::STANDARD.encode(signing_hex.0.to_bytes());
        let signing_base64 = parse_signing_key(&signing_key_base64).expect("base64 signing key");
        assert_eq!(signing_base64.0.to_bytes(), signing_hex.0.to_bytes());

        let verifying_value = verifying_key_value_from_signing_key_value(&signing_key_base64)
            .expect("derive verifying key");
        let verifying_base64 = parse_verifying_key(&verifying_value).expect("base64 verifying key");
        let verifying_hex = parse_verifying_key(TEST_VERIFYING_KEY_HEX).expect("hex verifying key");
        assert_eq!(verifying_base64.0.to_bytes(), verifying_hex.0.to_bytes());

        let key_file = tempfile::NamedTempFile::new().expect("key file");
        fs::write(
            key_file.path(),
            format!("APAK_SIGNING_KEY={signing_key_base64}\n"),
        )
        .expect("write key file");
        let loaded = load_signing_key(Some(key_file.path())).expect("load explicit key file");
        assert_eq!(loaded.0.to_bytes(), signing_hex.0.to_bytes());

        assert_eq!(format!("{signing_hex:?}"), "ApakSigningKey([REDACTED])");
        assert_eq!(format!("{verifying_hex:?}"), "ApakVerifyingKey([REDACTED])");
        assert!(matches!(
            parse_signing_key("not-a-key"),
            Err(ApakError::Signature(_))
        ));
    }

    #[test]
    fn create_package_template_rejects_non_empty_destination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("existing");
        fs::create_dir_all(&destination).expect("destination");
        fs::write(destination.join("keep.txt"), "keep").expect("existing file");

        let error = create_package_template(CreateOptions {
            destination,
            target: PackageTarget::Sample,
            id: "futureboard.samples".to_string(),
            name: "Samples".to_string(),
            version: "0.1.0".to_string(),
            publisher: "Futureboard".to_string(),
            description: String::new(),
            license: "Proprietary".to_string(),
        })
        .expect_err("non-empty destination should fail");

        assert!(matches!(error, ApakError::InvalidTemplate(_)));
    }

    #[test]
    fn package_targets_accept_plural_and_legacy_names() {
        let cases = [
            ("Samples", PackageTarget::Sample),
            ("Presets", PackageTarget::Preset),
            ("Plugins", PackageTarget::Plugin),
            ("Themes", PackageTarget::Theme),
            ("Services", PackageTarget::Service),
            ("Extension", PackageTarget::Extentions),
            ("Extention", PackageTarget::Extentions),
            ("Extentions", PackageTarget::Extentions),
        ];

        for (name, expected) in cases {
            let manifest = format!(
                "[package]\nid = \"x\"\nname = \"x\"\nversion = \"0.1.0\"\ntype = \"{name}\"\n"
            );
            let install: InstallToml = toml::from_str(&manifest).expect("target alias");
            assert_eq!(install.package.target, expected);
        }

        let canonical = toml::to_string(&InstallToml {
            package: PackageSection {
                id: "x".to_string(),
                name: "x".to_string(),
                version: "0.1.0".to_string(),
                target: PackageTarget::Extentions,
            },
            install: InstallSection::default(),
        })
        .expect("serialize extensions target");
        assert!(canonical.contains("type = \"Extensions\""));
    }

    #[test]
    fn package_types_resolve_to_their_runtime_directories() {
        let roots = InstallRoots {
            samples: PathBuf::from("Samples"),
            presets: PathBuf::from("Presets"),
            extentions: PathBuf::from("Extensions"),
        };
        let cases = [
            (
                PackageTarget::Sample,
                "Drums/kick.wav",
                "Samples/Drums/kick.wav",
            ),
            (
                PackageTarget::Preset,
                "Bass/init.toml",
                "Presets/Bass/init.toml",
            ),
            (
                PackageTarget::Plugin,
                "Synth/plugin.json",
                "Extensions/Plugins/Synth/plugin.json",
            ),
            (
                PackageTarget::Theme,
                "Dark/theme.json",
                "Extensions/Themes/Dark/theme.json",
            ),
            (
                PackageTarget::Service,
                "Cloud/service.json",
                "Extensions/Services/Cloud/service.json",
            ),
            (
                PackageTarget::Extentions,
                "Plugins/Synth/plugin.json",
                "Extensions/Plugins/Synth/plugin.json",
            ),
        ];

        for (target, asset, expected) in cases {
            let install = InstallToml {
                package: PackageSection {
                    id: "x".to_string(),
                    name: "x".to_string(),
                    version: "0.1.0".to_string(),
                    target,
                },
                install: InstallSection::default(),
            };
            assert_eq!(
                resolve_install_target(&install, &roots, Path::new(asset)).expect("target"),
                PathBuf::from(expected)
            );
        }
    }

    #[test]
    fn extensions_reject_unknown_top_level_asset() {
        let install = InstallToml {
            package: PackageSection {
                id: "x".to_string(),
                name: "x".to_string(),
                version: "0.1.0".to_string(),
                target: PackageTarget::Extentions,
            },
            install: InstallSection::default(),
        };
        let roots = InstallRoots {
            samples: PathBuf::from("Samples"),
            presets: PathBuf::from("Presets"),
            extentions: PathBuf::from("Extensions"),
        };
        let error = resolve_install_target(&install, &roots, Path::new("Other/file.txt"))
            .expect_err("unknown root should fail");
        assert!(matches!(error, ApakError::UnsupportedTarget(_)));
    }
}
