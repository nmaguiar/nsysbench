# How nsysbench works

This document explains what each `nsysbench` command and flag does, what it
measures, and the concepts behind the numbers it reports (GOPS, IOPS, SMT,
core classes, coefficient of variation, and the score formulas). See the
[README](../README.md) for build/install instructions and quick-start
examples.

## Global flags

These apply to every subcommand:

| Flag | Effect |
| --- | --- |
| `--json` | Print the full result as JSON instead of a colored terminal report. Also suppresses progress messages (equivalent to `--quiet`), so JSON output on stdout stays machine-readable. |
| `-q`, `--quiet` | Suppress the `nsysbench: ...` progress messages normally written to stderr while a benchmark runs. |

Progress messages go to **stderr**, results go to **stdout**, so
`nsysbench --json cpu > result.json` keeps the file clean even without
`--quiet`.

## Concepts

### GOPS (Giga-Operations Per Second)

The CPU benchmark reports throughput in GOPS. This is **not** a hardware
instruction-per-cycle measurement — it's a synthetic unit specific to
nsysbench's own kernels. Each kernel runs a fixed inner loop ("block") and
that block is defined to count as a fixed number of "operations":

- Scalar integer kernel: 20,000 loop iterations × 12 ops/iteration = 240,000 ops/block.
- Scalar floating-point kernel: 20,000 × 12 = 240,000 ops/block.
- SIMD kernel: depends on the detected SIMD path and its lane width —
  AVX2+FMA (x86_64) = 20,000 × 16 = 320,000 ops/block; SSE2 (x86_64), NEON
  (aarch64), and the scalar fallback = 20,000 × 8 = 160,000 ops/block.

Each worker thread runs its kernel's block repeatedly for the sample
duration, and total ops across all threads divided by elapsed seconds gives
operations/sec; dividing by 1e9 gives GOPS. Because the "operation" count
per block is fixed by definition rather than measured from the CPU, GOPS is
only meaningful as a **relative** number for comparing runs of nsysbench
against each other (same machine over time, or machine A vs. machine B) — it
does not correspond to a vendor-published FLOPS/IPS figure.

### Composite GOPS and the geometric mean

Each CPU topology stage runs three workloads — scalar-integer, scalar-fp,
and simd-fp — and combines their operations/sec (in GOPS) into one
**composite GOPS** figure using a **geometric mean**
(`(a × b × c)^(1/3)`, computed via the sum of logarithms). The geometric
mean is used instead of an arithmetic average because the three workloads
have very different absolute magnitudes (SIMD throughput is naturally much
higher than scalar throughput); an arithmetic mean would let the largest
workload dominate the composite figure, while the geometric mean treats a
proportional change in any one workload equally.

### IOPS and MB/s

The IO and network benchmarks report both a **throughput** figure (MB/s —
mebibytes, 1024×1024 bytes, per second) and, for IO, an **IOPS** figure
(I/O operations per second — how many individual block-sized read/write
calls completed per second). Small blocks and random access patterns tend
to be IOPS-bound (limited by per-operation overhead/latency), while large
blocks and sequential access tend to be throughput-bound (limited by raw
media/link bandwidth) — reporting both lets you see which regime a given
run fell into.

### SMT, physical cores, and logical CPUs

A **logical CPU** is a schedulable execution context (what the OS assigns
threads to). A **physical core** is a physical execution unit. **SMT**
(simultaneous multithreading — Intel calls it Hyper-Threading) lets one
physical core expose two or more logical CPUs that share the core's
execution resources. `smt_threads_per_core` is logical CPUs ÷ physical
cores. Running one thread per physical core ("physical-cores" stage)
isolates raw per-core throughput; running one thread per logical CPU
("all-logical" stage) shows whether SMT adds real throughput on top of
that, or just oversubscribes shared execution units.

### Core classes (performance/efficiency cores)

On hybrid CPUs (e.g. Intel P-core/E-core designs, Apple Silicon
performance/efficiency clusters), not all physical cores have the same
throughput. nsysbench detects these **core classes** from platform topology
data (Linux `sysfs` `core_type`/`cpu_capacity`, macOS `sysctl` perflevel
groups) and, when more than one class exists, runs a dedicated stage using
only the CPUs in each class (e.g. `performance-cores`, `efficiency-cores`),
in addition to the aggregate stages. This lets you see each class's
standalone throughput rather than an average blended across dissimilar
cores.

### Placement

`placement` describes how worker threads are pinned to specific logical
CPUs for a stage, which affects how trustworthy per-class/per-core numbers
are:

- **`pinned`** (Linux): `sched_setaffinity` binds each worker thread to one
  exact logical CPU, so a stage's threads run only on the CPUs it names.
- **`qos-advisory`** (macOS): threads are given a QoS class (user-initiated
  for performance cores, background for efficiency cores) as a hint to
  the scheduler; macOS does not expose direct core pinning, so placement
  is advisory rather than guaranteed.
- **`scheduler`** (other platforms): no pinning is attempted; the OS
  scheduler places threads freely.

### SIMD path

`simd_path` reports which vectorized instruction set nsysbench detected and
used for the SIMD workload at runtime: `avx2-fma` or `sse2` on x86_64,
`neon` on aarch64, or `scalar` as a portable fallback. This affects the
GOPS-per-block constant for that workload (see GOPS above), so composite
GOPS is only comparable across runs that used the same SIMD path.

### Coefficient of variation (stability)

Each workload in a CPU stage is timed three times (after a discarded 30ms
warm-up sample) and the **median** of the three is reported as
`operations_per_sec`. `coefficient_of_variation_percent` is the standard
deviation of those samples divided by their mean, as a percentage — a
measure of run-to-run noise. When any workload's CV exceeds 5%, the stage
carries a `stability_warning`, meaning the result was likely disturbed by
other system activity (background load, thermal throttling, frequency
scaling) and should be treated with more caution.

### Scaling factor, parallel efficiency, and SMT gain

- **Scaling factor** = a stage's composite GOPS ÷ the single-thread stage's
  composite GOPS. A value of 4.0 at 4 threads means throughput quadrupled.
- **Parallel efficiency %** = scaling factor ÷ thread count × 100. 100%
  means perfect linear scaling; well below 100% indicates contention
  (shared caches/memory bandwidth, SMT sharing, thermal limits).
- **SMT gain %** = `(all-logical GOPS ÷ physical-cores GOPS − 1) × 100`,
  reported only when both stages ran. It isolates the throughput SMT adds
  on top of one-thread-per-physical-core, separate from adding more
  physical cores.
- **Performance/efficiency ratio** = performance-class composite GOPS ÷
  efficiency-class composite GOPS, reported only on hybrid CPUs where both
  classes were benchmarked.

## Commands

### `cpu` — CPU raw processing and topology scaling

```bash
nsysbench cpu [--threads N] [--duration SECS] [--sequence]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `-t`, `--threads` | `0` | Worker-thread limit. `0` uses every logical CPU visible to the process (respecting `sched_getaffinity` on Linux). A positive value caps the CPUs used, in topology order. |
| `-d`, `--duration` | `8` | Measured seconds **per stage** (or, with `--sequence`, per thread count) — not a total run time. |
| `--sequence` | off | Run one stage per thread count, from 1 up to the selected thread limit, instead of the fixed topology checkpoints below. |

**Without `--sequence`**, nsysbench first detects CPU topology (logical
CPUs, physical cores, SMT ratio, core classes) and then runs a fixed set of
stages, each stage selecting a specific subset of CPUs:

1. **`single-thread`** — always runs, 1 CPU.
2. **One stage per core class** (e.g. `performance-cores`,
   `efficiency-cores`) — only added when that class has more than one CPU
   and fewer CPUs than the full selection (i.e. it's a genuine subset worth
   isolating).
3. **`physical-cores`** — one thread per physical core (skipping SMT
   siblings) — only added when this differs from both 1 and the full
   selection, i.e. when SMT is active.
4. **`all-logical`** — every selected logical CPU — only added when more
   than one CPU is selected.

Each stage runs the three workloads (scalar-integer, scalar-fp, simd-fp),
each sampled three times over `--duration`, and reports composite GOPS
(geometric mean, see Concepts), a stability warning if noisy, and — once all
stages complete — scaling factor and parallel efficiency relative to
`single-thread`. The overall result also reports `single_thread_score` and
`multi_thread_score` (each stage's composite GOPS × 100), `smt_gain_percent`,
`performance_efficiency_ratio`, and a headline `score` (see [CPU score
v2](#cpu-score-v2) below).

**With `--sequence`**, topology checkpoints are skipped; instead nsysbench
runs one stage per thread count from 1 through the selected limit
(`threads-1`, `threads-2`, ...), each for the same `--duration`. This is the
"expensive full scaling diagnostic" mentioned in the README — it directly
shows the scaling curve rather than a handful of checkpoints. Normal
(non-JSON) output renders this as a five-row UTF-8 sparkline of composite
GOPS vs. thread count; JSON output includes every individual stage's full
result.

### `memory` — memory raw speed

```bash
nsysbench memory [--size-mb N] [--duration SECS]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `-m`, `--size-mb` | `128` | Size of the in-memory buffer (array of `u64`) the benchmark allocates and operates on. |
| `-d`, `--duration` | `6` | Total benchmark time, split evenly across 4 phases (so each phase runs for roughly `duration / 4` seconds, minimum 1s each; the total is floored at 4s so every phase gets at least 1s). |

The four phases run in order and each reports a throughput in MB/s:

1. **Sequential write** — write every element of the buffer in index order.
2. **Sequential read** — read (and checksum, to avoid dead-code elimination)
   every element in index order.
3. **Random write** — write to a pseudo-random (LCG-generated) index,
   `elements` times per iteration, exercising cache/TLB behavior sequential
   access does not.
4. **Random read** — read from a pseudo-random index the same way.

The buffer stays resident for the whole run, so this measures the memory
subsystem (bandwidth and, for the random phases, latency/locality
sensitivity) rather than allocation cost. See [Memory
score](#memory-score) for how these four numbers combine into a score.

### `io` — storage IO raw speed

```bash
nsysbench io --path PATH [--block-kb N] [--file-size-mb N] [--duration SECS]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `-p`, `--path` | *(required)* | Directory/mountpoint to benchmark. A temporary file is created here and removed when the benchmark finishes. |
| `-b`, `--block-kb` | `4` | Size in KiB of each read/write call. |
| `-s`, `--file-size-mb` | `64` | Size of the test file (minimum 8 MiB enforced). |
| `-d`, `--duration` | `8` | Total benchmark time, split evenly across 4 phases (same `duration / 4`, minimum 4s total, rule as `memory`). |

The four phases, each measured for MB/s **and** IOPS:

1. **Sequential write** — write blocks in increasing offset order (wrapping
   at the file size).
2. **Sequential read** — read blocks in increasing offset order.
3. **Random write** — write to a pseudo-random block offset each iteration.
4. **Random read** — read from a pseudo-random block offset each iteration.

Small `--block-kb` and the random phases stress per-operation overhead
(latency-bound, visible in IOPS); large `--block-kb` and the sequential
phases stress raw transfer bandwidth (visible in MB/s). See [IO
score](#io-score) for how these combine.

### `network` — network raw speed

```bash
nsysbench network --target URL [--duration SECS]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `-t`, `--target` | *(required)* | URL to repeatedly `GET` for the duration of the benchmark. |
| `-d`, `--duration` | `8` | Total benchmark time; requests are issued back-to-back (no concurrency) until the deadline. |

Each request's body is streamed to a sink (not buffered in memory) so large
responses don't inflate memory use; only its byte count and elapsed time
are kept. The benchmark reports total throughput (MB/s), requests/sec, and
average per-request latency (ms). Because requests run sequentially and
un-pipelined, this measures single-connection round-trip performance to the
target, not aggregate/parallel bandwidth — point it at a nearby,
well-provisioned endpoint (the README example uses a Hetzner speed-test
file) for a stable baseline.

### `info` — hardware/storage metadata

```bash
nsysbench info [--path PATH]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `-p`, `--path` | `.` | Path/mountpoint whose filesystem metadata (filesystem type, total/available bytes via `df`) is reported. |

Unlike the other subcommands, `info` runs no workload — it only reports
static, performance-relevant environment metadata: CPU topology and vendor
details (from `/proc/cpuinfo` on Linux, `sysctl` on macOS), logical/physical
CPU counts, total memory, and storage filesystem/capacity for `--path`. Use
it alongside benchmark results to explain *why* two machines scored
differently (e.g. different core counts, filesystem, or available space).

### `run` — full suite

```bash
nsysbench run [--threads N] [--duration SECS] [--memory-mb N] [--io-path PATH] [--target URL]
```

This is also what runs when `nsysbench` is invoked with **no subcommand at
all** — but the two entry points use different defaults, so be aware which
one you're getting:

| Flag | `run` subcommand default | Bare `nsysbench` (no subcommand) default | Meaning |
| --- | --- | --- | --- |
| `-t`, `--threads` | `1` (i.e. omitting the flag runs CPU single-threaded) | `0` (all logical CPUs) | CPU worker-thread limit, passed through to the `cpu` benchmark. |
| `-d`, `--duration` | `5` | `5` | Seconds passed to each category's benchmark (memory/IO are floored at 4s internally, same as running them directly). |
| `--memory-mb` | `128` | `128` | Passed to the memory benchmark's `--size-mb`. |
| `--io-path` | `.` | `.` | Passed to the IO benchmark's `--path`. |
| `--target` | *(none — network skipped)* | *(none — network skipped)* | Passed to the network benchmark's `--target`; the network benchmark only runs if this is set. |

`run` executes CPU (as a single non-sequence stage set at the given thread
count), memory, and IO unconditionally, and network only if `--target` is
given, then sums each category's score into a `total_score` — see [Total
score](#total-score).

## Score formulas

Every category produces a `score` so results are comparable across runs and
machines; `run`/no-subcommand additionally sums them into one number.

### CPU score v2

```
score = 100 × single_thread_gops^0.35 × multi_thread_gops^0.65
```

`multi_thread_gops` is the `all-logical` stage's composite GOPS (or the
last stage run, if `all-logical` wasn't produced — e.g. single-CPU
machines). The exponents weight multi-threaded throughput more heavily
(0.65 vs. 0.35) since it dominates real-world multi-core workloads, while
still rewarding strong single-thread performance. This differs from the
simpler `single_thread_score`/`multi_thread_score` fields, which are just
each stage's own composite GOPS × 100 with no weighting between them.

### Memory score

```
score = (seq_write_MBs + seq_read_MBs + rand_write_MBs + rand_read_MBs) / 4 / 100
```

A straight average of the four phase throughputs, scaled down by 100 to
keep the number in a similar range to the other category scores.

### IO score

```
throughput_component = (seq_write_MBs + seq_read_MBs + rand_write_MBs + rand_read_MBs) / 4 / 50
iops_component        = (seq_write_iops + seq_read_iops + rand_write_iops + rand_read_iops) / 4 / 1000
score = throughput_component + iops_component
```

Combining both components means the score rewards devices that are strong
on either axis — a high-IOPS/low-latency SSD and a high-bandwidth/low-IOPS
sequential-optimized device can both score well, rather than the metric
being dominated by whichever axis has larger raw numbers.

### Network score

```
score = throughput_MBs / 20 + requests_per_sec / 50
```

### Total score

```
total_score = sum of the score of every category that ran
```

Categories that didn't run (e.g. network without `--target`) are simply
omitted from the sum, not counted as zero — so total scores are only
comparable between runs that executed the same set of categories.

## Output formats

By default, results print as a colored, human-readable terminal report
(the boxed "⚡ nsysbench performance report ⚡" for `run`, or a per-category
report for individual subcommands), with progress status lines on stderr as
each phase starts. `--json` switches stdout to a single pretty-printed JSON
document matching the structures above (`CpuResult`/`CpuSequenceResult`,
`MemoryResult`, `IoResult`, `NetworkResult`, `SystemInfo`, or `SuiteResult`
for `run`), and implicitly silences progress output the same way `--quiet`
does.
