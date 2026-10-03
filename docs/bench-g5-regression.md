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

- `UNIT` in `bench/gates.py` is the one table of chunk sizes. `counts()` rounds a scaled byte count down to a whole number of units, with a floor of one unit. The writers use the same `UNIT` values, so they cannot drift apart again.
- Full scale is unchanged: `big_bytes` 1 GiB, `large_bytes` 8 MiB.
- `g5` stats the file after `fsync` and refuses (exit, nothing recorded) when the written size is not `counts()["big_bytes"]`. That is a harness bug and must not produce a number.
- `g5` records `bytes` (expected), `written_bytes` and `read_bytes`. `read_matches` is `read == written == expected`, so a real short read still records `false`.
- `ensure_tree` stats each large file and refuses on a size mismatch, and its `.generated` marker now includes `large_bytes`, so a tree built at another size is regenerated.
- `compare.py` exits 3 and prints nothing comparable when any g5 rep did not read back what it wrote. Pre-fix result files are rejected by this, which is intended: their g5 throughput was of a short read.

## After the fix, measured

Same entrypoint, one rep each.

| scale | expected | written | read | `read_matches` | root |
|---|---|---|---|---|---|
| 2 | 20971520 | 20971520 | 20971520 | true | `bench/out/g5-regression/root-fixed2` (cowfs slot) |
| 100 | 1073741824 | 1073741824 | 1073741824 | true | throwaway dir on local APFS, deleted after |

These confirm the byte accounting only. No throughput in this document is a result, and no native-versus-cowfs verdict is made here.

Negative controls, in `bench/test_gates.py`:

- a file truncated after the write: g5 refuses (`counts() says`).
- a read-back one byte short of the file: `read_matches` is `false`, `read_bytes` is 1 byte under `written_bytes`.
- an unaligned expected size (1 MiB - 1, 2 MiB + 1, and the original 21474836): g5 refuses.
- `compare.py` on the pre-fix baseline file exits 3.

The same 13 tests run against the pre-fix `gates.py` and `compare.py` give 7 failures and 4 errors, so they do detect the defect.

## Run the tests

```sh
python3 -m unittest discover -s bench -v
```

They write under a temp directory, a few tens of MiB, in well under a second.

## Recipe for the later valid controls (not run here)

Run only on a quiet machine, serially, under the existing CPU lock in `bench/run-pair.sh`, after the cowfs arm is mounted by its owner.

1. Gate on load: record `uptime` first and proceed only when load1 is below the harness ceiling of 30 and the same order of magnitude on both arms. This session saw load1 8 to 10, so take a measured idle baseline first and do not assume a quiet machine.
2. Smoke with the real deliverable: `COWFS_BENCH_SCALE=2 python3 bench/gates.py --root ROOT --label NAME --reps 1 --gates g5 --no-resume` on each arm. Open the JSONL and check `bytes == written_bytes == read_bytes` and `read_matches` before anything else.
3. Native-versus-native control first: `sh bench/run-pair.sh NATIVE_ROOT NATIVE_ROOT2 5 g5`, then `python3 bench/compare.py --native ... --cowfs ... --noise-floor ...` to get the noise floor. Both roots must be plain directories.
4. Only then the cowfs arm, `sh bench/run-pair.sh NATIVE_ROOT COWFS_ROOT 5 g5`, at `COWFS_BENCH_SCALE=100`.
5. Treat any `compare.py` exit 3 as an invalid run, and any exit 2 as unmeasurable. Neither is a finding.
6. Result files go under `bench/out/` and stay ignored; copy only the summary into an issue.
