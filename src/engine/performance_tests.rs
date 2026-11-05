//! Reproducible loopback workload for performance reports, run separately from correctness tests.
use super::{Info, Job, Limiter};
use crate::transport::client::agent;
use std::io::Read;
use std::sync::Arc;
use std::time::Instant;

#[test]
#[ignore = "performance measurement; run with tools/benchmark.py"]
fn performance_download() {
    const BYTES: usize = 32 * 1024 * 1024;
    let root = std::env::temp_dir().join(format!("ndm-performance-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let source = Arc::new(vec![0x5a; BYTES]);
    let url = crate::test_support::serve(source);
    let agent = agent("", "");
    let mut trials = Vec::new();
    let mut segments = Vec::new();
    for trial in 0..3 {
        let job = Job::new(Info {
            id: trial + 1,
            url: format!("{url}r/fixture.bin"),
            name: format!("trial-{trial}.bin"),
            folder: root.display().to_string(),
            conns: 8,
            ..Default::default()
        });
        let started = Instant::now();
        assert_eq!(job.run_part(&agent, &Limiter::default()), Ok(true));
        trials.push(started.elapsed().as_secs_f64());
        segments.push(job.info().segs.len());
        let mut file = std::fs::File::open(job.info().path()).unwrap();
        let mut buffer = [0; 64 * 1024];
        let mut read = 0;
        loop {
            let count = file.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            assert!(buffer[..count].iter().all(|byte| *byte == 0x5a));
            read += count;
        }
        assert_eq!(read, BYTES);
    }
    let mut sorted = trials.clone();
    sorted.sort_by(f64::total_cmp);
    println!(
        "NDM_BENCH {}",
        serde_json::json!({
            "case": "range_download", "bytes": BYTES, "trials_seconds": trials,
            "median_seconds": sorted[1], "bytes_per_second": BYTES as f64 / sorted[1],
            "connection_limit": 8, "segments": segments,
        })
    );
    std::fs::remove_dir_all(root).unwrap();
}
