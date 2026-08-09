//! Manual probe: prove a bundle that never returns from module load is killed
//! at the deadline and the sweep continues.
//!
//! Not a unit test — it needs real plug-ins installed. Run with a short budget:
//! `FUTUREBOARD_PLUGIN_SCAN_TIMEOUT_SECS=5 cargo run -p sphere-plugin-host --example scan_timeout_probe -- <bundle>...`

use std::path::Path;
use std::time::Instant;

fn main() {
    let bundles: Vec<String> = std::env::args().skip(1).collect();
    if bundles.is_empty() {
        eprintln!("usage: scan_timeout_probe <bundle.vst3>...");
        return;
    }
    let started = Instant::now();
    for bundle in &bundles {
        let path = Path::new(bundle);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| bundle.clone());
        let t = Instant::now();
        match SpherePluginHost::scan::isolation::run_isolated_bundle_scan(path) {
            Ok(outcome) => println!(
                "{:>6}ms  OK   plugins={} failures={}  {name}",
                t.elapsed().as_millis(),
                outcome.plugins.len(),
                outcome.failures.len(),
            ),
            Err(error) => println!("{:>6}ms  ERR  {name}: {error}", t.elapsed().as_millis()),
        }
    }
    println!(
        "sweep completed in {}ms without hanging",
        started.elapsed().as_millis()
    );
}
