# Repository Guidelines

## Project Structure & Module Organization

`src/main.rs` contains the complete Rust CLI: argument definitions, benchmark
kernels, platform inspection, output formatting, and JSON result types.
`docs/BENCHMARKS.md` explains benchmark semantics and scoring; update it when
user-visible measurements or flags change. `Cargo.toml` declares the Rust 2024
package and dependencies, while `Cargo.lock` pins reproducible builds. Release
packaging is defined in `.github/workflows/release-binaries.yml`.

## Build, Test, and Development Commands

- `cargo build` compiles a fast local debug binary.
- `cargo build --release` produces the optimized `target/release/nsysbench`.
- `cargo run -- --help` checks command-line parsing without a long benchmark.
- `cargo fmt --check` verifies Rust formatting; run `cargo fmt` before commit.
- `cargo clippy --all-targets -- -D warnings` catches common Rust mistakes.
- `cargo test` runs the test suite. The repository currently has no dedicated
  test module, so add focused unit tests alongside new deterministic logic.

Avoid using lengthy benchmarks as routine validation. For a short manual smoke
test, use `cargo run -- --quiet cpu --threads 1 --duration 1`. IO benchmarks
write temporary test files to the selected `--path`; use `/tmp` rather than the
repository directory. Network benchmarks require an explicit, reachable URL.

## Coding Style & Naming Conventions

Use standard `rustfmt` output (four-space indentation). Follow existing Rust
conventions: `PascalCase` for types and enums, `snake_case` for functions,
fields, and modules, and descriptive clap argument structs such as `CpuArgs`.
Keep CLI output stable: normal results go to stdout, progress to stderr, and
`--json` must remain machine-readable. Prefer typed structs with `Serialize`
over manually assembled JSON.

## Testing Guidelines

Test pure scoring, parsing, and topology helpers with small deterministic
fixtures; avoid assertions tied to the host CPU, clock speed, disk, or network.
Name tests after behavior, for example `score_uses_all_workload_components`.
Run formatting, Clippy, and `cargo test` before opening a pull request.

## Commit & Pull Request Guidelines

Recent history uses concise imperative messages, often Conventional Commit
style: `feat(cpu): implement topology-aware CPU score v2` or
`chore(ci): fix target triples`. Keep commits scoped. PRs should summarize
behavioral changes, note benchmark/JSON compatibility effects, link relevant
issues, and include sample terminal or JSON output when presentation changes.
