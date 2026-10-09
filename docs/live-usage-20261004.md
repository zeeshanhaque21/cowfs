# Actual cowfs usage metrics, 2026-10-04

Captured at 19:32:54 to 19:33:29 UTC, with a subsequent shallow pool inventory.
This is observed development usage, not production acceptance.
No daemon restart, store mutation, GC, benchmark, lease return or runner change was performed.

## Current measured inventory

| Metric | Observed value | Meaning |
| --- | ---: | --- |
| Mounted pool directories | 9 | Six project labels, some with multiple pools |
| Numeric slot directories | 28 | Directory inventory, not active leases or successful builds |
| Core snapshots | 1 | The shared `base` snapshot contains the pools |
| Indexed unique blocks | 822,138 | Includes indexed blocks, not necessarily only currently reachable content |
| Unique uncompressed block bytes | 45,732,618,321 / 42.59 GiB | Does not count repeated references separately |
| Stored block record bytes | 16,595,466,496 / 15.46 GiB | Compressed payload plus record overhead |
| Record-size ratio | 2.756x | Unique uncompressed bytes divided by stored records |
| Record-size reduction | 63.71% | Not an isolated dedup saving or native-directory comparison |
| Pack files | 62 | Shallow stat of the native pack directory, no content reads |
| Pack file bytes | 16,595,484,937 | Captured separately from the control counters |
| Metadata file length | 958,959,616 | `meta.redb`, not a resident-memory measurement |
| Total store file lengths | 17,554,444,654 / 16.35 GiB | Packs, metadata and small state files; non-atomic inventory |
| Allocated blocks attributed to store files | 17,442,611,200 / 16.24 GiB | `st_blocks * 512`, not exclusive APFS physical ownership |

The observed pools are lumen, three nanoMuse pools, two cowfs pools, amicable, personal-assistant and hefty-search.
Numeric slots per pool are 3, 1, 16, 1, 0, 5, 1, 0 and 1 in the recorded inventory order.
No recursive slot walk or whole-tree apparent-byte recount was performed.
The table does not establish how many slots are active, leased, idle or abandoned.

## Process and health samples

PID 15263 started on 2026-10-03 at 20:44:29 local time.
The process sample showed 15 h 48 m 25 s elapsed and 179 m 47.59 s cumulative CPU time.
Reported control-server uptime was 56,003 seconds, approximately 15 h 33 m.
These clocks differ: process lifetime includes initialization before the control server becomes ready.
The daemon was restored after an earlier mount failure, so neither clock establishes uninterrupted multi-day availability.

Resident memory was 778,352 KiB, approximately 760.1 MiB.
Cumulative CPU time divided by elapsed process lifetime is approximately 0.190 CPU-core equivalents.
That is an average over this process lifetime, not an operation-cost benchmark or a workload-normalized efficiency figure.
The instantaneous `ps` CPU field was 63.2%, and host load averages were approximately 14.63, 18.19 and 20.62.
Those point samples do not prove a bottleneck or a quiet measurement window.

Three consecutive read-only status requests succeeded, with identical block counters.
Client wall times were 43.919 ms, 37.388 ms and 88.238 ms, median 43.919 ms.
They include process startup and the `rtk` wrapper, so they are not pure RPC latencies or filesystem operation timings.
One mounted pool-directory enumeration completed successfully in 249.288 ms including client startup.
This is a single operational probe, not a directory-performance distribution.

## Earlier actual workload evidence

The 2026-10-02 nanoMuse observation log has 20 samples across 571.512 seconds.
All 20 daemon control probes and all 20 mounted `node_modules` metadata reads succeeded in that sampled window.
The retained npm log evidence reports exit 0 for an install.
It does not establish the project's lint, typecheck or build exit codes, and the sampled probes do not establish continuous uptime.
Host load1 ranged from 9.16 to 15.71 during that window.

The separately audited live build trial in `docs/live-trial-metrics.md` measured three repetitions per phase on a loaded shared debug daemon.

| Phase | Native A median | Native B median | Cowfs median |
| --- | ---: | ---: | ---: |
| Clean build | 24.54 s | 21.86 s | 433.76 s |
| No-op build | 0.12 s | 0.20 s | 15.01 s |
| Edit/rebuild | 0.84 s | 0.88 s | 46.86 s |

Every cowfs repetition was slower than its native controls.
This is real observed slowdown, not a speedup from dedup or prolonged usage.
It is not a valid quiet release-daemon acceptance verdict because daemon build mode, host load and concurrent store traffic were not controlled.
No new build load was introduced for this report.

## What cannot be recovered from existing telemetry

- Multi-day uptime percentage, downtime duration or historical request failure rate.
- Completed-build count, build success rate or per-project latency distributions.
- Current per-write dedup savings: the live control API does not expose `put_bytes`, `dedup_bytes` or `dedup_hits`.
- Current apparent-tree versus native-size savings: no atomic whole-tree inventory or matched native copy exists for this sample.
- Live-byte versus garbage accounting from these status counters alone.
- Crash safety, rename durability or absence of data loss from fsck-free operational use.
- Performance of the newly merged fixes: the shared daemon is still the preserved older binary.

The earlier apparent/store ratio of 3.658x and the original corpus CDC-plus-compression ratio of 14.04x have different populations and methods.
Neither is a current usage dedup figure.

## Reproduction and provenance

The status command used the preserved CLI with `--socket /Users/zeeshanhaque/.cowfs/sock/daemon.sock --timeout 5 --json status`.
Store measurement used bounded `scandir` and `stat` of the native store, its state directory and pack directory, with a 10,000-entry cap per measured directory.
Pool measurement read only directory names and numeric slot directory types under the mounted `base/.treehouse`, with a 15-second process timeout.
No pack contents were opened for measurement.
Only the local daemon and CLI binaries were hashed.

Daemon SHA-256: `5c6f74d0e25593bf4a47af16d5e689ba765f1fe879e93c4658651d49ce93a5b1`.
CLI SHA-256: `1b7447f93fe956ba9de64efe6cb40a6d7ab4ae3a44c9c7f488da0bcaad05fb83`.
The filesystem implementation revision is not inferred from the current checkout.
Historical evidence is `spikes/nfs-loopback/out/live/nanomuse-monitor.jsonl` and the previously audited live metrics report.
Raw measurements for this capture are in `bench/out/live-usage-20261004/measurement.json`.

## Conclusion

Cowfs is supporting a meaningful development inventory, with substantial block-record compression and successful sampled operation.
The available real build measurements show severe overhead, and the historical telemetry is insufficient to certify reliability or dedup savings.
Using it for several days is useful evidence of exposure, not proof that the remaining correctness and performance gates are satisfied.
