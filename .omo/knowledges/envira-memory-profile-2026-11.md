# envira memory and CPU profile (2026-11-05)

Measured after a report that envira exhausted a memory-limited VM. Numbers are peak RSS
(`VmHWM` from `/proc/<pid>/status`, sampled every 20 ms over the whole process tree), glibc
release build on a 60-core host unless noted; installs ran only in `envira-it-ubuntu` containers.

## Where the memory went

| Scenario | envira itself | envira subprocesses | biggest children |
|---|---|---|---|
| `status`, `status --json`, `doctor` | 11 MiB | – | detect commands, 8 at once: node CLIs 110-150 MiB each (`npm ls -g` per npm tool, opencode, agy); tree ~290 MiB |
| `status --latest` (non-GitHub queries re-run) | 75-80 MiB (musl release: 64) | – | same as status |
| TUI idle | 80 MiB HWM at start (the latest check), ~33 MiB after | – | – |
| `fetch-release` gh / yazi / superfile (15 MB assets) | – | 112 / 98 / 85 MiB each, 3 at once in one run | – |
| install of gh yazi superfile build-tools xh gdown | 12-14 MiB | 3 × ~100 MiB at the same time | rustup-init 630-670 MiB; `cargo install` = one rustc per core, 200-560 MiB each (tree 7 GB at 60 cores) |
| a step printing 2M lines (CLI, also with `-q`) | 190 MiB, 27 s CPU | – | – |
| same in the TUI | 243 MiB kept after the run, 8% CPU idle afterwards | – | – |

In a 512 MiB / 2 CPU container rustup-init is OOM-killed ("Killed" in the rust-toolchain log)
whatever envira does; `RUSTUP_IO_THREADS=1` brings it to ~210 MiB and it succeeds (+3 s on
2 CPUs, +4-9 s on 60). `--profile minimal` and `RUSTUP_CONCURRENT_DOWNLOADS=1` do not help.
envira's runner now sets `RUSTUP_IO_THREADS=1` for every step unless the user set it.

## Causes found

- `fetch.rs` read the asset into a `Vec` (`bytes()` then `to_vec()`: 2x), read each binary out of
  the archive into a `Vec`, then cloned it: peak ≈ 2 × asset + 2 × binaries. Now the download
  streams into a temp file through a SHA-256 writer and entries are copied to staged files:
  ~12 MiB regardless of the asset.
- `latest.rs` parsed every JSON answer into a `serde_json::Value`. api.anaconda.org returns
  11 MB for conda and 4 MB for jupyterlab; the `Value` trees were ~60 MB. `json_path` now walks
  the text with a `DeserializeSeed` and `IgnoredAny`: 40 MiB (the bodies themselves remain).
- `exec.rs` wrote each log line with `writeln!` to an unbuffered `File` (one write syscall per
  format piece) while the pump read the pipe as fast as it could into an unbounded channel: the
  output queued in memory. The line channel is bounded now (the step waits on its pipe) and a
  line is one `write_all`: 2M lines take 12 MiB and half the CPU.
- The TUI kept every line of every unit and scanned the selected log backwards for a hint on
  every frame. It keeps the last 20k lines per unit now.
- `/tmp` on tmpfs (Fedora, Arch) is still RAM: the staged asset and binaries of a release
  download sit there until the step ends (`tempfile` honours `TMPDIR`). That is about half the
  old peak, freed with the temp dir, and swappable.

## Done after the profile (2026-11-05)

- tokio had one worker per core: 61 threads and ~4 GB VIRT on a glibc build (per-thread malloc
  arenas). `main.rs` now uses at most 4 workers: the TUI has 7 threads and ~490 MB VIRT.
- `cargo install` built with one job per core. Steps now get `CARGO_BUILD_JOBS` = one per GiB
  available (`MemAvailable`, or the cgroup v2 limit minus usage when lower), unless that is
  every core or the user set it. Measured on a 60-core host: 1 GB container -> 1, 6 GB -> 5,
  no limit -> unset. Only one cargo build runs at a time (the `build` lane).

- Detection (status, the TUI's first scan) ran 8 commands at once, many of them Node/Python CLIs
  (`opencode --version`, `conda --version`, `npm ls -g`) at 30-150 MiB each. Now
  `runner::process_jobs` allows about one per 192 MiB available (2 to 8, cgroup-aware), for
  detection, `installed` queries and command-type latest queries (dnf/zypper/apt-cache; HTTP
  queries stay at 6). The npm `installed` query is one `npm ls -g --depth=0` shared by every npm
  tool. Measured on a workstation (many tools installed), peak tree RSS of `envira status`:
  295 MiB before; under `systemd-run --user --scope -p MemoryMax=768M` (3 jobs) or 512M (2 jobs)
  ~200 MiB after, 1.8 s -> 2.7-3.5 s. Without a limit nothing changes (8 jobs).
- Measuring a memory limit on the host without a VM: `systemd-run --user --scope --quiet
  -p MemoryMax=512M -p MemorySwapMax=0 <cmd>` puts the command in its own cgroup (shown in
  /proc/self/cgroup), which is also what `available_memory` reads.
- `available_memory` finds the hierarchies in /proc/self/mountinfo, not at a fixed path: the
  cgroup2 mount (`memory.max`/`memory.current`, at /sys/fs/cgroup or /sys/fs/cgroup/unified on
  hybrid systems) and the v1 memory mount (`memory.limit_in_bytes`/`memory.usage_in_bytes`), with
  the process's path made relative to the mount's root (a v1 container sees its own cgroup as
  the root), and takes the tightest limit up to each mount point. Checked in Docker (v2):
  1 GB -> CARGO_BUILD_JOBS=1, 3 GB -> 2, no limit -> unset. v1 is covered by unit tests only.

## Scan memory budget (2026-11-05, second pass)

Target set by the user: the scan (`envira status`, the TUI's first scan) within 200 MB, envira
plus every process it starts. A cap on the number of commands cannot promise that: `agy
--version` alone is 125-160 MiB and `opencode --version` 120-130 MiB (single-file builds that
map most of their 200+ MB binary), `npm ls -g` 110 MiB, `apt-cache policy <pkg>` 92 MiB (it maps
the package cache), and without a cgroup limit `process_jobs` allows 8 at once (tree ~300 MiB).

`runner/budget.rs` gates every `capture_as` (detection, checks, `installed` and latest command
queries; steps are not gated):
- The limit follows the memory available, not the machine's total (user's call, same day):
  `limit_for(available_memory())`, where available is `MemAvailable` or what a cgroup limit
  leaves (the tightest). From 1 GiB available up: half of it, which never holds a scan back
  (status 1.3-1.4 s, ~210-270 MiB, faster than before thanks to the lighter detects). Under
  1 GiB: `SCAN_MEMORY` (176 MiB: 200 MB less a margin), at most 3/4 of what is left. Unknown:
  no budget on macOS (`OPEN`; the user's call: no Mac has less than 8 GiB, and without /proc
  nothing is measured anyway), `SCAN_MEMORY` elsewhere. Read again while none of envira's
  commands runs, at most once a second (the TUI lives long; reading it while ours run would
  count them against themselves).
- A command starts only when envira's own RSS + the reservations of the running commands + its
  estimate fit in the limit, or when nothing else runs.
- Estimate, in order: its own peak seen before (x1.25 + 2 MiB); for a command that ended before
  it could be sampled, its static estimate capped at 32 MiB; the highest peak of another command
  of the same program (`apt-cache policy git` for `apt-cache policy curl`); else static: bash
  4 MiB + per program a Node/Bun/Deno script or runtime 160, Python/other script 64, native ELF
  its file size (8-160), not installed 0, builtins 0. The shell parsing behind it
  (`programs`) is rough on purpose: first word of each simple command, assignments skipped,
  `$(...)` inside double quotes split out.
- Under a limit of less than 1 GiB, a program never measured runs one command at a time until
  one has ended: otherwise six `apt-cache policy` started together at a 12 MiB estimate each
  (seen: 220 MiB tree). Not above: with plenty of memory it only slows a first scan, and where
  nothing can be measured (macOS) the program stays unmeasured, so its commands would queue on
  every scan.
- While a command runs, its tree's `VmHWM` (summed over `/proc/<pid>/task/*/children`, which
  needs CONFIG_PROC_CHILDREN; only the pid where missing) is sampled every 10 ms and the
  reservation grows with it. Not at once: a sample right after fork reads the pre-exec process
  (`node --version` was "learned" at 8 KiB; it is 16 MiB).
- Peaks go to `<state_dir>/probes.toml` (a cache), saved by `main` after the command. Rewritten
  only when a command is new, outgrew its headroom, or fell under half (then it fades by a
  quarter per run); peaks vary by about a third between runs (codex 33-46 MiB, gh 25-37), and
  any-change rewrites happened every run. Written without fsync (`files::replace_cache`): one
  fsync on the busy workstation disk took 2.2 s and made `status` stall 4-10 s now and then.
  Without fsync it still stalled (2 in ~40 runs under a cgroup limit): caught with a sampler
  that dumps `/proc/<pid>/task/*/{wchan,syscall}` when envira has had no child for 1 s, the
  main thread was in D state in `renameat` (syscall 264), wchan `rq_qos_wait`. ext4's
  `auto_da_alloc` flushes the data of a file renamed over an existing one, and that write
  waited in the block layer's throttling. `replace_cache` now removes the old file first (a
  rename onto a free name flushes nothing; a reader may briefly see no cache).

Also: the jupyter detect reads the package metadata (`importlib.metadata`, 17 MiB) instead of
`jupyter-lab --version` (python 57 + node/yarn 98 MiB), and pm2's reads the package.json behind
the command on PATH instead of `npm ls -g pm2` (110 MiB, and it missed a pm2 installed under an
older npm prefix: status said "not installed").

Measured on the workstation (summed RSS of the tree, 20 ms samples; old = 26fff17; memory
available set with `systemd-run --user --scope -p MemoryMax=...`):

| Scenario | old peak / time | now peak / time |
|---|---|---|
| `status`, ~100 GiB available (budget open) | 279-313 MiB / 1.7-1.8 s | 209-271 MiB / 1.3-1.4 s |
| `status`, MemoryMax=2G (open) | – | 209-271 MiB / 1.3-1.4 s |
| `status`, MemoryMax=1G (just under 1 GiB available: 200 MB budget) | – | 165-171 MiB / 2.5-2.6 s |
| `status`, MemoryMax=768M | – | 136-138 MiB / 3.2 s |
| `status`, MemoryMax=256M | 195-205 MiB / 3.6-4.6 s | 135-168 MiB / 3.1 s |
| 200 MB budget forced on the big host, no saved peaks / peaks saved | – | 136-168 MiB / 3.1 s; 134-170 MiB / 2.5 s |
| TUI first scan, 200 MB budget forced | 300-303 MiB / 1.6-1.7 s | 134-171 MiB / 2.4-3.0 s |
| TUI start, no caches at all, latest refresh + update check over the network, 200 MB budget forced | 221 MiB | 174 MiB (envira itself at most 54 MiB, during the latest JSON) |

The largest single command bounds what is possible: agy alone is up to 160 MiB plus envira's
11-20 MiB. Batching every apt package into one `apt-cache policy a b c` (needs a multi-package
query in the catalog format) would shorten the serialized latest phase (~25 x 0.15 s).

## How to measure

- Sampler: walk `/proc/*/stat` for the tree from the root pid, read `VmHWM`/`VmRSS`/`Threads`
  and utime+stime every 20 ms; `VmHWM` is monotonic, so the last sample before exit is the peak.
- The TUI: start the sampler as the parent of envira inside tmux (the sampler keeps the
  terminal for envira and writes its report to a file) and record a per-100 ms timeline of
  envira's RSS and the tree's. Attaching to the pid after start misses the first scan's first
  half second, where the biggest command may run. Record each process's first and last sample
  too: a stretch with no child alive points at envira itself waiting (that is how the fsync
  stall showed), confirmed with `strace -T -e trace=fsync`.
- `/usr/bin/time -f %M` gives the exact peak of a command too short for 20 ms sampling.
- GitHub's anonymous API limit (60/h per IP, shared with everything else on the network) ran out
  during the first container run. A stand-in for `api.github.com` inside the container avoids it:
  a throwaway CA added with `update-ca-certificates`, a leaf for `api.github.com` (webpki rejects
  a self-signed CA used as the leaf), a small Python HTTPS server answering
  `releases/latest` with `browser_download_url`s on the real github.com (downloads are not rate
  limited), and `docker run --add-host api.github.com:127.0.0.1`.
- `docker run --memory=512m --memory-swap=512m --cpuset-cpus=0,1` imitates a small VM
  (`nproc` and Rust's `available_parallelism` both see 2 CPUs).
