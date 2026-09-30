//! Every `Fault` against the whole suite, and against the oracle where the suite is silent.
//! Slow: `cargo test -p cowfs-vfs-test --release --features mutation-tests --test mutations_full -- --nocapture`.

use std::sync::Arc;

use cowfs_vfs::Vfs;
use cowfs_vfs_test::conformance::{run_all, Options};
use cowfs_vfs_test::model::{random_ops, run};
use cowfs_vfs_test::{Fault, MemVfs};

/// Faults no check and no oracle sequence can see, each with the reason.
const UNCATCHABLE: &[(Fault, &str)] = &[];

const ORACLE_SEQUENCES: u64 = 2000;

#[test]
fn every_fault_is_caught() {
    let results: Vec<(Fault, Vec<String>, Option<u64>)> = std::thread::scope(|s| {
        let hs: Vec<_> = Fault::ALL
            .iter()
            .map(|&fault| {
                s.spawn(move || {
                    let factory = move || -> Arc<dyn Vfs> { Arc::new(MemVfs::with_fault(fault)) };
                    let report = run_all(&factory, &Options::default());
                    let failed: Vec<String> = report
                        .failures()
                        .iter()
                        .map(|r| r.check.name.to_string())
                        .collect();
                    let oracle = if failed.is_empty() {
                        (0..ORACLE_SEQUENCES)
                            .find(|&i| run(&MemVfs::with_fault(fault), &random_ops(i, 60)).is_err())
                    } else {
                        None
                    };
                    (fault, failed, oracle)
                })
            })
            .collect();
        hs.into_iter()
            .map(|h| h.join().expect("mutation run panicked"))
            .collect()
    });
    let mut missed = Vec::new();
    for (fault, failed, oracle) in &results {
        println!(
            "{fault:?}: {} checks fail {:?}, oracle sequence {oracle:?}",
            failed.len(),
            &failed[..failed.len().min(3)]
        );
        if failed.is_empty() && oracle.is_none() && !UNCATCHABLE.iter().any(|(f, _)| f == fault) {
            missed.push(*fault);
        }
    }
    println!(
        "{} faults, {} missed: {missed:?}",
        results.len(),
        missed.len()
    );
    assert!(missed.is_empty(), "faults nothing caught: {missed:?}");
}
