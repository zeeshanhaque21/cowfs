mod common;

use std::collections::HashSet;

use common::{compressible, random};
use cowfs_store::{chunks, AVG_CHUNK_LEN, MAX_CHUNK_LEN, MIN_CHUNK_LEN};
use proptest::prelude::*;

fn lens(data: &[u8]) -> Vec<usize> {
    chunks(data).map(<[u8]>::len).collect()
}

fn check_invariants(data: &[u8]) {
    let ls = lens(data);
    assert_eq!(ls.iter().sum::<usize>(), data.len());
    let joined: Vec<u8> = chunks(data).flatten().copied().collect();
    assert_eq!(joined, data);
    for (i, &l) in ls.iter().enumerate() {
        assert!(l <= MAX_CHUNK_LEN, "chunk {i} is {l}");
        assert!(l > 0);
        if i + 1 < ls.len() {
            assert!(l >= MIN_CHUNK_LEN, "chunk {i} is {l}");
        }
    }
    if !data.is_empty() && data.len() <= MIN_CHUNK_LEN {
        assert_eq!(ls.len(), 1);
    }
    assert_eq!(ls, lens(data), "not deterministic");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn invariants_random(seed in any::<u64>(), len in 0usize..900_000) {
        check_invariants(&random(seed, len));
    }

    #[test]
    fn invariants_compressible(seed in any::<u64>(), len in 0usize..900_000) {
        check_invariants(&compressible(seed, len));
    }

    #[test]
    fn invariants_edge_sizes(seed in any::<u64>(), delta in 0usize..4, which in 0usize..3) {
        let base = [MIN_CHUNK_LEN, AVG_CHUNK_LEN, MAX_CHUNK_LEN][which];
        check_invariants(&random(seed, base - 1 + delta));
    }
}

#[test]
fn small_inputs_are_one_chunk() {
    for len in [1, 2, 100, MIN_CHUNK_LEN - 1, MIN_CHUNK_LEN] {
        assert_eq!(lens(&random(3, len)), vec![len]);
        assert_eq!(lens(&vec![0u8; len]), vec![len]);
    }
    assert!(lens(&[]).is_empty());
}

#[test]
fn all_zero_input_uses_max_chunks() {
    let ls = lens(&vec![0u8; 3 * MAX_CHUNK_LEN + 5]);
    assert_eq!(ls, vec![MAX_CHUNK_LEN, MAX_CHUNK_LEN, MAX_CHUNK_LEN, 5]);
}

#[test]
fn average_chunk_size_is_near_target() {
    let data = random(11, 16 << 20);
    let n = chunks(&data).count();
    let avg = data.len() / n;
    assert!(
        (AVG_CHUNK_LEN / 2..AVG_CHUNK_LEN * 2).contains(&avg),
        "average {avg}"
    );
}

#[test]
fn boundaries_are_content_defined() {
    let data = random(5, 8 << 20);
    let mut shifted = vec![0xAB];
    shifted.extend_from_slice(&data);
    let a: HashSet<&[u8]> = chunks(&data).collect();
    let b: Vec<&[u8]> = chunks(&shifted).collect();
    let shared = b.iter().filter(|c| a.contains(**c)).count();
    assert!(
        shared * 10 >= b.len() * 9,
        "only {shared} of {} chunks survive a 1 byte insert",
        b.len()
    );
}

// Pins the chunker (gear table, masks, normalization) so a dependency change cannot move boundaries silently.
#[test]
fn boundaries_are_pinned() {
    let ls = lens(&random(42, 1 << 20));
    assert_eq!(ls, GOLDEN);
}

const GOLDEN: &[usize] = &[
    106594, 75208, 19070, 105223, 66060, 92895, 156030, 42707, 136630, 120388, 69647, 50111, 8013,
];
