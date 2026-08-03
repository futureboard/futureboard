//! Platform / edition vocabulary for packaging.
//!
//! Keeps every piece of platform-specific string mapping in one module so the
//! rest of the packager stays OS-agnostic. Adding a new target triple only
//! requires a row in [`platform_folder`]; nothing here shells out or touches
//! the filesystem.

use std::fmt;

use anyhow::{Result, bail};

/// Which edition of Futureboard Studio to build and stage.
///
/// The build arguments come straight from the existing `.cargo/config.toml`
/// aliases — this enum only decides which feature set and target directory to
/// use, it does not invent a new feature model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edition {
    /// Default public build (the `build-ce` / `check-ce` aliases).
    Community,
    /// Windows Professional Edition with ASIO (the `*-professional-win` aliases).
    Professional,
}

impl Edition {
    /// Lower-case identifier used in output paths and `build-info.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            Edition::Community => "community",
            Edition::Professional => "professional",
        }
    }

    /// Cargo `--features` argument for this edition, or `None` for the default
    /// Community configuration. Mirrors `build-professional-win` in
    /// `.cargo/config.toml`.
    pub fn cargo_features(self) -> Option<&'static str> {
        match self {
            Edition::Community => None,
            Edition::Professional => {
                Some("futureboard_native/professional,sphere_directaudioengine/asio")
            }
        }
    }

    /// Per-edition Cargo target directory. Cargo unifies features within one
    /// build graph, so the editions must not share a directory — this matches
    /// the `--target-dir target/<edition>` split the aliases already use.
    pub fn target_dir(self) -> &'static str {
        match self {
            Edition::Community => "target/community",
            Edition::Professional => "target/professional",
        }
    }
}

impl fmt::Display for Edition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Edition {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "community" | "ce" => Ok(Edition::Community),
            // `exclusive` / `ee` are the edition's former name, still accepted so
            // existing scripts and muscle memory keep working after the rename.
            "professional" | "pro" | "exclusive" | "ee" => Ok(Edition::Professional),
            other => bail!("unknown edition `{other}` (expected `community` or `professional`)"),
        }
    }
}

/// Map a Cargo target triple to a short, readable output folder name.
///
/// Unknown triples never panic — they are normalized into a filesystem-safe
/// slug so a novel target still produces a sane directory under `out/`.
pub fn platform_folder(triple: &str) -> String {
    match triple {
        "x86_64-pc-windows-msvc" | "x86_64-pc-windows-gnu" => "windows-x64".to_string(),
        "aarch64-pc-windows-msvc" => "windows-arm64".to_string(),
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => "linux-x64".to_string(),
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl" => "linux-arm64".to_string(),
        "x86_64-apple-darwin" => "macos-x64".to_string(),
        "aarch64-apple-darwin" => "macos-arm64".to_string(),
        other => normalize_folder(other),
    }
}

/// The dynamic-library file extension (without dot) for a target triple's OS:
/// `dll` on Windows, `dylib` on Apple, `so` elsewhere.
pub fn dynamic_library_extension(triple: &str) -> &'static str {
    if triple.contains("windows") {
        "dll"
    } else if triple.contains("apple") || triple.contains("darwin") {
        "dylib"
    } else {
        "so"
    }
}

/// The dynamic-library file-name prefix for a target triple's OS: empty on
/// Windows, `lib` on Unix-likes (Linux/macOS).
pub fn dynamic_library_prefix(triple: &str) -> &'static str {
    if triple.contains("windows") {
        ""
    } else {
        "lib"
    }
}

/// The platform-correct dynamic-library file name for a crate `base_name`
/// (Cargo's `[lib] name`), e.g. `rodharerist` → `rodharerist.dll` /
/// `librodharerist.so` / `librodharerist.dylib`.
pub fn dynamic_library_file_name(base_name: &str, triple: &str) -> String {
    format!(
        "{}{}.{}",
        dynamic_library_prefix(triple),
        base_name,
        dynamic_library_extension(triple)
    )
}

/// Turn an arbitrary triple into a lower-case `[a-z0-9-]` slug so it is always
/// a valid directory name on every supported filesystem.
fn normalize_folder(triple: &str) -> String {
    let mut slug = String::with_capacity(triple.len());
    let mut prev_dash = false;
    for character in triple.chars() {
        let mapped = if character.is_ascii_alphanumeric() {
            character.to_ascii_lowercase()
        } else {
            '-'
        };
        if mapped == '-' {
            if prev_dash {
                continue;
            }
            prev_dash = true;
        } else {
            prev_dash = false;
        }
        slug.push(mapped);
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "unknown-target".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_triples_map_to_readable_names() {
        assert_eq!(platform_folder("x86_64-pc-windows-msvc"), "windows-x64");
        assert_eq!(platform_folder("aarch64-pc-windows-msvc"), "windows-arm64");
        assert_eq!(platform_folder("x86_64-unknown-linux-gnu"), "linux-x64");
        assert_eq!(platform_folder("aarch64-unknown-linux-gnu"), "linux-arm64");
        assert_eq!(platform_folder("x86_64-apple-darwin"), "macos-x64");
        assert_eq!(platform_folder("aarch64-apple-darwin"), "macos-arm64");
    }

    #[test]
    fn unknown_triple_is_normalized_not_panicked() {
        assert_eq!(
            platform_folder("riscv64gc-unknown-linux-gnu"),
            "riscv64gc-unknown-linux-gnu"
        );
        // Collapses runs of non-alphanumerics and trims edges.
        assert_eq!(platform_folder("Weird__Target!!"), "weird-target");
        assert_eq!(platform_folder("///"), "unknown-target");
    }

    #[test]
    fn dynamic_library_naming_matches_platform() {
        assert_eq!(dynamic_library_extension("x86_64-pc-windows-msvc"), "dll");
        assert_eq!(dynamic_library_extension("x86_64-unknown-linux-gnu"), "so");
        assert_eq!(dynamic_library_extension("aarch64-apple-darwin"), "dylib");

        assert_eq!(dynamic_library_prefix("x86_64-pc-windows-msvc"), "");
        assert_eq!(dynamic_library_prefix("x86_64-unknown-linux-gnu"), "lib");
        assert_eq!(dynamic_library_prefix("aarch64-apple-darwin"), "lib");

        assert_eq!(
            dynamic_library_file_name("rodharerist", "x86_64-pc-windows-msvc"),
            "rodharerist.dll"
        );
        assert_eq!(
            dynamic_library_file_name("rodharerist", "x86_64-unknown-linux-gnu"),
            "librodharerist.so"
        );
        assert_eq!(
            dynamic_library_file_name("rodharerist", "aarch64-apple-darwin"),
            "librodharerist.dylib"
        );
    }

    #[test]
    fn edition_parsing_is_case_insensitive() {
        assert_eq!("Community".parse::<Edition>().unwrap(), Edition::Community);
        assert_eq!(
            "PROFESSIONAL".parse::<Edition>().unwrap(),
            Edition::Professional
        );
        assert_eq!("ce".parse::<Edition>().unwrap(), Edition::Community);
        assert!("enterprise".parse::<Edition>().is_err());
    }

    #[test]
    fn professional_carries_feature_flags_community_does_not() {
        assert_eq!(Edition::Community.cargo_features(), None);
        assert_eq!(
            Edition::Professional.cargo_features(),
            Some("futureboard_native/professional,sphere_directaudioengine/asio")
        );
    }
}
