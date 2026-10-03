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
- `ensure_tree` verifies the fixture after generating it and again on every marker hit, and refuses (never repairs) on any difference. The contract is an exact tree: the top level holds only `d000` to `d255`, `big`, and optionally `.git` and `.generated`; each `dNNN` holds exactly the expected `fNNNNN` regular files of 256 bytes and nothing else; `big` holds exactly `b000` to `b(large_files-1)`, each exactly `large_bytes`. Stray, missing, renamed, non-file and wrong-length entries all refuse, with a message that names the tree to remove. The marker includes `large_bytes`. The check is untimed setup, one scandir and stat pass, identical on both arms.
- The tree check is by name and length only. A same-length content corruption is not detected, because hashing 100k files on every run would dominate setup and was not part of the acceptance. This is a known limit, not a guarantee.
- `gates.py` records `scale` in the meta record, and `scaled_bytes()` is the one function that turns a scale into a byte count, used by both `counts()` and `compare.py`.
- `compare.py` validates every input file (`--native`, `--cowfs` and `--noise-floor`) before it loads or compares anything, and any failure exits 3 with no comparison printed. There is no legacy exemption, no flag and no compatibility mode. A file is valid only if all of these hold:
  - exactly one `meta` record, before the first rep, with no duplicate or conflicting meta;
  - `counts.big_bytes` is an integer (not bool, not float), at least 1 MiB, and a multiple of 1 MiB;
  - when the meta has a `scale`, it is a finite number and `scaled_bytes("big_bytes", scale)` equals `counts.big_bytes`. So 3 MiB is valid at scale 0.3 and impossible at scale 100;
  - at least one g5 rep, so every comparison needs g5 in every input. A file with no meta, no reps, or no g5 reps is invalid;
  - every g5 rep has `bytes`, `written_bytes` and `read_bytes` present, all integers (not bool, not float, not string), and all equal to the meta `big_bytes`.
  The `read_matches` flag is never consulted. A boolean-only row, however well aligned, is invalid.
- Scope: this is a byte-accounting check of the harness output. It cannot tell a hand-written but internally consistent file from a real run, and it does not try to. Result files written before this change that lack a meta `scale` are still accepted if their counts are valid and every g5 row has the three integer fields; pre-fix rows are not.

## Semantics that changed, and results that are no longer comparable

- At non-multiple scales `big_bytes` and `large_bytes` now round down to the unit instead of truncating. At scale 2, `big_bytes` moved from 21474836 to 20971520. `large_bytes` is 1048576 at scale 2, as before, because the 1 MiB floor is kept.
- `large_bytes` differs from the old value only where the old value was above 1 MiB and not a multiple of 64 KiB, for example scale 13: 1090519 became 1048576. Before this fix the tree files there were 1048576 bytes while `counts()` said 1090519.
- Scale 100 is unchanged for both keys. Results at scale 100 are comparable with earlier ones.
- Any smoke run at a non-multiple scale before this change is not a valid comparison with one after it. Those runs had a g5 that wrote fewer bytes than recorded, a `read_matches` of false, and a throughput computed on the expected count rather than the written one. Do not mix them. The meta `counts` differ, so a resumed run cannot mix them, and `compare.py` exits 3 on the old files, including any pre-fix result file with a g5 row that has no `written_bytes` and `read_bytes`, whatever its `read_matches` says. That includes old valid-looking scale-100 files: they cannot be verified, so they cannot be compared.
- The first run after this change regenerates every existing tree, because the marker format changed.
- Not changed here and worth its own issue: `small_files` is also rounded by the tree generator (`256 * max(1, total // 256)`), so scale 100 generates 99840 files, not 100000. g4 reports `entries`.

## After the fix, measured

Same entrypoint, one rep each. Byte counts only.

| scale | expected | written | read | recorded | root |
|---|---|---|---|---|---|
| 2 | 20971520 | 20971520 | 20971520 | yes | `bench/out/g5-regression/root-r2` (cowfs slot) |
| 100 | 1073741824 | 1073741824 | 1073741824 | yes | throwaway dir on local APFS, deleted after (run on the first revision of this PR; the g5 byte path is unchanged for a good run) |

Real tree fixture through the entrypoint, smallest scale: `COWFS_BENCH_SCALE=0.001 bench/gates.py --gates g4 --reps 1` (counts: 10 small-file slots, 10 large files of 1 MiB; the tree is 256 `dNNN` directories of one 256-byte file, `big/` with 10 files, `.git`, `.generated`). The first run needs `cargo fetch` for the g4 corpus and took about 25 minutes of network at load 12 to 20, almost all of it the fetch, so use a warm `COWFS_BENCH_CARGO_HOME` for reruns. With the tree built, a rerun on an intact tree passes the verification. After the same rerun on my own fixture with one change each, the entrypoint exits 1 before any rep: an extra `big/b099`, `big/b003` truncated to 5 bytes, a stray top-level `stray.txt`. Each change was then reverted and the intact tree passes again.

No throughput in this document is a result, and no native-versus-cowfs verdict is made. Host load1 was 10 to 15 during these runs.

Negative controls, all in `bench/test_gates.py`:

- a file truncated after the write: g5 refuses.
- a read-back one byte short of the file: g5 refuses, through `gates.main()` too, with no rep row and no `seq.bin` left, and a sibling file in `big/` untouched.
- an unaligned expected size (1 MiB - 1, 2 MiB + 1, 21474836): g5 refuses.
- a regression matrix of 33 invalid result files, each placed in turn as a native file, a second native file, the cowfs file and the noise-floor file (132 runs), where `compare.py` must exit 3, name the file, and print no `PASS`: legacy boolean-only rows (aligned 1 GiB, with and without the byte fields); no meta; rep before meta; duplicate and conflicting meta; meta without counts or without `big_bytes`; `big_bytes` bool, float, zero, negative, below 1 MiB, unaligned; `bytes` bool, float, zero, negative; missing `written_bytes` or `read_bytes`; string-typed count; the forged 5-written 7-read row with the flag true; a read one byte short with the flag true; forged aligned 3 MiB under a 4 MiB meta; equal counts that differ from meta; 3 MiB under a meta with scale 100; scale as string, bool, infinity; an empty file; meta only; no g5 reps; a g5 rep with no metrics; non-JSON and a missing file.
- six valid modern files that must still pass in every slot, including 3 MiB at scale 0.3, 20 MiB at scale 2, the 1 MiB floor at scale 0.001, and one whose flag is false but whose numbers all agree (numbers decide, not the flag).
- trees: a large file truncated to 5 bytes, a small file truncated, a small file removed, a large file removed, a whole `dNNN` directory removed, and strays (extra `big/b099`, a top-level file, an extra `d300` directory, an extra file or directory inside a `dNNN`, a directory inside `big`), each under an unchanged marker: `ensure_tree` refuses.

The tests that guard the tree rounding use scale 13, where the old 1 MiB floor stops hiding the defect. 26 tests. Run against the pre-fix `gates.py` and `compare.py` (`ab99868`), 22 fail or error. Run against the previous head of this PR (`75f16a9`), 4 fail or error, and the old `compare.py` exits 0 on 18 of the 33 invalid cases (all legacy, missing-meta, bad-meta and no-g5 shapes).

## Run the tests

This is the exact command a CI step would run. The workflows are not owned by this change, so the coordinator wires it in.

```sh
python3 -m unittest discover -s bench -v
```

26 tests, a few seconds, a few tens of MiB under a temp directory, no network and no cargo. Needs `git` for the tree fixture tests.

## Recipe for the later valid controls (not run here)

Run only on a quiet machine, serially, under the existing CPU lock in `bench/run-pair.sh`, after the cowfs arm is mounted by its owner.

1. Gate on load: record `uptime` first and proceed only when load1 is below the harness ceiling of 30 and the same order of magnitude on both arms. This session saw load1 8 to 10, so take a measured idle baseline first and do not assume a quiet machine.
2. Smoke with the real deliverable: `COWFS_BENCH_SCALE=2 python3 bench/gates.py --root ROOT --label NAME --reps 1 --gates g5 --no-resume` on each arm. Open the JSONL and check `bytes == written_bytes == read_bytes` and `read_matches` before anything else.
3. Native-versus-native control first: `sh bench/run-pair.sh NATIVE_ROOT NATIVE_ROOT2 5 g5`, then `python3 bench/compare.py --native ... --cowfs ... --noise-floor ...` to get the noise floor. Both roots must be plain directories.
4. Only then the cowfs arm, `sh bench/run-pair.sh NATIVE_ROOT COWFS_ROOT 5 g5`, at `COWFS_BENCH_SCALE=100`.
5. Treat any `compare.py` exit 3 as an invalid run, and any exit 2 as unmeasurable. Neither is a finding.
6. Result files go under `bench/out/` and stay ignored; copy only the summary into an issue.
