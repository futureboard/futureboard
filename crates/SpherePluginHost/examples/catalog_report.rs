//! Manual probe: report what the cached plug-in catalog currently holds.
//!
//! Read-only. Used to compare the catalog before and after a rescan without
//! launching the app.
//!
//! `cargo run -p sphere-plugin-host --example catalog_report`

use std::collections::BTreeMap;

use SpherePluginHost::{CatalogLoad, PluginFormat, PluginKind, PluginRegistry};

fn main() {
    let catalog = match PluginRegistry::load_catalog() {
        CatalogLoad::Loaded { catalog, .. } => catalog,
        CatalogLoad::MissingDatabase { path } => {
            println!("no database at {}", path.display());
            return;
        }
        CatalogLoad::Error { path, message } => {
            println!("error reading {}: {message}", path.display());
            return;
        }
    };

    let rows: Vec<_> = catalog
        .plugins
        .iter()
        .map(|entry| entry.to_registry_plugin())
        .collect();

    let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_format: BTreeMap<&str, usize> = BTreeMap::new();
    let mut ids = std::collections::HashSet::new();
    let mut duplicate_ids = 0usize;
    let mut duplicate_display = 0usize;
    let mut display_keys = std::collections::HashSet::new();
    for row in &rows {
        *by_kind.entry(row.kind.as_str()).or_default() += 1;
        *by_format.entry(row.format.label()).or_default() += 1;
        if !ids.insert(row.id.clone()) {
            duplicate_ids += 1;
        }
        let key = format!(
            "{}|{}|{}",
            row.vendor.to_lowercase(),
            row.name.to_lowercase(),
            row.format.label()
        );
        if !display_keys.insert(key) {
            duplicate_display += 1;
        }
    }

    println!(
        "catalog: {} rows from {}",
        rows.len(),
        catalog.source_path.display()
    );
    println!("  by kind:   {by_kind:?}");
    println!("  by format: {by_format:?}");
    println!("  duplicate ids: {duplicate_ids}, duplicate vendor+name+format: {duplicate_display}");

    let instruments: Vec<&str> = rows
        .iter()
        .filter(|r| r.kind == PluginKind::Instrument && r.format == PluginFormat::Vst3)
        .map(|r| r.name.as_str())
        .collect();
    println!("  VST3 instruments: {}", instruments.len());
    for name in instruments.iter().take(25) {
        println!("    {name}");
    }

    // Anything the old name heuristic used to promote to Instrument.
    let bassy: Vec<String> = rows
        .iter()
        .filter(|r| {
            let n = r.name.to_lowercase();
            n.contains("bass") || n.contains("drum") || n.contains("generator")
        })
        .map(|r| format!("{} [{}] {:?}", r.name, r.kind.as_str(), r.sub_categories))
        .collect();
    println!("  name-heuristic tripwires: {}", bassy.len());
    for row in bassy.iter().take(20) {
        println!("    {row}");
    }
}
