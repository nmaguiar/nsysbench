use clap::{Args, Parser, Subcommand};
use colored::Colorize;
use reqwest::blocking::Client;
use serde::Serialize;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::hint::black_box;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(
    name = "nsysbench",
    version,
    about = "A colorful system benchmark for CPU, memory, disk IO and network"
)]
struct Cli {
    /// Output as JSON for machine processing
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Benchmark CPU raw speed using prime throughput
    Cpu(CpuArgs),
    /// Benchmark memory read/write raw speed
    Memory(MemoryArgs),
    /// Benchmark storage IO raw speed on a path/mountpoint
    Io(IoArgs),
    /// Benchmark network raw speed to a target URL
    Network(NetworkArgs),
    /// Run a suite of benchmarks and aggregate score
    Run(RunArgs),
}

#[derive(Args, Debug, Clone)]
struct CpuArgs {
    /// Number of worker threads (1, 2, or any custom number)
    #[arg(short, long, default_value_t = 1)]
    threads: usize,
    /// Duration in seconds
    #[arg(short, long, default_value_t = 5)]
    duration: u64,
}

#[derive(Args, Debug, Clone)]
struct MemoryArgs {
    /// Memory buffer size in MB
    #[arg(short = 'm', long, default_value_t = 128)]
    size_mb: usize,
    /// Duration in seconds
    #[arg(short, long, default_value_t = 6)]
    duration: u64,
}

#[derive(Args, Debug, Clone)]
struct IoArgs {
    /// Mount point or path for IO benchmarking
    #[arg(short, long)]
    path: PathBuf,
    /// Block size in KB
    #[arg(short = 'b', long, default_value_t = 4)]
    block_kb: usize,
    /// Test file size in MB
    #[arg(short = 's', long, default_value_t = 64)]
    file_size_mb: usize,
    /// Duration in seconds
    #[arg(short, long, default_value_t = 8)]
    duration: u64,
}

#[derive(Args, Debug, Clone)]
struct NetworkArgs {
    /// Target URL to benchmark against
    #[arg(short, long)]
    target: String,
    /// Duration in seconds
    #[arg(short, long, default_value_t = 8)]
    duration: u64,
}

#[derive(Args, Debug, Clone)]
struct RunArgs {
    /// Number of CPU threads
    #[arg(short, long)]
    threads: Option<usize>,
    /// Duration per benchmark category (seconds)
    #[arg(short, long, default_value_t = 5)]
    duration: u64,
    /// Memory size in MB
    #[arg(long, default_value_t = 128)]
    memory_mb: usize,
    /// IO path/mountpoint
    #[arg(long, default_value = ".")]
    io_path: PathBuf,
    /// Optional network target URL (when omitted, network benchmark is skipped)
    #[arg(long)]
    target: Option<String>,
}

impl Default for RunArgs {
    fn default() -> Self {
        Self {
            threads: Some(1),
            duration: 5,
            memory_mb: 128,
            io_path: PathBuf::from("."),
            target: None,
        }
    }
}

#[derive(Debug, Serialize)]
struct CpuResult {
    threads: usize,
    duration_secs: f64,
    primes_found: u64,
    throughput_primes_per_sec: f64,
    score: f64,
}

#[derive(Debug, Serialize)]
struct MemoryResult {
    size_mb: usize,
    seq_write_mb_s: f64,
    seq_read_mb_s: f64,
    rand_write_mb_s: f64,
    rand_read_mb_s: f64,
    score: f64,
}

#[derive(Debug, Serialize)]
struct IoResult {
    path: String,
    block_kb: usize,
    file_size_mb: usize,
    seq_write_mb_s: f64,
    seq_write_iops: f64,
    seq_read_mb_s: f64,
    seq_read_iops: f64,
    rand_write_mb_s: f64,
    rand_write_iops: f64,
    rand_read_mb_s: f64,
    rand_read_iops: f64,
    score: f64,
}

#[derive(Debug, Serialize)]
struct NetworkResult {
    target: String,
    duration_secs: f64,
    requests: u64,
    bytes: u64,
    throughput_mb_s: f64,
    requests_per_sec: f64,
    avg_latency_ms: f64,
    score: f64,
}

#[derive(Debug, Serialize)]
struct HostInfo {
    os: String,
    arch: String,
    logical_cpus: usize,
    physical_cores_hint: usize,
}

#[derive(Debug, Serialize)]
struct SuiteResult {
    host: HostInfo,
    cpu: Option<CpuResult>,
    memory: Option<MemoryResult>,
    io: Option<IoResult>,
    network: Option<NetworkResult>,
    total_score: f64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    let result = match cli.command {
        Some(Command::Cpu(args)) => {
            let cpu = bench_cpu(&args);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&cpu)?);
            } else {
                print_cpu(&cpu);
            }
            return Ok(());
        }
        Some(Command::Memory(args)) => {
            let mem = bench_memory(&args);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&mem)?);
            } else {
                print_memory(&mem);
            }
            return Ok(());
        }
        Some(Command::Io(args)) => {
            let io = bench_io(&args)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&io)?);
            } else {
                print_io(&io);
            }
            return Ok(());
        }
        Some(Command::Network(args)) => {
            let network = bench_network(&args)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&network)?);
            } else {
                print_network(&network);
            }
            return Ok(());
        }
        Some(Command::Run(args)) => run_suite(args)?,
        None => run_suite(RunArgs::default())?,
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print_suite(&result);
    }

    Ok(())
}

fn run_suite(args: RunArgs) -> Result<SuiteResult, Box<dyn Error>> {
    let threads = args.threads.unwrap_or(1);
    let cpu = Some(bench_cpu(&CpuArgs {
        threads,
        duration: args.duration,
    }));

    let memory = Some(bench_memory(&MemoryArgs {
        size_mb: args.memory_mb,
        duration: args.duration.max(4),
    }));

    let io = Some(bench_io(&IoArgs {
        path: args.io_path,
        block_kb: 4,
        file_size_mb: 64,
        duration: args.duration.max(4),
    })?);

    let network = if let Some(target) = args.target {
        Some(bench_network(&NetworkArgs {
            target,
            duration: args.duration,
        })?)
    } else {
        None
    };

    let total_score = total_score(&[
        cpu.as_ref().map(|r| r.score),
        memory.as_ref().map(|r| r.score),
        io.as_ref().map(|r| r.score),
        network.as_ref().map(|r| r.score),
    ]);

    Ok(SuiteResult {
        host: host_info(),
        cpu,
        memory,
        io,
        network,
        total_score,
    })
}

fn host_info() -> HostInfo {
    let logical = thread::available_parallelism().map_or(1, usize::from);
    HostInfo {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        logical_cpus: logical,
        physical_cores_hint: logical.max(1) / 2,
    }
}

fn bench_cpu(args: &CpuArgs) -> CpuResult {
    let threads = args.threads.max(1);
    let stop = Arc::new(AtomicBool::new(false));
    let total = Arc::new(AtomicU64::new(0));
    let start = Instant::now();

    let mut handles = Vec::with_capacity(threads);
    for worker in 0..threads {
        let stop = Arc::clone(&stop);
        let total = Arc::clone(&total);
        handles.push(thread::spawn(move || {
            let mut local = 0u64;
            let mut n = 3 + (worker as u64 * 2);
            let step = (threads as u64) * 2;
            while !stop.load(Ordering::Relaxed) {
                if is_prime(n) {
                    local += 1;
                }
                n = n.saturating_add(step);
            }
            total.fetch_add(local, Ordering::Relaxed);
        }));
    }

    thread::sleep(Duration::from_secs(args.duration.max(1)));
    stop.store(true, Ordering::Relaxed);

    for handle in handles {
        let _ = handle.join();
    }

    let elapsed = start.elapsed().as_secs_f64();
    let primes_found = total.load(Ordering::Relaxed);
    let throughput = primes_found as f64 / elapsed;

    CpuResult {
        threads,
        duration_secs: elapsed,
        primes_found,
        throughput_primes_per_sec: throughput,
        score: cpu_score(throughput),
    }
}

fn bench_memory(args: &MemoryArgs) -> MemoryResult {
    let size_mb = args.size_mb.max(1);
    let elements = (size_mb * 1024 * 1024 / 8).max(1);
    let mut data = vec![0u64; elements];
    let phase_secs = (args.duration.max(4) / 4).max(1);
    let phase = Duration::from_secs(phase_secs);

    let seq_write_bytes = run_for(phase, || {
        let mut bytes = 0u64;
        for (i, cell) in data.iter_mut().enumerate() {
            *cell = i as u64;
            bytes += 8;
        }
        bytes
    });

    let seq_read_bytes = run_for(phase, || {
        let mut bytes = 0u64;
        let mut checksum = 0u64;
        for value in &data {
            checksum = checksum.wrapping_add(*value);
            bytes += 8;
        }
        black_box(checksum);
        bytes
    });

    let mut rng = 0x9E3779B97F4A7C15u64;
    let rand_write_bytes = run_for(phase, || {
        let mut bytes = 0u64;
        for _ in 0..elements {
            rng = lcg_next(rng);
            let idx = (rng as usize) % elements;
            data[idx] = rng;
            bytes += 8;
        }
        bytes
    });

    let rand_read_bytes = run_for(phase, || {
        let mut bytes = 0u64;
        let mut checksum = 0u64;
        for _ in 0..elements {
            rng = lcg_next(rng);
            let idx = (rng as usize) % elements;
            checksum = checksum.wrapping_add(data[idx]);
            bytes += 8;
        }
        black_box(checksum);
        bytes
    });

    let seq_write_mb_s = bytes_to_mb_s(seq_write_bytes, phase.as_secs_f64());
    let seq_read_mb_s = bytes_to_mb_s(seq_read_bytes, phase.as_secs_f64());
    let rand_write_mb_s = bytes_to_mb_s(rand_write_bytes, phase.as_secs_f64());
    let rand_read_mb_s = bytes_to_mb_s(rand_read_bytes, phase.as_secs_f64());

    MemoryResult {
        size_mb,
        seq_write_mb_s,
        seq_read_mb_s,
        rand_write_mb_s,
        rand_read_mb_s,
        score: memory_score(
            seq_write_mb_s,
            seq_read_mb_s,
            rand_write_mb_s,
            rand_read_mb_s,
        ),
    }
}

fn bench_io(args: &IoArgs) -> Result<IoResult, Box<dyn Error>> {
    fs::create_dir_all(&args.path)?;

    let file_path = args
        .path
        .join(format!("nsysbench-io-{}.dat", std::process::id()));
    let phase_secs = (args.duration.max(4) / 4).max(1);

    let (seq_write_mb_s, seq_write_iops) = run_io_phase(
        &file_path,
        args.file_size_mb,
        args.block_kb,
        phase_secs,
        true,
        false,
    )?;

    let (seq_read_mb_s, seq_read_iops) = run_io_phase(
        &file_path,
        args.file_size_mb,
        args.block_kb,
        phase_secs,
        false,
        false,
    )?;

    let (rand_write_mb_s, rand_write_iops) = run_io_phase(
        &file_path,
        args.file_size_mb,
        args.block_kb,
        phase_secs,
        true,
        true,
    )?;

    let (rand_read_mb_s, rand_read_iops) = run_io_phase(
        &file_path,
        args.file_size_mb,
        args.block_kb,
        phase_secs,
        false,
        true,
    )?;

    let _ = fs::remove_file(&file_path);

    Ok(IoResult {
        path: args.path.display().to_string(),
        block_kb: args.block_kb,
        file_size_mb: args.file_size_mb,
        seq_write_mb_s,
        seq_write_iops,
        seq_read_mb_s,
        seq_read_iops,
        rand_write_mb_s,
        rand_write_iops,
        rand_read_mb_s,
        rand_read_iops,
        score: io_score(
            seq_write_mb_s,
            seq_read_mb_s,
            rand_write_mb_s,
            rand_read_mb_s,
            seq_write_iops,
            seq_read_iops,
            rand_write_iops,
            rand_read_iops,
        ),
    })
}

fn run_io_phase(
    file_path: &Path,
    file_size_mb: usize,
    block_kb: usize,
    seconds: u64,
    write_mode: bool,
    random_mode: bool,
) -> Result<(f64, f64), Box<dyn Error>> {
    let block_size = (block_kb.max(1) * 1024) as u64;
    let file_size = (file_size_mb.max(8) * 1024 * 1024) as u64;
    let blocks = (file_size / block_size).max(1);

    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(file_path)?;
    file.set_len(file_size)?;

    let start = Instant::now();
    let end = start + Duration::from_secs(seconds.max(1));

    let mut offset_blocks = 0u64;
    let mut rng = 0xD1B54A32D192ED03u64;
    let mut bytes = 0u64;
    let mut ops = 0u64;
    let mut buffer = vec![0u8; block_size as usize];

    while Instant::now() < end {
        let block_idx = if random_mode {
            rng = lcg_next(rng);
            rng % blocks
        } else {
            let idx = offset_blocks;
            offset_blocks = (offset_blocks + 1) % blocks;
            idx
        };

        let offset = block_idx * block_size;
        file.seek(SeekFrom::Start(offset))?;

        if write_mode {
            for (i, byte) in buffer.iter_mut().enumerate() {
                *byte = ((block_idx as usize + i) & 0xFF) as u8;
            }
            file.write_all(&buffer)?;
        } else {
            file.read_exact(&mut buffer)?;
            black_box(buffer[0]);
        }

        bytes += block_size;
        ops += 1;
    }

    if write_mode {
        file.flush()?;
    }

    let elapsed = start.elapsed().as_secs_f64();
    let mb_s = bytes_to_mb_s(bytes, elapsed);
    let iops = ops as f64 / elapsed;
    Ok((mb_s, iops))
}

fn bench_network(args: &NetworkArgs) -> Result<NetworkResult, Box<dyn Error>> {
    let client = Client::builder().timeout(Duration::from_secs(15)).build()?;
    let start = Instant::now();
    let end = start + Duration::from_secs(args.duration.max(1));

    let mut requests = 0u64;
    let mut bytes = 0u64;
    let mut total_latency = 0f64;

    while Instant::now() < end {
        let req_start = Instant::now();
        let mut response = client.get(&args.target).send()?;
        let mut sink = std::io::sink();
        let copied = std::io::copy(&mut response, &mut sink)?;
        bytes += copied;
        requests += 1;
        total_latency += req_start.elapsed().as_secs_f64();
    }

    if requests == 0 {
        return Err("no network requests completed".into());
    }

    let elapsed = start.elapsed().as_secs_f64();
    let throughput_mb_s = bytes_to_mb_s(bytes, elapsed);
    let requests_per_sec = requests as f64 / elapsed;
    let avg_latency_ms = total_latency * 1000.0 / requests as f64;

    Ok(NetworkResult {
        target: args.target.clone(),
        duration_secs: elapsed,
        requests,
        bytes,
        throughput_mb_s,
        requests_per_sec,
        avg_latency_ms,
        score: network_score(throughput_mb_s, requests_per_sec),
    })
}

fn bytes_to_mb_s(bytes: u64, seconds: f64) -> f64 {
    if seconds <= 0.0 {
        return 0.0;
    }
    (bytes as f64 / (1024.0 * 1024.0)) / seconds
}

fn run_for<F>(duration: Duration, mut workload: F) -> u64
where
    F: FnMut() -> u64,
{
    let start = Instant::now();
    let end = start + duration;
    let mut bytes = 0u64;
    while Instant::now() < end {
        bytes = bytes.saturating_add(workload());
    }
    bytes
}

fn lcg_next(state: u64) -> u64 {
    state.wrapping_mul(6364136223846793005).wrapping_add(1)
}

fn is_prime(n: u64) -> bool {
    if n < 2 {
        return false;
    }
    if n == 2 {
        return true;
    }
    if n.is_multiple_of(2) {
        return false;
    }

    let mut d = 3u64;
    while d.saturating_mul(d) <= n {
        if n.is_multiple_of(d) {
            return false;
        }
        d += 2;
    }
    true
}

fn cpu_score(throughput: f64) -> f64 {
    throughput / 1000.0
}

fn memory_score(seq_w: f64, seq_r: f64, rand_w: f64, rand_r: f64) -> f64 {
    (seq_w + seq_r + rand_w + rand_r) / 4.0 / 100.0
}

fn io_score(
    seq_w: f64,
    seq_r: f64,
    rand_w: f64,
    rand_r: f64,
    seq_w_iops: f64,
    seq_r_iops: f64,
    rand_w_iops: f64,
    rand_r_iops: f64,
) -> f64 {
    let throughput_component = (seq_w + seq_r + rand_w + rand_r) / 4.0 / 50.0;
    let iops_component = (seq_w_iops + seq_r_iops + rand_w_iops + rand_r_iops) / 4.0 / 1000.0;
    throughput_component + iops_component
}

fn network_score(throughput_mb_s: f64, requests_per_sec: f64) -> f64 {
    (throughput_mb_s / 20.0) + (requests_per_sec / 50.0)
}

fn total_score(scores: &[Option<f64>]) -> f64 {
    scores.iter().flatten().sum()
}

fn print_suite(result: &SuiteResult) {
    println!(
        "\n{}",
        "╔══════════════════════════════════════════════════════════════╗"
            .bright_blue()
            .bold()
    );
    println!(
        "{}",
        "║                ⚡ nsysbench performance report ⚡            ║"
            .bright_blue()
            .bold()
    );
    println!(
        "{}",
        "╚══════════════════════════════════════════════════════════════╝"
            .bright_blue()
            .bold()
    );

    println!(
        "{} {}  {} {}  {} {}",
        "🖥️".bright_cyan(),
        format!("OS: {}", result.host.os).white(),
        "🧬".bright_cyan(),
        format!("ARCH: {}", result.host.arch).white(),
        "🧵".bright_cyan(),
        format!("LOGICAL CPU: {}", result.host.logical_cpus).white()
    );

    if let Some(cpu) = &result.cpu {
        print_section(
            "CPU",
            cpu.score,
            &[("Prime/s", cpu.throughput_primes_per_sec)],
        );
    }

    if let Some(mem) = &result.memory {
        print_section(
            "MEMORY",
            mem.score,
            &[
                ("Seq Write MB/s", mem.seq_write_mb_s),
                ("Seq Read MB/s", mem.seq_read_mb_s),
                ("Rand Write MB/s", mem.rand_write_mb_s),
                ("Rand Read MB/s", mem.rand_read_mb_s),
            ],
        );
    }

    if let Some(io) = &result.io {
        print_section(
            "IO",
            io.score,
            &[
                ("Seq Write MB/s", io.seq_write_mb_s),
                ("Seq Read MB/s", io.seq_read_mb_s),
                ("Rand Write MB/s", io.rand_write_mb_s),
                ("Rand Read MB/s", io.rand_read_mb_s),
                ("Seq Write IOPS", io.seq_write_iops),
                ("Seq Read IOPS", io.seq_read_iops),
                ("Rand Write IOPS", io.rand_write_iops),
                ("Rand Read IOPS", io.rand_read_iops),
            ],
        );
    }

    if let Some(net) = &result.network {
        print_section(
            "NETWORK",
            net.score,
            &[
                ("Throughput MB/s", net.throughput_mb_s),
                ("Req/s", net.requests_per_sec),
                ("Latency ms", net.avg_latency_ms),
            ],
        );
    }

    println!(
        "\n{} {}",
        "🏁 TOTAL SCORE:".bright_magenta().bold(),
        format!("{:.2}", result.total_score).bright_white().bold()
    );
}

fn print_section(name: &str, score: f64, metrics: &[(&str, f64)]) {
    println!(
        "\n{} {}  {} {}",
        "▶".bright_green().bold(),
        name.bright_green().bold(),
        "Score".bright_yellow(),
        format!("{:.2}", score).bright_white().bold()
    );

    for (label, value) in metrics {
        println!("  {:<18} {:>12.2}", label.blue(), value);
    }
}

fn print_cpu(cpu: &CpuResult) {
    println!("{}", "⚙️ CPU benchmark".bright_green().bold());
    print_section(
        "CPU",
        cpu.score,
        &[("Prime/s", cpu.throughput_primes_per_sec)],
    );
}

fn print_memory(mem: &MemoryResult) {
    println!("{}", "🧠 Memory benchmark".bright_green().bold());
    print_section(
        "MEMORY",
        mem.score,
        &[
            ("Seq Write MB/s", mem.seq_write_mb_s),
            ("Seq Read MB/s", mem.seq_read_mb_s),
            ("Rand Write MB/s", mem.rand_write_mb_s),
            ("Rand Read MB/s", mem.rand_read_mb_s),
        ],
    );
}

fn print_io(io: &IoResult) {
    println!("{}", "💽 IO benchmark".bright_green().bold());
    print_section(
        "IO",
        io.score,
        &[
            ("Seq Write MB/s", io.seq_write_mb_s),
            ("Seq Read MB/s", io.seq_read_mb_s),
            ("Rand Write MB/s", io.rand_write_mb_s),
            ("Rand Read MB/s", io.rand_read_mb_s),
            ("Seq Write IOPS", io.seq_write_iops),
            ("Seq Read IOPS", io.seq_read_iops),
            ("Rand Write IOPS", io.rand_write_iops),
            ("Rand Read IOPS", io.rand_read_iops),
        ],
    );
}

fn print_network(net: &NetworkResult) {
    println!("{}", "🌐 Network benchmark".bright_green().bold());
    print_section(
        "NETWORK",
        net.score,
        &[
            ("Throughput MB/s", net.throughput_mb_s),
            ("Req/s", net.requests_per_sec),
            ("Latency ms", net.avg_latency_ms),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prime_detection_is_correct_for_small_values() {
        assert!(!is_prime(1));
        assert!(is_prime(2));
        assert!(is_prime(3));
        assert!(!is_prime(4));
        assert!(is_prime(29));
        assert!(!is_prime(35));
    }

    #[test]
    fn total_score_sums_available_categories() {
        let total = total_score(&[Some(1.0), None, Some(2.5), Some(3.5)]);
        assert!((total - 7.0).abs() < f64::EPSILON);
    }

    #[test]
    fn memory_score_is_average_based() {
        let score = memory_score(100.0, 200.0, 300.0, 400.0);
        assert!((score - 2.5).abs() < 1e-9);
    }
}
