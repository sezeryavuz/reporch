//! Timing harness for the real scan (§5.3, spike #9). Not part of the product.
fn main() {
    let home = std::env::var("HOME").expect("HOME");
    let cfg = reporch_store::ScanConfig::default();
    let opts = reporch_scan::ScanOptions {
        roots: vec![std::path::PathBuf::from(&home)],
        max_depth: cfg.max_depth,
        skip: cfg.skip.iter().cloned().collect(),
        threads: 0,
    };
    for label in ["cold-ish", "warm"] {
        let t = std::time::Instant::now();
        let found = reporch_scan::scan(&opts);
        let el = t.elapsed();
        println!(
            "{label:9} {:>4} repos in {:>7.3}s",
            found.len(),
            el.as_secs_f64()
        );
        if label == "warm" {
            let with_org = found.iter().filter(|r| r.remote_org.is_some()).count();
            println!("          {with_org} have a remote org");
            let ranked = reporch_scan::rank(&found, "modulex", &Default::default());
            println!("          top for project 'modulex':");
            for r in ranked.iter().take(6) {
                println!(
                    "            {:>4}  {:<28} {:?}",
                    r.score, r.repo.name, r.reasons
                );
            }
        }
    }
}
