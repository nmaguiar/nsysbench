use clap::{Args, Parser, Subcommand};
use colored::Colorize;
use reqwest::blocking::Client;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::hint::black_box;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
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

    /// Suppress progress messages written to stderr
    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Benchmark CPU raw processing performance and topology scaling
    Cpu(CpuArgs),
    /// Benchmark memory read/write raw speed
    Memory(MemoryArgs),
    /// Benchmark storage IO raw speed on a path/mountpoint
    Io(IoArgs),
    /// Benchmark network raw speed to a target URL
    Network(NetworkArgs),
    /// Show hardware and storage metadata useful for comparing benchmark results
    Info(InfoArgs),
    /// Run a suite of benchmarks and aggregate score
    Run(RunArgs),
}

#[derive(Args, Debug, Clone)]
struct CpuArgs {
    /// Worker-thread limit (0 uses every processor available to this process)
    #[arg(short, long, default_value_t = 0)]
    threads: usize,
    /// Measured seconds per topology stage
    #[arg(short, long, default_value_t = 8)]
    duration: u64,
    /// Run every thread count from 1 through --threads instead of topology checkpoints
    #[arg(long)]
    sequence: bool,
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
struct InfoArgs {
    /// Path or mount point whose storage metadata should be shown
    #[arg(short, long, default_value = ".")]
    path: PathBuf,
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
            threads: Some(0),
            duration: 5,
            memory_mb: 128,
            io_path: PathBuf::from("."),
            target: None,
        }
    }
}

#[derive(Debug, Serialize)]
struct CpuResult {
    score_version: u8,
    requested_threads: usize,
    topology: CpuTopology,
    capabilities: CpuCapabilities,
    stages: Vec<CpuStageResult>,
    single_thread_score: f64,
    multi_thread_score: f64,
    smt_gain_percent: Option<f64>,
    performance_efficiency_ratio: Option<f64>,
    score: f64,
}

#[derive(Debug, Serialize)]
struct CpuSequenceResult {
    score_version: u8,
    duration_secs_per_stage: u64,
    topology: CpuTopology,
    capabilities: CpuCapabilities,
    results: Vec<CpuStageResult>,
}

#[derive(Debug, Clone, Serialize)]
struct CpuTopology {
    logical_cpus: Vec<usize>,
    physical_cores: usize,
    smt_threads_per_core: usize,
    core_classes: Vec<CpuCoreClass>,
    source: String,
    #[serde(skip)]
    core_groups: Vec<Vec<usize>>,
}

#[derive(Debug, Clone, Serialize)]
struct CpuCoreClass {
    id: String,
    logical_cpus: Vec<usize>,
    physical_cores: usize,
}

#[derive(Debug, Clone, Serialize)]
struct CpuCapabilities {
    placement: String,
    topology_detail: bool,
    simd_path: String,
}

#[derive(Debug, Serialize)]
struct CpuStageResult {
    id: String,
    threads: usize,
    cpu_class: Option<String>,
    placement: String,
    logical_cpus: Vec<usize>,
    workloads: Vec<CpuWorkloadResult>,
    composite_gops: f64,
    score: f64,
    scaling_factor: f64,
    parallel_efficiency_percent: f64,
    stability_warning: Option<String>,
}

#[derive(Debug, Serialize)]
struct CpuWorkloadResult {
    name: String,
    operations_per_sec: f64,
    min_operations_per_sec: f64,
    max_operations_per_sec: f64,
    coefficient_of_variation_percent: f64,
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
struct CpuInfo {
    logical_cpus: usize,
    physical_cores: Option<usize>,
    details: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct MemoryInfo {
    total_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
struct IoInfo {
    path: String,
    filesystem: Option<String>,
    total_bytes: Option<u64>,
    available_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SystemInfo {
    cpu: CpuInfo,
    memory: MemoryInfo,
    io: IoInfo,
}

#[derive(Debug, Serialize)]
struct SuiteResult {
    cpu: Option<CpuResult>,
    memory: Option<MemoryResult>,
    io: Option<IoResult>,
    network: Option<NetworkResult>,
    total_score: f64,
}

struct Reporter {
    enabled: bool,
}

impl Reporter {
    fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    fn status(&self, message: impl std::fmt::Display) {
        if self.enabled {
            eprintln!(
                "{}",
                format!("nsysbench: {message}").bright_black().italic()
            );
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let reporter = Reporter::new(!cli.json && !cli.quiet);

    let result = match cli.command {
        Some(Command::Cpu(args)) => {
            if args.sequence {
                let sequence = bench_cpu_sequence(&args, &reporter);
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&sequence)?);
                } else {
                    print_report_separator();
                    print_cpu_sequence(&sequence);
                }
            } else {
                let cpu = bench_cpu(&args, &reporter);
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&cpu)?);
                } else {
                    print_report_separator();
                    print_cpu(&cpu);
                }
            }
            return Ok(());
        }
        Some(Command::Memory(args)) => {
            let mem = bench_memory(&args, &reporter);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&mem)?);
            } else {
                print_report_separator();
                print_memory(&mem);
            }
            return Ok(());
        }
        Some(Command::Io(args)) => {
            let io = bench_io(&args, &reporter)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&io)?);
            } else {
                print_report_separator();
                print_io(&io);
            }
            return Ok(());
        }
        Some(Command::Network(args)) => {
            let network = bench_network(&args, &reporter)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&network)?);
            } else {
                print_report_separator();
                print_network(&network);
            }
            return Ok(());
        }
        Some(Command::Info(args)) => {
            reporter.status(format!(
                "collecting system information for {}",
                args.path.display()
            ));
            let info = system_info(&args.path);
            reporter.status("system information collected");
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                print_report_separator();
                print_system_info(&info);
            }
            return Ok(());
        }
        Some(Command::Run(args)) => run_suite(args, &reporter)?,
        None => run_suite(RunArgs::default(), &reporter)?,
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print_report_separator();
        print_suite(&result);
    }

    Ok(())
}

fn run_suite(args: RunArgs, reporter: &Reporter) -> Result<SuiteResult, Box<dyn Error>> {
    reporter.status("starting benchmark suite");
    let threads = args.threads.unwrap_or(1);
    let cpu = Some(bench_cpu(
        &CpuArgs {
            threads,
            duration: args.duration,
            sequence: false,
        },
        reporter,
    ));

    let memory = Some(bench_memory(
        &MemoryArgs {
            size_mb: args.memory_mb,
            duration: args.duration.max(4),
        },
        reporter,
    ));

    let io = Some(bench_io(
        &IoArgs {
            path: args.io_path,
            block_kb: 4,
            file_size_mb: 64,
            duration: args.duration.max(4),
        },
        reporter,
    )?);

    let network = if let Some(target) = args.target {
        Some(bench_network(
            &NetworkArgs {
                target,
                duration: args.duration,
            },
            reporter,
        )?)
    } else {
        reporter.status("skipping network benchmark (no target provided)");
        None
    };

    let total_score = total_score(&[
        cpu.as_ref().map(|r| r.score),
        memory.as_ref().map(|r| r.score),
        io.as_ref().map(|r| r.score),
        network.as_ref().map(|r| r.score),
    ]);

    reporter.status("benchmark suite completed");
    Ok(SuiteResult {
        cpu,
        memory,
        io,
        network,
        total_score,
    })
}

fn system_info(path: &Path) -> SystemInfo {
    SystemInfo {
        cpu: cpu_info(),
        memory: memory_info(),
        io: io_info(path),
    }
}

fn cpu_info() -> CpuInfo {
    let topology = cpu_topology();
    let (reported_physical_cores, mut details) = cpu_details();
    details.insert("topology source".to_string(), topology.source.clone());
    details.insert(
        "core classes".to_string(),
        topology
            .core_classes
            .iter()
            .map(|class| {
                format!(
                    "{}: {} logical / {} physical",
                    class.id,
                    class.logical_cpus.len(),
                    class.physical_cores
                )
            })
            .collect::<Vec<_>>()
            .join(", "),
    );
    CpuInfo {
        logical_cpus: topology.logical_cpus.len(),
        physical_cores: reported_physical_cores.or(Some(topology.physical_cores)),
        details,
    }
}

#[cfg(target_os = "linux")]
fn cpu_details() -> (Option<usize>, BTreeMap<String, String>) {
    let contents = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let mut details: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut physical_cores = BTreeSet::new();

    for processor in contents.split("\n\n") {
        let fields: BTreeMap<_, _> = processor
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
            .collect();
        for key in [
            "model name",
            "vendor_id",
            "cpu family",
            "model",
            "stepping",
            "cpu MHz",
            "cache size",
        ] {
            if let Some(value) = fields.get(key) {
                details
                    .entry(key.to_string())
                    .or_default()
                    .insert(value.clone());
            }
        }
        if let (Some(package), Some(core)) = (fields.get("physical id"), fields.get("core id")) {
            physical_cores.insert(format!("{package}:{core}"));
        }
    }

    (
        (!physical_cores.is_empty()).then_some(physical_cores.len()),
        details
            .into_iter()
            .map(|(key, values)| (key, values.into_iter().collect::<Vec<_>>().join(", ")))
            .collect(),
    )
}

#[cfg(target_os = "macos")]
fn cpu_details() -> (Option<usize>, BTreeMap<String, String>) {
    let output = std::process::Command::new("sysctl").arg("-a").output();
    let mut details = BTreeMap::new();
    let mut physical_cores = None;

    if let Ok(output) = output {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            if matches!(
                key,
                "machdep.cpu.brand_string"
                    | "machdep.cpu.vendor"
                    | "machdep.cpu.family"
                    | "machdep.cpu.model"
                    | "machdep.cpu.stepping"
                    | "hw.cpufrequency"
                    | "hw.tbfrequency"
                    | "hw.cachelinesize"
                    | "hw.l1dcachesize"
                    | "hw.l1icachesize"
                    | "hw.l2cachesize"
                    | "hw.l3cachesize"
                    | "hw.model"
                    | "hw.machine"
                    | "hw.machine_arch"
                    | "hw.physicalcpu"
                    | "hw.logicalcpu"
            ) || key.starts_with("hw.perflevel")
            {
                if key == "hw.physicalcpu" {
                    physical_cores = value.parse().ok();
                }
                details.insert(key.to_string(), value.to_string());
            }
        }
    }

    (physical_cores, details)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn cpu_details() -> (Option<usize>, BTreeMap<String, String>) {
    (None, BTreeMap::new())
}

#[cfg(target_os = "linux")]
fn memory_info() -> MemoryInfo {
    let total_bytes = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                line.strip_prefix("MemTotal:")?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
                    .map(|kilobytes| kilobytes * 1024)
            })
        });
    MemoryInfo { total_bytes }
}

#[cfg(target_os = "macos")]
fn memory_info() -> MemoryInfo {
    let total_bytes = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|value| value.trim().parse().ok());
    MemoryInfo { total_bytes }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn memory_info() -> MemoryInfo {
    MemoryInfo { total_bytes: None }
}

fn io_info(path: &Path) -> IoInfo {
    let path = path.display().to_string();
    let mut info = IoInfo {
        path: path.clone(),
        filesystem: None,
        total_bytes: None,
        available_bytes: None,
    };
    let Ok(output) = std::process::Command::new("df")
        .args(["-kP", &path])
        .output()
    else {
        return info;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(line) = stdout.lines().last() else {
        return info;
    };
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() >= 4 {
        info.filesystem = Some(fields[0].to_string());
        info.total_bytes = fields[1].parse::<u64>().ok().map(|blocks| blocks * 1024);
        info.available_bytes = fields[3].parse::<u64>().ok().map(|blocks| blocks * 1024);
    }
    info
}

fn bench_cpu(args: &CpuArgs, reporter: &Reporter) -> CpuResult {
    let topology = cpu_topology();
    let capabilities = cpu_capabilities(&topology);
    let selected = select_cpus(&topology, args.threads);
    let stages = topology_stages(&topology, &selected);
    reporter.status(format!(
        "running CPU score v2: {} topology stage(s), {}s each",
        stages.len(),
        args.duration.max(1)
    ));

    let mut results = Vec::with_capacity(stages.len());
    for stage in stages {
        reporter.status(format!(
            "running CPU stage {} with {} thread(s)",
            stage.id,
            stage.cpus.len()
        ));
        results.push(run_cpu_stage(&stage, args.duration.max(1), &capabilities));
    }

    let single = results
        .iter()
        .find(|stage| stage.threads == 1)
        .map_or(0.0, |stage| stage.composite_gops);
    for stage in &mut results {
        stage.scaling_factor = if single > 0.0 {
            stage.composite_gops / single
        } else {
            0.0
        };
        stage.parallel_efficiency_percent = if stage.threads > 0 {
            stage.scaling_factor / stage.threads as f64 * 100.0
        } else {
            0.0
        };
    }
    let multi = results
        .iter()
        .find(|stage| stage.id == "all-logical")
        .or_else(|| results.last())
        .map_or(0.0, |stage| stage.composite_gops);
    let physical = results
        .iter()
        .find(|stage| stage.id == "physical-cores")
        .map(|stage| stage.composite_gops);
    let smt_gain_percent = physical
        .filter(|value| *value > 0.0)
        .map(|value| (multi / value - 1.0) * 100.0);
    let performance_efficiency_ratio = class_ratio(&results);
    let score = cpu_score_v2(single, multi);
    reporter.status("CPU score v2 completed");

    CpuResult {
        score_version: 2,
        requested_threads: args.threads,
        topology,
        capabilities,
        stages: results,
        single_thread_score: single * 100.0,
        multi_thread_score: multi * 100.0,
        smt_gain_percent,
        performance_efficiency_ratio,
        score,
    }
}

fn bench_cpu_sequence(args: &CpuArgs, reporter: &Reporter) -> CpuSequenceResult {
    let topology = cpu_topology();
    let capabilities = cpu_capabilities(&topology);
    let selected = select_cpus(&topology, args.threads);
    let duration = args.duration.max(1);
    reporter.status(format!(
        "running full CPU scaling sequence from 1 to {} thread(s), {duration}s each",
        selected.len()
    ));
    let mut results: Vec<_> = (1..=selected.len())
        .map(|count| CpuStage {
            id: format!("threads-{count}"),
            cpu_class: None,
            cpus: selected[..count].to_vec(),
        })
        .map(|stage| run_cpu_stage(&stage, duration, &capabilities))
        .collect();
    let single = results.first().map_or(0.0, |stage| stage.composite_gops);
    for stage in &mut results {
        stage.scaling_factor = if single > 0.0 {
            stage.composite_gops / single
        } else {
            0.0
        };
        stage.parallel_efficiency_percent = if stage.threads > 0 {
            stage.scaling_factor / stage.threads as f64 * 100.0
        } else {
            0.0
        };
    }
    CpuSequenceResult {
        score_version: 2,
        duration_secs_per_stage: duration,
        topology,
        capabilities,
        results,
    }
}

#[derive(Debug, Clone)]
struct CpuStage {
    id: String,
    cpu_class: Option<String>,
    cpus: Vec<usize>,
}

fn cpu_topology() -> CpuTopology {
    #[cfg(target_os = "linux")]
    {
        linux_cpu_topology()
    }
    #[cfg(target_os = "macos")]
    {
        macos_cpu_topology()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        fallback_cpu_topology("scheduler-visible logical CPUs")
    }
}

#[allow(dead_code)]
fn fallback_cpu_topology(source: &str) -> CpuTopology {
    let logical_cpus: Vec<_> =
        (0..thread::available_parallelism().map_or(1, usize::from)).collect();
    CpuTopology {
        physical_cores: logical_cpus.len(),
        smt_threads_per_core: 1,
        core_classes: vec![CpuCoreClass {
            id: "default".to_string(),
            logical_cpus: logical_cpus.clone(),
            physical_cores: logical_cpus.len(),
        }],
        logical_cpus: logical_cpus.clone(),
        source: source.to_string(),
        core_groups: logical_cpus.iter().map(|cpu| vec![*cpu]).collect(),
    }
}

#[cfg(target_os = "linux")]
fn linux_cpu_topology() -> CpuTopology {
    let allowed = linux_allowed_cpus();
    let mut cores: BTreeMap<Vec<usize>, (String, Vec<usize>)> = BTreeMap::new();
    for cpu in &allowed {
        let root = format!("/sys/devices/system/cpu/cpu{cpu}");
        let siblings = fs::read_to_string(format!("{root}/topology/core_cpus_list"))
            .ok()
            .map(|value| parse_cpu_list(&value))
            .filter(|cpus| !cpus.is_empty())
            .unwrap_or_else(|| vec![*cpu]);
        let key: Vec<_> = siblings
            .into_iter()
            .filter(|candidate| allowed.contains(candidate))
            .collect();
        let class = fs::read_to_string(format!("{root}/topology/core_type"))
            .ok()
            .map(|value| linux_core_type_name(value.trim()))
            .or_else(|| {
                fs::read_to_string(format!("{root}/cpu_capacity"))
                    .ok()
                    .map(|v| format!("capacity-{}", v.trim()))
            })
            .unwrap_or_else(|| "default".to_string());
        cores.entry(key.clone()).or_insert((class, key));
    }
    normalize_linux_capacity_classes(&mut cores);
    let core_groups: Vec<_> = cores.values().map(|(_, cpus)| cpus.clone()).collect();
    let physical_cores = core_groups.len().max(1);
    let smt_threads_per_core = allowed.len().div_ceil(physical_cores);
    let mut class_cpus: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut class_cores: BTreeMap<String, usize> = BTreeMap::new();
    for (class, cpus) in cores.values() {
        class_cpus.entry(class.clone()).or_default().extend(cpus);
        *class_cores.entry(class.clone()).or_default() += 1;
    }
    CpuTopology {
        logical_cpus: allowed,
        physical_cores,
        smt_threads_per_core,
        core_classes: class_cpus
            .into_iter()
            .map(|(id, logical_cpus)| CpuCoreClass {
                physical_cores: class_cores[&id],
                id,
                logical_cpus,
            })
            .collect(),
        source: "Linux sysfs topology intersected with sched_getaffinity".to_string(),
        core_groups,
    }
}

#[cfg(target_os = "linux")]
fn linux_core_type_name(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "atom" | "32" | "0x20" => "efficiency".to_string(),
        "core" | "64" | "0x40" => "performance".to_string(),
        other => format!("core-type-{other}"),
    }
}

#[cfg(target_os = "linux")]
fn normalize_linux_capacity_classes(cores: &mut BTreeMap<Vec<usize>, (String, Vec<usize>)>) {
    let mut capacities: Vec<_> = cores
        .values()
        .filter_map(|(class, _)| class.strip_prefix("capacity-")?.parse::<u64>().ok())
        .collect();
    capacities.sort_unstable();
    capacities.dedup();
    if capacities.len() < 2 {
        return;
    }
    let lowest = capacities[0];
    let highest = *capacities.last().unwrap_or(&lowest);
    for (class, _) in cores.values_mut() {
        if let Some(capacity) = class
            .strip_prefix("capacity-")
            .and_then(|value| value.parse::<u64>().ok())
        {
            *class = if capacity == lowest {
                "efficiency".to_string()
            } else if capacity == highest {
                "performance".to_string()
            } else {
                format!("capacity-{capacity}")
            };
        }
    }
}

#[cfg(target_os = "linux")]
fn linux_allowed_cpus() -> Vec<usize> {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) == 0 {
            let cpus: Vec<_> = (0..libc::CPU_SETSIZE as usize)
                .filter(|cpu| libc::CPU_ISSET(*cpu, &set))
                .collect();
            if !cpus.is_empty() {
                return cpus;
            }
        }
    }
    (0..thread::available_parallelism().map_or(1, usize::from)).collect()
}

#[cfg(target_os = "macos")]
fn macos_cpu_topology() -> CpuTopology {
    let logical = sysctl_usize("hw.logicalcpu")
        .unwrap_or_else(|| thread::available_parallelism().map_or(1, usize::from));
    let physical = sysctl_usize("hw.physicalcpu").unwrap_or(logical);
    let performance = sysctl_usize("hw.perflevel0.logicalcpu").unwrap_or(logical);
    let efficiency = sysctl_usize("hw.perflevel1.logicalcpu").unwrap_or(0);
    let logical_cpus: Vec<_> = (0..logical).collect();
    let mut classes = vec![CpuCoreClass {
        id: "performance".to_string(),
        logical_cpus: (0..performance.min(logical)).collect(),
        physical_cores: performance.min(physical),
    }];
    if efficiency > 0 && performance < logical {
        classes.push(CpuCoreClass {
            id: "efficiency".to_string(),
            logical_cpus: (performance..logical).collect(),
            physical_cores: efficiency.min(physical.saturating_sub(performance)),
        });
    }
    CpuTopology {
        logical_cpus,
        physical_cores: physical,
        smt_threads_per_core: logical.div_ceil(physical.max(1)),
        core_classes: classes,
        source: "macOS sysctl perflevel topology; placement is QoS advisory".to_string(),
        core_groups: (0..logical).map(|cpu| vec![cpu]).collect(),
    }
}

#[cfg(target_os = "macos")]
fn sysctl_usize(name: &str) -> Option<usize> {
    std::process::Command::new("sysctl")
        .args(["-n", name])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|value| value.trim().parse().ok())
}

#[allow(dead_code)]
fn parse_cpu_list(value: &str) -> Vec<usize> {
    value
        .trim()
        .split(',')
        .flat_map(|part| {
            let part = part.trim();
            match part.split_once('-') {
                Some((start, end)) => start
                    .parse::<usize>()
                    .ok()
                    .zip(end.parse::<usize>().ok())
                    .map_or_else(Vec::new, |(start, end)| (start..=end).collect()),
                None => part.parse().ok().into_iter().collect(),
            }
        })
        .collect()
}

fn cpu_capabilities(topology: &CpuTopology) -> CpuCapabilities {
    CpuCapabilities {
        placement: placement_name(),
        topology_detail: topology.source.contains("sysfs") || topology.source.contains("sysctl"),
        simd_path: simd_path().to_string(),
    }
}

fn placement_name() -> String {
    #[cfg(target_os = "linux")]
    {
        return "pinned".to_string();
    }
    #[cfg(target_os = "macos")]
    {
        return "qos-advisory".to_string();
    }
    #[allow(unreachable_code)]
    "scheduler".to_string()
}

fn select_cpus(topology: &CpuTopology, requested: usize) -> Vec<usize> {
    let mut cpus = Vec::new();
    for class in &topology.core_classes {
        for cpu in &class.logical_cpus {
            if !cpus.contains(cpu) {
                cpus.push(*cpu);
            }
        }
    }
    if cpus.is_empty() {
        cpus = topology.logical_cpus.clone();
    }
    let limit = if requested == 0 {
        cpus.len()
    } else {
        requested.min(cpus.len()).max(1)
    };
    cpus.truncate(limit);
    cpus
}

fn topology_stages(topology: &CpuTopology, selected: &[usize]) -> Vec<CpuStage> {
    let mut stages = vec![CpuStage {
        id: "single-thread".to_string(),
        cpu_class: None,
        cpus: selected[..1].to_vec(),
    }];
    for class in &topology.core_classes {
        let cpus: Vec<_> = class
            .logical_cpus
            .iter()
            .copied()
            .filter(|cpu| selected.contains(cpu))
            .collect();
        if cpus.len() > 1 && cpus.len() < selected.len() {
            stages.push(CpuStage {
                id: format!("{}-cores", class.id),
                cpu_class: Some(class.id.clone()),
                cpus,
            });
        }
    }
    let physical: Vec<_> = topology
        .core_classes
        .iter()
        .flat_map(|class| class.logical_cpus.iter().copied())
        .filter(|cpu| selected.contains(cpu))
        .collect::<Vec<_>>();
    let mut one_per_core = Vec::new();
    let mut seen = BTreeSet::new();
    for cpu in physical {
        let key = core_key(topology, cpu);
        if seen.insert(key) {
            one_per_core.push(cpu);
        }
    }
    if one_per_core.len() > 1 && one_per_core.len() < selected.len() {
        stages.push(CpuStage {
            id: "physical-cores".to_string(),
            cpu_class: None,
            cpus: one_per_core,
        });
    }
    if selected.len() > 1 {
        stages.push(CpuStage {
            id: "all-logical".to_string(),
            cpu_class: None,
            cpus: selected.to_vec(),
        });
    }
    stages
}

fn core_key(topology: &CpuTopology, cpu: usize) -> usize {
    topology
        .core_groups
        .iter()
        .position(|group| group.contains(&cpu))
        .unwrap_or(cpu)
}

#[derive(Clone, Copy)]
enum CpuWorkload {
    Integer,
    FloatingPoint,
    Simd,
}

impl CpuWorkload {
    const ALL: [Self; 3] = [Self::Integer, Self::FloatingPoint, Self::Simd];

    fn name(self) -> &'static str {
        match self {
            Self::Integer => "scalar-integer",
            Self::FloatingPoint => "scalar-fp",
            Self::Simd => "simd-fp",
        }
    }

    fn run_block(self, seed: u64) -> u64 {
        match self {
            Self::Integer => integer_block(seed),
            Self::FloatingPoint => floating_point_block(seed),
            Self::Simd => simd_block(seed),
        }
    }
}

fn run_cpu_stage(
    stage: &CpuStage,
    duration_secs: u64,
    capabilities: &CpuCapabilities,
) -> CpuStageResult {
    let samples_per_workload = 3usize;
    let sample_duration = Duration::from_secs_f64(
        (duration_secs as f64 / (CpuWorkload::ALL.len() * samples_per_workload) as f64).max(0.05),
    );
    let mut workloads = Vec::new();
    for workload in CpuWorkload::ALL {
        let mut samples = Vec::with_capacity(samples_per_workload);
        let _ = run_parallel_workload(stage, workload, Duration::from_millis(30), capabilities);
        for _ in 0..samples_per_workload {
            samples.push(run_parallel_workload(
                stage,
                workload,
                sample_duration,
                capabilities,
            ));
        }
        samples.sort_by(f64::total_cmp);
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        let variance = samples
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / samples.len() as f64;
        workloads.push(CpuWorkloadResult {
            name: workload.name().to_string(),
            operations_per_sec: samples[samples.len() / 2],
            min_operations_per_sec: samples[0],
            max_operations_per_sec: samples[samples.len() - 1],
            coefficient_of_variation_percent: if mean > 0.0 {
                variance.sqrt() / mean * 100.0
            } else {
                0.0
            },
        });
    }
    let composite_gops = geometric_mean(
        workloads
            .iter()
            .map(|result| result.operations_per_sec / 1e9),
    );
    let max_cv = workloads
        .iter()
        .map(|result| result.coefficient_of_variation_percent)
        .fold(0.0, f64::max);
    let stability_warning = (max_cv > 5.0).then(|| format!("high sample variation ({max_cv:.1}%)"));
    CpuStageResult {
        id: stage.id.clone(),
        threads: stage.cpus.len(),
        cpu_class: stage.cpu_class.clone(),
        placement: capabilities.placement.clone(),
        logical_cpus: stage.cpus.clone(),
        workloads,
        composite_gops,
        score: composite_gops * 100.0,
        scaling_factor: 0.0,
        parallel_efficiency_percent: 0.0,
        stability_warning,
    }
}

fn run_parallel_workload(
    stage: &CpuStage,
    workload: CpuWorkload,
    duration: Duration,
    capabilities: &CpuCapabilities,
) -> f64 {
    let barrier = Arc::new(Barrier::new(stage.cpus.len() + 1));
    let mut handles = Vec::with_capacity(stage.cpus.len());
    for (worker, cpu) in stage.cpus.iter().copied().enumerate() {
        let barrier = Arc::clone(&barrier);
        let class = stage.cpu_class.clone();
        let placement = capabilities.placement.clone();
        handles.push(thread::spawn(move || {
            apply_worker_placement(cpu, class.as_deref(), &placement);
            barrier.wait();
            let end = Instant::now() + duration;
            let mut seed = (worker as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let mut operations = 0u64;
            while Instant::now() < end {
                operations = operations.saturating_add(workload.run_block(seed));
                seed = seed.wrapping_add(0xD1B5_4A32_D192_ED03);
            }
            black_box(seed);
            operations
        }));
    }
    let start = Instant::now();
    barrier.wait();
    let operations = handles
        .into_iter()
        .filter_map(|handle| handle.join().ok())
        .sum::<u64>();
    operations as f64 / start.elapsed().as_secs_f64().max(f64::MIN_POSITIVE)
}

fn apply_worker_placement(cpu: usize, class: Option<&str>, placement: &str) {
    #[cfg(target_os = "linux")]
    if placement == "pinned" {
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_ZERO(&mut set);
            libc::CPU_SET(cpu, &mut set);
            let _ = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
        }
    }
    #[cfg(target_os = "macos")]
    if placement == "qos-advisory" {
        let qos = if class == Some("efficiency") {
            libc::qos_class_t::QOS_CLASS_BACKGROUND
        } else {
            libc::qos_class_t::QOS_CLASS_USER_INITIATED
        };
        unsafe {
            let _ = libc::pthread_set_qos_class_self_np(qos, 0);
        }
    }
    let _ = (cpu, class);
}

fn integer_block(seed: u64) -> u64 {
    let mut a = seed;
    let mut b = seed.rotate_left(17);
    let mut c = !seed;
    let mut d = seed.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    for _ in 0..20_000 {
        a = a.wrapping_mul(0x9E37_79B1).rotate_left(13) ^ b;
        b = b.wrapping_add(0xC2B2_AE3D).rotate_right(11) ^ c;
        c = c.wrapping_mul(0x1656_67B1).rotate_left(7) ^ d;
        d = d.wrapping_add(a).rotate_right(19) ^ b;
    }
    black_box((a, b, c, d));
    20_000 * 12
}

fn floating_point_block(seed: u64) -> u64 {
    let mut a = seed as f64 * 1e-12 + 1.0;
    let mut b = a + 0.25;
    let mut c = a + 0.5;
    let mut d = a + 0.75;
    for _ in 0..20_000 {
        a = a * 1.000_000_1 + b * 0.000_000_1;
        b = b * 0.999_999_9 + c * 0.000_000_2;
        c = c * 1.000_000_2 + d * 0.000_000_1;
        d = d * 0.999_999_8 + a * 0.000_000_3;
    }
    black_box((a, b, c, d));
    20_000 * 12
}

fn simd_block(seed: u64) -> u64 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma")
        {
            return simd_avx2_fma_block(seed);
        }
        if std::arch::is_x86_feature_detected!("sse2") {
            return simd_sse2_block(seed);
        }
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        return simd_neon_block(seed);
    }
    #[allow(unreachable_code)]
    simd_scalar_block(seed)
}

fn simd_scalar_block(seed: u64) -> u64 {
    let mut lanes = [seed as f32 * 1e-6 + 1.0, 1.25, 1.5, 1.75];
    for _ in 0..20_000 {
        for lane in &mut lanes {
            *lane = *lane * 1.000_001 + 0.000_001;
        }
    }
    black_box(lanes);
    20_000 * 8
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn simd_avx2_fma_block(seed: u64) -> u64 {
    use std::arch::x86_64::*;
    let mut value = _mm256_set1_ps(seed as f32 * 1e-6 + 1.0);
    let scale = _mm256_set1_ps(1.000_001);
    let add = _mm256_set1_ps(0.000_001);
    for _ in 0..20_000 {
        value = _mm256_fmadd_ps(value, scale, add);
    }
    black_box(_mm256_cvtss_f32(value));
    20_000 * 16
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn simd_sse2_block(seed: u64) -> u64 {
    use std::arch::x86_64::*;
    let mut value = _mm_set1_ps(seed as f32 * 1e-6 + 1.0);
    let scale = _mm_set1_ps(1.000_001);
    let add = _mm_set1_ps(0.000_001);
    for _ in 0..20_000 {
        value = _mm_add_ps(_mm_mul_ps(value, scale), add);
    }
    black_box(_mm_cvtss_f32(value));
    20_000 * 8
}

#[cfg(target_arch = "aarch64")]
unsafe fn simd_neon_block(seed: u64) -> u64 {
    use std::arch::aarch64::*;
    unsafe {
        let mut value = vdupq_n_f32(seed as f32 * 1e-6 + 1.0);
        let scale = vdupq_n_f32(1.000_001);
        let add = vdupq_n_f32(0.000_001);
        for _ in 0..20_000 {
            value = vfmaq_f32(add, value, scale);
        }
        black_box(vgetq_lane_f32(value, 0));
    }
    20_000 * 8
}

fn simd_path() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma")
        {
            return "avx2-fma";
        }
        if std::arch::is_x86_feature_detected!("sse2") {
            return "sse2";
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return "neon";
    }
    #[allow(unreachable_code)]
    "scalar"
}

fn geometric_mean(values: impl Iterator<Item = f64>) -> f64 {
    let values: Vec<_> = values
        .filter(|value| *value > 0.0 && value.is_finite())
        .collect();
    if values.is_empty() {
        0.0
    } else {
        (values.iter().map(|value| value.ln()).sum::<f64>() / values.len() as f64).exp()
    }
}

fn cpu_score_v2(single_gops: f64, multi_gops: f64) -> f64 {
    if single_gops <= 0.0 || multi_gops <= 0.0 {
        0.0
    } else {
        100.0 * single_gops.powf(0.35) * multi_gops.powf(0.65)
    }
}

fn class_ratio(stages: &[CpuStageResult]) -> Option<f64> {
    let performance = stages
        .iter()
        .find(|stage| stage.cpu_class.as_deref() == Some("performance"))?
        .composite_gops;
    let efficiency = stages
        .iter()
        .find(|stage| stage.cpu_class.as_deref() == Some("efficiency"))?
        .composite_gops;
    (efficiency > 0.0).then_some(performance / efficiency)
}

fn bench_memory(args: &MemoryArgs, reporter: &Reporter) -> MemoryResult {
    let size_mb = args.size_mb.max(1);
    let elements = (size_mb * 1024 * 1024 / 8).max(1);
    let mut data = vec![0u64; elements];
    let phase_secs = (args.duration.max(4) / 4).max(1);
    let phase = Duration::from_secs(phase_secs);

    reporter.status(format!(
        "running memory sequential write phase for {phase_secs}s"
    ));
    let seq_write_bytes = run_for(phase, || {
        let mut bytes = 0u64;
        for (i, cell) in data.iter_mut().enumerate() {
            *cell = i as u64;
            bytes += 8;
        }
        bytes
    });

    reporter.status(format!(
        "running memory sequential read phase for {phase_secs}s"
    ));
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
    reporter.status(format!(
        "running memory random write phase for {phase_secs}s"
    ));
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

    reporter.status(format!(
        "running memory random read phase for {phase_secs}s"
    ));
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

    let result = MemoryResult {
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
    };
    reporter.status("memory benchmark completed");
    result
}

fn bench_io(args: &IoArgs, reporter: &Reporter) -> Result<IoResult, Box<dyn Error>> {
    fs::create_dir_all(&args.path)?;

    let file_path = args
        .path
        .join(format!("nsysbench-io-{}.dat", std::process::id()));
    let phase_secs = (args.duration.max(4) / 4).max(1);
    reporter.status(format!(
        "running IO benchmark in {} using {} KiB blocks and a {} MiB file",
        args.path.display(),
        args.block_kb.max(1),
        args.file_size_mb.max(8)
    ));

    let (seq_write_mb_s, seq_write_iops) = run_io_phase(
        &file_path,
        args.file_size_mb,
        args.block_kb,
        phase_secs,
        true,
        false,
        "sequential write",
        reporter,
    )?;

    let (seq_read_mb_s, seq_read_iops) = run_io_phase(
        &file_path,
        args.file_size_mb,
        args.block_kb,
        phase_secs,
        false,
        false,
        "sequential read",
        reporter,
    )?;

    let (rand_write_mb_s, rand_write_iops) = run_io_phase(
        &file_path,
        args.file_size_mb,
        args.block_kb,
        phase_secs,
        true,
        true,
        "random write",
        reporter,
    )?;

    let (rand_read_mb_s, rand_read_iops) = run_io_phase(
        &file_path,
        args.file_size_mb,
        args.block_kb,
        phase_secs,
        false,
        true,
        "random read",
        reporter,
    )?;

    let _ = fs::remove_file(&file_path);

    let result = IoResult {
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
    };
    reporter.status("IO benchmark completed");
    Ok(result)
}

fn run_io_phase(
    file_path: &Path,
    file_size_mb: usize,
    block_kb: usize,
    seconds: u64,
    write_mode: bool,
    random_mode: bool,
    phase_name: &str,
    reporter: &Reporter,
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
    reporter.status(format!(
        "running IO {phase_name} phase for {}s",
        seconds.max(1)
    ));

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

fn bench_network(args: &NetworkArgs, reporter: &Reporter) -> Result<NetworkResult, Box<dyn Error>> {
    reporter.status(format!(
        "running network benchmark against {} for {}s",
        args.target,
        args.duration.max(1)
    ));
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

    let result = NetworkResult {
        target: args.target.clone(),
        duration_secs: elapsed,
        requests,
        bytes,
        throughput_mb_s,
        requests_per_sec,
        avg_latency_ms,
        score: network_score(throughput_mb_s, requests_per_sec),
    };
    reporter.status("network benchmark completed");
    Ok(result)
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

fn print_report_separator() {
    println!("{}", "┄".repeat(62).bright_black());
}

fn print_suite(result: &SuiteResult) {
    println!(
        "\n{}",
        "╔══════════════════════════════════╗"
            .bright_blue()
            .bold()
    );
    println!(
        "{}",
        "║ ⚡nsysbench performance report ⚡║"
            .bright_blue()
            .bold()
    );
    println!(
        "{}",
        "╚══════════════════════════════════╝"
            .bright_blue()
            .bold()
    );

    if let Some(cpu) = &result.cpu {
        print_section(
            "CPU",
            cpu.score,
            &[
                ("Single-thread score", cpu.single_thread_score),
                ("All-logical score  ", cpu.multi_thread_score),
            ],
        );
    }

    if let Some(mem) = &result.memory {
        print_section(
            "MEMORY",
            mem.score,
            &[    
                ("Seq Write MB/s     ", mem.seq_write_mb_s),
                ("Seq Read MB/s      ", mem.seq_read_mb_s),
                ("Rand Write MB/s    ", mem.rand_write_mb_s),
                ("Rand Read MB/s     ", mem.rand_read_mb_s),
            ],
        );
    }

    if let Some(io) = &result.io {
        print_section(
            "IO",
            io.score,
            &[ 
                ("Seq Write MB/s     ", io.seq_write_mb_s),
                ("Seq Read MB/s      ", io.seq_read_mb_s),
                ("Rand Write MB/s    ", io.rand_write_mb_s),
                ("Rand Read MB/s     ", io.rand_read_mb_s),
                ("Seq Write IOPS     ", io.seq_write_iops),
                ("Seq Read IOPS      ", io.seq_read_iops),
                ("Rand Write IOPS    ", io.rand_write_iops),
                ("Rand Read IOPS     ", io.rand_read_iops),
            ],
        );
    }

    if let Some(net) = &result.network {
        print_section(
            "NETWORK",
            net.score,
            &[   
                ("Throughput MB/s    ", net.throughput_mb_s),
                ("Req/s              ", net.requests_per_sec),
                ("Latency ms         ", net.avg_latency_ms),
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
    println!("{}", "⚙️  CPU benchmark  ".bright_green().bold());
    print_section(
        "CPU",
        cpu.score,
        &[
            ("Single-thread score", cpu.single_thread_score),
            ("All-logical score  ", cpu.multi_thread_score),
        ],
    );
    println!(
        "  Logical CPUs: {} | Physical cores: {} | SMT: {}",
        cpu.topology.logical_cpus.len(),
        cpu.topology.physical_cores,
        cpu.topology.smt_threads_per_core
    );
    println!(
        "  Placement: {} | SIMD: {}",
        cpu.capabilities.placement, cpu.capabilities.simd_path
    );
    for stage in &cpu.stages {
        println!(
            "  {:<20} {:>8.2} GOPS  {:>2} threads",
            stage.id, stage.composite_gops, stage.threads
        );
    }
    if let Some(gain) = cpu.smt_gain_percent {
        println!("  SMT gain: {gain:.1}%");
    }
    if let Some(ratio) = cpu.performance_efficiency_ratio {
        println!("  Performance/efficiency ratio: {ratio:.2}x");
    }
}

fn print_cpu_sequence(sequence: &CpuSequenceResult) {
    println!("{}", "⚙️  CPU scaling benchmark".bright_green().bold());
    println!();
    println!(
        "  {}",
        format!(
            "{} test(s), {}s each",
            sequence.results.len(),
            sequence.duration_secs_per_stage
        )
        .bright_white()
    );

    for result in &sequence.results {
        println!(
            "  {:>2} thread{}  {:>12.2} GOPS",
            result.threads,
            if result.threads == 1 { " " } else { "s" },
            result.composite_gops
        );
    }

    let values: Vec<f64> = sequence
        .results
        .iter()
        .map(|result| result.composite_gops)
        .collect();
    println!("\n  {}", "Composite GOPS".bright_yellow());
    for line in sparkline(&values).lines() {
        println!("  {}", color_chart_line(line));
    }
    println!("  {}", "threads".dimmed());
}

fn print_system_info(info: &SystemInfo) {
    println!("{}", "ℹ️ System performance metadata  ".bright_green().bold());
    println!("\n{}", "CPU".bright_cyan().bold());
    println!(
        "  Logical CPUs: {} | Physical cores: {}",
        info.cpu.logical_cpus,
        info.cpu
            .physical_cores
            .map_or_else(|| "unknown".to_string(), |cores| cores.to_string())
    );
    for (key, value) in &info.cpu.details {
        println!("  {key}: {value}");
    }
    println!("\n{}", "Memory".bright_cyan().bold());
    println!(
        "  Total: {}",
        info.memory
            .total_bytes
            .map_or_else(|| "unknown".to_string(), format_bytes)
    );
    println!("\n{}", "Storage".bright_cyan().bold());
    println!("  Path: {}", info.io.path);
    println!(
        "  Filesystem: {}",
        info.io.filesystem.as_deref().unwrap_or("unknown")
    );
    println!(
        "  Capacity: {} | Available: {}",
        info.io
            .total_bytes
            .map_or_else(|| "unknown".to_string(), format_bytes),
        info.io
            .available_bytes
            .map_or_else(|| "unknown".to_string(), format_bytes)
    );
}

fn format_bytes(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / 1024_f64.powi(3))
}

fn sparkline(values: &[f64]) -> String {
    const HEIGHT: usize = 5;
    const COLUMN_GAP: &str = "┄┄┄";

    let Some(&max) = values
        .iter()
        .filter(|value| value.is_finite())
        .max_by(|a, b| a.total_cmp(b))
    else {
        return String::new();
    };

    let label_width = values.len().max(1).to_string().len().max(1);
    let heights: Vec<usize> = values
        .iter()
        .map(|value| {
            if max <= 0.0 || !value.is_finite() {
                0
            } else {
                ((value / max) * HEIGHT as f64)
                    .round()
                    .clamp(1.0, HEIGHT as f64) as usize
            }
        })
        .collect();

    let mut lines = Vec::with_capacity(HEIGHT + 1);
    for row in (1..=HEIGHT).rev() {
        lines.push(
            heights
                .iter()
                .map(|height| if *height >= row { "█" } else { "┄" }.to_string())
                .collect::<Vec<_>>()
                .join(COLUMN_GAP),
        );
    }
    lines.push(
        (1..=values.len())
            .map(|thread_count| format!("{thread_count:<label_width$}"))
            .collect::<Vec<_>>()
            // Plot columns are one character wide; labels may be wider (e.g. "10").
            // Shorten their separator to retain the same chart-column pitch.
            .join(&" ".repeat(COLUMN_GAP.chars().count().saturating_sub(label_width - 1))),
    );
    lines.join("\n")
}

fn color_chart_line(line: &str) -> String {
    line.chars()
        .map(|character| match character {
            '█' => character.to_string().bright_cyan().to_string(),
            '┄' => character.to_string().bright_black().to_string(),
            _ => character.to_string(),
        })
        .collect()
}

fn print_memory(mem: &MemoryResult) {
    println!("{}", "🧠 Memory benchmark  ".bright_green().bold());
    print_section(
        "MEMORY",
        mem.score,
        &[   
            ("Seq Write MB/s     ", mem.seq_write_mb_s),
            ("Seq Read MB/s      ", mem.seq_read_mb_s),
            ("Rand Write MB/s    ", mem.rand_write_mb_s),
            ("Rand Read MB/s     ", mem.rand_read_mb_s),
        ],
    );
}

fn print_io(io: &IoResult) {
    println!("{}", "💽 IO benchmark  ".bright_green().bold());
    print_section(
        "IO",
        io.score,
        &[ 
            ("Seq Write MB/s     ", io.seq_write_mb_s),
            ("Seq Read MB/s      ", io.seq_read_mb_s),
            ("Rand Write MB/s    ", io.rand_write_mb_s),
            ("Rand Read MB/s     ", io.rand_read_mb_s),
            ("Seq Write IOPS     ", io.seq_write_iops),
            ("Seq Read IOPS      ", io.seq_read_iops),
            ("Rand Write IOPS    ", io.rand_write_iops),
            ("Rand Read IOPS     ", io.rand_read_iops),
        ],
    );
}

fn print_network(net: &NetworkResult) {
    println!("{}", "🌐 Network benchmark  ".bright_green().bold());
    print_section(
        "NETWORK",
        net.score,
        &[ 
            ("Throughput MB/s    ", net.throughput_mb_s),
            ("Req/s              ", net.requests_per_sec),
            ("Latency ms         ", net.avg_latency_ms),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_list_parser_handles_ranges_and_individual_cpus() {
        assert_eq!(parse_cpu_list("0-2, 5, 7-8\n"), vec![0, 1, 2, 5, 7, 8]);
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

    #[test]
    fn sparkline_is_five_rows_with_dotted_grid_and_spaced_thread_labels() {
        let chart = sparkline(&[10.0, 20.0, 30.0]);
        let lines: Vec<_> = chart.lines().collect();
        assert_eq!(lines.len(), 6);
        assert_eq!(lines.last(), Some(&"1   2   3"));
        assert_eq!(lines[0], "┄┄┄┄┄┄┄┄█");
        assert_eq!(lines[4], "█┄┄┄█┄┄┄█");

        let ten_threads = sparkline(&(1..=10).map(f64::from).collect::<Vec<_>>());
        assert!(ten_threads.lines().take(5).all(|line| !line.contains(' ')));
    }

    #[test]
    fn cpu_v2_score_uses_weighted_geometric_mean() {
        let score = cpu_score_v2(4.0, 16.0);
        assert!((score - 984.916).abs() < 0.01);
    }

    #[test]
    fn selected_cpu_limit_is_applied() {
        let topology = fallback_cpu_topology("test");
        assert_eq!(select_cpus(&topology, 1).len(), 1);
        assert_eq!(select_cpus(&topology, 0).len(), topology.logical_cpus.len());
    }

    #[test]
    fn compute_kernels_report_their_fixed_operation_counts() {
        assert_eq!(integer_block(1), 240_000);
        assert_eq!(floating_point_block(1), 240_000);
    }
}
