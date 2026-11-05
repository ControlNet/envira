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

## Not changed (recommendations)

- Detection runs 8 commands at once; node-based `--version`/`npm ls -g` cost 110-150 MiB each.
  `npm ls -g --depth=0` (all packages, one call shared by every npm tool) would replace one call
  per npm tool.

## How to measure

- Sampler: walk `/proc/*/stat` for the tree from the root pid, read `VmHWM`/`VmRSS`/`Threads`
  and utime+stime every 20 ms; `VmHWM` is monotonic, so the last sample before exit is the peak.
- GitHub's anonymous API limit (60/h per IP, shared with everything else on the network) ran out
  during the first container run. A stand-in for `api.github.com` inside the container avoids it:
  a throwaway CA added with `update-ca-certificates`, a leaf for `api.github.com` (webpki rejects
  a self-signed CA used as the leaf), a small Python HTTPS server answering
  `releases/latest` with `browser_download_url`s on the real github.com (downloads are not rate
  limited), and `docker run --add-host api.github.com:127.0.0.1`.
- `docker run --memory=512m --memory-swap=512m --cpuset-cpus=0,1` imitates a small VM
  (`nproc` and Rust's `available_parallelism` both see 2 CPUs).
