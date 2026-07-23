# nsysbench

`nsysbench` is a command-based Rust benchmarking tool to compare machine raw performance.

## Features

- Colorful, UTF-8 friendly terminal output
- CPU raw speed benchmark (prime throughput with configurable threads: 1, 2, N)
- Memory raw speed benchmark (sequential/random read and write)
- Storage IO raw speed benchmark (sequential/random read and write with IOPS + throughput)
- Network raw speed benchmark to a target URL
- JSON output for machine consumption
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

Run only CPU benchmark:

```bash
cargo run -- cpu --threads 1 --duration 5
cargo run -- cpu --threads 2 --duration 5
cargo run -- cpu --threads 8 --duration 5
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

## Cross compilation

Use Rust target triples to compile for major OS/architectures, e.g.:

```bash
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-pc-windows-gnu aarch64-apple-darwin
cargo build --release --target x86_64-unknown-linux-gnu
```
