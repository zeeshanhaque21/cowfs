# cowfs release decisions, 2026-10-10

Zee answered six questions that were blocking release work.
This file records each question in plain English, the answer, and the work that follows.
Nothing in it has been started yet.

## 1. How do we judge "fast enough" when checking a big project's status on Linux?

Background.
The speed goal says cowfs may be at most 1.5 times slower than a normal disk.
A normal Linux disk answers this in a fraction of a second, so 1.5 times that is a tiny number.
Early rough tests showed cowfs 2 to 3 times slower on this kind of small lookup.
On the Mac this test already uses a different rule: pass if cowfs adds less than one second.

Answer: **allow up to one extra second on Linux, the same rule as the Mac.**

Work that follows.
- Change the Linux branch of the status-check speed test (`bench/compare.py`, the g2 rule) to pass when cowfs adds less than one second.
- Update the docs that describe the g2 rule so Mac and Linux read the same.
- Re-run the test on a quiet machine to get the first official result.

## 2. What do we do about the Mac "pipe file" difference?

Background.
A named pipe is a special file that programs use to pass data to each other.
Opening one that nobody is reading gives a different error on the Mac than on a normal disk.
The error comes from the Mac's own network-disk code.
Nothing on our side or in any mount setting changes it.
It makes the Mac correctness test fail every time, although everything else matches a normal disk (issue #204).

Answer: **accept it as a known Mac limit and revisit after release.**

Work that follows.
- Add this single difference to the test's list of accepted known differences, marked as signed off by Zee on 2026-10-10, so the Mac correctness test can pass.
- After release, try another Mac connection method (FUSE-T or macFUSE, issue #37) and see whether the difference goes away.

## 3. Is the random file-operation stress test enough as it is?

Background.
The stress test passed 18 out of 18 runs against the Linux test machine's own disk type (btrfs), which is the bar set on 2026-10-09.
Two rarely used operations are switched off on both sides: cutting a hole out of the middle of a file, and inserting space into it.
The amount of disk space each file uses is not compared.
One difference is known: after zeroing a range, cowfs reports half the space btrfs does (64 blocks against 128).

Answer: **also compare the space used before release.**

Work that follows.
- Add the space-used comparison to the stress test.
- Decide whether the known zero-range difference is a real defect or an acceptable difference, and record the reason.
- Release notes must still name the two switched-off operations.

## 4. How should cleanup of unused data be triggered?

Background.
Cleanup (garbage collection) works, but only when a person runs the command by hand.
Nothing runs it automatically, and the "recently used" hints it should use are never filled in.
A long-running disk therefore fills up slowly (issue #10).

Answer: **an automatic timer, plus a trigger when the disk is nearly full.**

This differs from the recommendation in `docs/gc-scheduling-10-20261009.md`, which advised a timer that is off by default and no low-disk trigger.
The reason given there is that cleanup needs free headroom, because rewritten data lands before the old data is removed.
Started on an almost full disk it can fail with "storage full" and make the problem worse.

Work that follows.
- Add the timer inside the daemon, with a minimum gap between runs and an idle check.
- Start filling in the recently-used hints.
- Add the low-disk trigger as its own piece of work, and design it around the risk above: only start when enough headroom exists, back off after a failure, and never retry faster than the backoff.
- The risk must be tested on purpose: a nearly full disk, a failed cleanup, and a retry.
- The timer's default (on or off) was not decided and needs one more answer.

## 5. What goes into the prepared "starting copy" for new workspaces, and who attaches it?

Background.
Fresh workspaces start from a prepared starting copy.
Today it holds only source code, so the first workspace in each pool does the whole build from scratch (issue #123).
Two decisions were open: whether the starting copy may hold already-built files, and who attaches a new workspace's snapshot to its folder.

Answer: **allow built files in the starting copy, and let the helper tool attach and detach the snapshot.**

Work that follows.
- Add an optional source folder to the refresh command, with a daemon check that the folder is a working copy at the right commit and has no uncommitted changes to tracked files.
- Before relying on it, measure whether built files still work when the copy is mounted at a different folder path, because they may contain absolute paths.
  The result may be that this is only supported with the fixed-path build mode.
- The helper tool creates the snapshot, attaches it after creation, and detaches it when the workspace is returned.
- A failed build must leave the old starting copy untouched.
- Add a real-mount test, and extend the long acceptance run (about 17 minutes).

## 6. What is the oldest Linux we promise to support?

Background.
The private build-folder feature needs support from the Linux kernel.
Automatic checks run on the newest hosted Linux (about version 6.17) and on Ubuntu 22.04 (Linux 6.8).
One manual run was done on 6.12.
Nothing older is tested (issue #171).

Answer: **Linux 6.8 or newer.**

Work that follows.
- State "Linux 6.8 or newer" in the user documentation and release notes.
- Keep the Ubuntu 22.04 check in the automatic tests, and plan for the day that runner image is retired.
- Versions between 6.8 and 6.12 are covered by the promise but are not tested individually.

## Still undecided

These were listed as needing a decision but were not part of the six.
- Whether the simulated power-cut testing is enough for release (gate g6).
- Whether to accept the two attack gaps that cannot be fixed from the server side (issue #262).
- Whether to leave Linux's torn reads alone (issue #45).
- Whether the base record should keep prior details after a half-failed swap (issue #323).
