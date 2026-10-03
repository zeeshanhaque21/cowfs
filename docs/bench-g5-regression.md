# g5 byte-count regression (issue #55)

## Defect

`counts()` in `bench/gates.py` scaled `big_bytes` and `large_bytes` and truncated to an integer.
`g5` writes whole 1 MiB chunks and `ensure_tree` writes whole 64 KiB chunks.
At any scale whose byte count is not a multiple of the chunk, the file written was shorter than `counts()` said, so `read_matches` (`got == expected`) could never be true.
At scale 100 the count is a multiple of the chunk, so it passed and the defect went unseen.
It hit the native arm too, so it was a harness bug and not a cowfs bug.

## Baseline, measured before the fix

Entrypoint `bench/gates.py --gates g5 --reps 1 --no-resume`, `COWFS_BENCH_SCALE=2`, one rep, on the cowfs-mounted worktree slot, load1 9.15.

| field | value |
|---|---|
| `counts()["big_bytes"]` (expected) | 21474836 |
| bytes the gate wrote | 20971520 (20 chunks of 1 MiB) |
| shortfall | 503316 |
| `read_matches` | false |

Raw record: `bench/out/g5-regression/g5reg-baseline-20261002-211539.jsonl` (ignored, local).
The baseline row has no `written_bytes` field because the pre-fix gate did not measure it.

## Fix

- `UNIT` in `bench/gates.py` is the one table of chunk sizes. `counts()` rounds a scaled byte count down to a whole number of units. Both byte keys keep the original floor of 1 MiB (`MIN_BYTES`), which is a multiple of both units. The writers use the same `UNIT` values, so they cannot drift apart again.
- Full scale is unchanged: `big_bytes` 1 GiB, `large_bytes` 8 MiB.
- `g5` stats the file after `fsync` and refuses when the written size is not `counts()["big_bytes"]`, and refuses when the read-back count is not the written size. A refusal exits non-zero and records no rep, so a short read never produces a throughput row. Real failures are harness bugs or data loss, and neither may yield a number.
- `g5` records `bytes` (expected), `written_bytes` and `read_bytes`. A recorded rep always has all three equal; `read_matches` is kept as `true` for older readers and nothing trusts it.
- `g5` removes its own `big/seq.bin` in a `finally`, on success and on refusal, and refuses if the file is still there. It removes nothing else.
- `ensure_tree` verifies the fixture after generating it and again on every marker hit: every `dNNN` directory holds the expected number of files of exactly 256 bytes, and every `big/bNNN` is exactly `large_bytes`. A mismatch refuses and names the tree to remove. It does not repair, because a short fixture file on the cowfs arm may be data loss. The `.generated` marker includes `large_bytes`. The check is untimed setup, a stat pass over the tree, identical on both arms.
- `compare.py` validates every input file (`--native`, `--cowfs` and `--noise-floor`) before comparing anything. For each g5 rep, expected, written and read must be integers and numerically equal, and `bytes` must equal the meta `counts.big_bytes`. Any failure exits 3. The `read_matches` flag is not trusted. Pre-fix rows with no written/read fields pass only when `read_matches` is true and the size is a whole MiB (the old write unit), so valid pre-fix scale-100 files stay usable.

## Semantics that changed, and results that are no longer comparable

- At non-multiple scales `big_bytes` and `large_bytes` now round down to the unit instead of truncating. At scale 2, `big_bytes` moved from 21474836 to 20971520. `large_bytes` is 1048576 at scale 2, as before, because the 1 MiB floor is kept.
- `large_bytes` differs from the old value only where the old value was above 1 MiB and not a multiple of 64 KiB, for example scale 13: 1090519 became 1048576. Before this fix the tree files there were 1048576 bytes while `counts()` said 1090519.
- Scale 100 is unchanged for both keys. Results at scale 100 are comparable with earlier ones.
- Any smoke run at a non-multiple scale before this change is not a valid comparison with one after it. Those runs had a g5 that wrote fewer bytes than recorded, a `read_matches` of false, and a throughput computed on the expected count rather than the written one. Do not mix them. The meta `counts` differ, so a resumed run cannot mix them, and `compare.py` exits 3 on the old files.
- The first run after this change regenerates every existing tree, because the marker format changed.
- Not changed here and worth its own issue: `small_files` is also rounded by the tree generator (`256 * max(1, total // 256)`), so scale 100 generates 99840 files, not 100000. g4 reports `entries`.

## After the fix, measured

Same entrypoint, one rep each. Byte counts only.

| scale | expected | written | read | recorded | root |
|---|---|---|---|---|---|
| 2 | 20971520 | 20971520 | 20971520 | yes | `bench/out/g5-regression/root-r2` (cowfs slot) |
| 100 | 1073741824 | 1073741824 | 1073741824 | yes | throwaway dir on local APFS, deleted after (run on the first revision of this PR; the g5 byte path is unchanged for a good run) |

No throughput in this document is a result, and no native-versus-cowfs verdict is made. Host load1 was 10 to 15 during these runs.

Negative controls, all in `bench/test_gates.py`:

- a file truncated after the write: g5 refuses.
- a read-back one byte short of the file: g5 refuses, through `gates.main()` too, with no rep row and no `seq.bin` left, and a sibling file in `big/` untouched.
- an unaligned expected size (1 MiB - 1, 2 MiB + 1, 21474836): g5 refuses.
- forged rows: `bytes` 4 MiB, `written_bytes` 5, `read_bytes` 7 with `read_matches` true; a read one byte short with the flag true; a string or bool where an integer belongs; a missing `read_bytes`; meta counts disagreeing with `bytes`. Each is invalid.
- each of those forged and short rows is placed in a native file, a second native file, the cowfs file and the noise-floor file in turn: `compare.py` exits 3 every time and names the file.
- a tree with a large file truncated to 5 bytes, a small file truncated, and a small file removed, each under an unchanged marker: `ensure_tree` refuses.

The tests that guard the tree rounding use scale 13, where the old 1 MiB floor stops hiding the defect. Run against the pre-fix `gates.py` and `compare.py`, 21 of the 24 tests fail or error (11 against the first revision of this PR, 662b4e8).

## Run the tests

This is the exact command a CI step would run. The workflows are not owned by this change, so the coordinator wires it in.

```sh
python3 -m unittest discover -s bench -v
```

24 tests, a few seconds, a few tens of MiB under a temp directory, no network and no cargo. Needs `git` for the tree fixture tests.

## Recipe for the later valid controls (not run here)

Run only on a quiet machine, serially, under the existing CPU lock in `bench/run-pair.sh`, after the cowfs arm is mounted by its owner.

1. Gate on load: record `uptime` first and proceed only when load1 is below the harness ceiling of 30 and the same order of magnitude on both arms. This session saw load1 8 to 10, so take a measured idle baseline first and do not assume a quiet machine.
2. Smoke with the real deliverable: `COWFS_BENCH_SCALE=2 python3 bench/gates.py --root ROOT --label NAME --reps 1 --gates g5 --no-resume` on each arm. Open the JSONL and check `bytes == written_bytes == read_bytes` and `read_matches` before anything else.
3. Native-versus-native control first: `sh bench/run-pair.sh NATIVE_ROOT NATIVE_ROOT2 5 g5`, then `python3 bench/compare.py --native ... --cowfs ... --noise-floor ...` to get the noise floor. Both roots must be plain directories.
4. Only then the cowfs arm, `sh bench/run-pair.sh NATIVE_ROOT COWFS_ROOT 5 g5`, at `COWFS_BENCH_SCALE=100`.
5. Treat any `compare.py` exit 3 as an invalid run, and any exit 2 as unmeasurable. Neither is a finding.
6. Result files go under `bench/out/` and stay ignored; copy only the summary into an issue.
