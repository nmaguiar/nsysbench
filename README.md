# nsysbench

`nsysbench` is a command-based Rust benchmarking tool to compare machine raw performance.

See [docs/BENCHMARKS.md](docs/BENCHMARKS.md) for a full explanation of every
command and flag, and the concepts behind the numbers they report (GOPS,
IOPS, SMT, core classes, and the score formulas).

## Features

- Colorful, UTF-8 friendly terminal output
- CPU score v2: scalar integer, scalar floating-point, and SIMD compute kernels
- Topology-aware CPU stages for single thread, core classes, physical cores, SMT, and all logical CPUs
- Memory raw speed benchmark (sequential/random read and write)
- Storage IO raw speed benchmark (sequential/random read and write with IOPS + throughput)
- Network raw speed benchmark to a target URL
- JSON output for machine consumption
- Progress messages on stderr by default (disable with `--quiet`)
- Category scoring + total score for easy VM/bare-metal comparison
- Cross-platform Rust implementation (Linux, macOS, Windows; major CPU architectures supported by Rust targets)

## Build

```bash
cargo build --release
```

## Usage

Run full suite (CPU + memory + IO, optional network):

```bash
cargo run -- run --threads 2 --duration 5 --memory-mb 256 --io-path /tmp --target https://speed.hetzner.de/1MB.bin
```

Run the topology-aware CPU benchmark (about 30 seconds on a typical hybrid CPU):

```bash
cargo run --release -- cpu
cargo run --release -- cpu --threads 1
cargo run --release -- cpu --threads 8
```

`--threads 0` (the default) uses every logical CPU available to the process.
`--duration` is the measured seconds for each topology stage. Results include
the selected SIMD path, placement capability, per-workload stability, physical
core throughput, all-logical throughput, and SMT gain when applicable.

Show the performance-relevant CPU, memory, and storage metadata separately:

```bash
cargo run -- info --path /tmp
cargo run -- --json info --path /tmp
```

Run the expensive full scaling diagnostic from one worker through the requested
thread count. Each test runs sequentially for the same duration; normal terminal
output includes a five-row UTF-8 GOPS chart, while JSON output contains every
individual stage:

```bash
cargo run -- cpu --threads 8 --duration 5 --sequence
cargo run -- --json cpu --threads 8 --duration 5 --sequence
```

Run only memory benchmark:

```bash
cargo run -- memory --size-mb 256 --duration 8
```

Run only IO benchmark:

```bash
cargo run -- io --path /tmp --block-kb 4 --file-size-mb 64 --duration 8
```

Run only network benchmark:

```bash
cargo run -- network --target https://speed.hetzner.de/1MB.bin --duration 8
```

JSON output:

```bash
cargo run -- --json run --threads 2 --duration 5 --io-path /tmp
```

Normal terminal output reports benchmark progress on stderr. This keeps stdout
available for results while showing the active benchmark phase. Use `--quiet`
to suppress those messages; `--json` also suppresses them so JSON output stays
machine-readable.

```bash
cargo run -- --quiet cpu --threads 2 --duration 5
```

## Cross compilation

Use Rust target triples to compile for major OS/architectures. For portable Linux
release binaries, prefer the statically linked musl targets; these run on older
glibc systems such as AWS Linux instances and do not require the glibc version
of the build host:

```bash
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
# Linux: install musl-tools first (for example, apt-get install musl-tools)
cargo build --release --target x86_64-unknown-linux-musl
```

## Release binaries

The **Release binaries** GitHub Actions workflow builds native release archives for
Linux, macOS, and Windows on x86_64 and ARM64. The Linux x86_64 and ARM64
(AWS Graviton) archives use static musl builds, avoiding runtime glibc version
requirements. Publishing a GitHub release attaches all six archives plus
`SHA256SUMS` to that release. It can also be run manually from the Actions tab;
provide the tag of the release to create or update.
