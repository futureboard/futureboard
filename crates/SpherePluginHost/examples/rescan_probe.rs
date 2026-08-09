//! Manual probe: run the app's real registry scan and summarise the result.
//!
//! Exercises the same `PluginRegistry::scan_with_progress` path the Plugin
//! Manager uses, so a run here is evidence about the shipped scan, not a
//! parallel implementation.
//!
//! `cargo run -p sphere-plugin-host --example rescan_probe`

use std::collections::{BTreeMap, HashSet};

use SpherePluginHost::registry::{PluginRegistry, ScanOptions, ScanProgress};
use SpherePluginHost::PluginFormat;

fn main() {
    let started = std::time::Instant::now();
    let mut bundles_seen = 0usize;
    let result = PluginRegistry::scan_with_progress(
        ScanOptions {
            paths: None,
            delete_presets_first: false,
            include_au: false,
            formats_only: Some(vec![PluginFormat::Vst3, PluginFormat::Clap]),
        },
        |progress| {
            if let ScanProgress::ScanningBundle { current, total, .. } = progress {
                bundles_seen = current;
                if current % 25 == 0 || current == total {
                    eprintln!("  bundle {current}/{total}");
                }
            }
        },
    );

    let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_format: BTreeMap<&str, usize> = BTreeMap::new();
    let mut display_keys = HashSet::new();
    let mut duplicate_display = 0usize;
    for plugin in &result.plugins {
        *by_kind.entry(plugin.kind.as_str()).or_default() += 1;
        *by_format.entry(plugin.format.label()).or_default() += 1;
        let key = format!(
            "{}|{}|{}",
            plugin.vendor.to_lowercase(),
            plugin.name.to_lowercase(),
            plugin.format.label()
        );
        if !display_keys.insert(key) {
            duplicate_display += 1;
        }
    }

    println!("scan finished in {:?}", started.elapsed());
    println!("  bundles scanned: {bundles_seen}");
    println!("  plugins: {}", result.plugins.len());
    println!("  by kind:   {by_kind:?}");
    println!("  by format: {by_format:?}");
    println!("  duplicate vendor+name+format: {duplicate_display}");
    println!("  failures: {}", result.failed.len());
    for failure in result.failed.iter().take(12) {
        println!(
            "    {} — {}",
            failure.path.file_name().map_or_else(
                || failure.path.display().to_string(),
                |n| n.to_string_lossy().into_owned()
            ),
            failure.error.chars().take(90).collect::<String>()
        );
    }
}
