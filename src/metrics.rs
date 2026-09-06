//! Read-only Linux telemetry. Rates use counter deltas and monotonic elapsed time.
//!
//! Kernel interfaces: https://docs.kernel.org/filesystems/proc.html,
//! https://docs.kernel.org/block/stat.html, and
//! https://docs.kernel.org/hwmon/sysfs-interface.html.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub uptime_secs: f64,
    pub load: [f64; 3],
    pub cpu_percent: Option<f64>,
    pub cores: Vec<Core>,
    pub cpu_model: String,
    pub hostname: String,
    pub kernel: String,
    pub memory: Memory,
    pub disks: Vec<Disk>,
    pub networks: Vec<Network>,
    pub sensors: Vec<Sensor>,
    pub warnings: Vec<String>,
}

impl Snapshot {
    /// Hottest recognized CPU reading, preferring a device's physical die
    /// temperature over its offset fan-control value when both are available.
    pub fn cpu_temperature(&self) -> Option<f64> {
        let physical_devices: BTreeSet<&str> = self
            .sensors
            .iter()
            .filter(|sensor| sensor.is_cpu && sensor.celsius.is_finite())
            .filter_map(|sensor| match &sensor.cpu_temperature_source {
                CpuTemperatureSource::Die { device } => Some(device.as_str()),
                _ => None,
            })
            .collect();
        self.sensors
            .iter()
            .filter(|sensor| sensor.is_cpu && sensor.celsius.is_finite())
            .filter(|sensor| match &sensor.cpu_temperature_source {
                CpuTemperatureSource::Control { device } => {
                    !physical_devices.contains(device.as_str())
                }
                _ => true,
            })
            .map(|sensor| sensor.celsius)
            .reduce(f64::max)
    }

    /// Clock statistics across cores with a valid reading in this sample.
    pub fn cpu_frequency_stats(&self) -> Option<FrequencyStats> {
        let mut frequencies = self
            .cores
            .iter()
            .filter_map(|core| core.frequency_mhz)
            .filter(|frequency| frequency.is_finite() && *frequency > 0.0);
        let first = frequencies.next()?;
        let mut stats = FrequencyStats {
            min_mhz: first,
            average_mhz: first,
            max_mhz: first,
        };
        for (index, frequency) in frequencies.enumerate() {
            stats.min_mhz = stats.min_mhz.min(frequency);
            stats.average_mhz += (frequency - stats.average_mhz) / (index + 2) as f64;
            stats.max_mhz = stats.max_mhz.max(frequency);
        }
        Some(stats)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrequencyStats {
    pub min_mhz: f64,
    pub average_mhz: f64,
    pub max_mhz: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Core {
    pub id: usize,
    pub percent: Option<f64>,
    pub frequency_mhz: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct Memory {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    /// Reclaimable page/slab cache, excluding shared memory and buffers.
    pub cached_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Disk {
    pub name: String,
    pub read_bytes_per_sec: Option<f64>,
    pub write_bytes_per_sec: Option<f64>,
    pub total_read_bytes: u64,
    pub total_write_bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Network {
    pub name: String,
    pub received_bytes_per_sec: Option<f64>,
    pub transmitted_bytes_per_sec: Option<f64>,
    pub total_received_bytes: u64,
    pub total_transmitted_bytes: u64,
    /// Administrative state, with a definite operational state as a fallback.
    /// None means the state is unknown or its source is unavailable.
    pub is_up: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct Sensor {
    pub id: String,
    pub label: String,
    pub celsius: f64,
    pub critical_celsius: Option<f64>,
    pub is_cpu: bool,
    pub(crate) cpu_temperature_source: CpuTemperatureSource,
}

#[derive(Clone, Debug, Default)]
pub(crate) enum CpuTemperatureSource {
    #[default]
    Other,
    Control {
        device: String,
    },
    Die {
        device: String,
    },
}

#[derive(Clone, Copy, Debug)]
struct CpuTimes([u64; 8]);

#[derive(Clone, Copy, Debug)]
struct Counters {
    first: u64,
    second: u64,
}

#[derive(Clone, Copy, Debug)]
struct DiskBaseline {
    diskseq: u64,
    counters: Counters,
}

#[derive(Clone, Copy, Debug)]
struct NetworkBaseline {
    ifindex: u32,
    counters: Counters,
}

#[derive(Debug)]
pub struct Collector {
    proc_root: PathBuf,
    sys_root: PathBuf,
    previous_cpu: BTreeMap<String, CpuTimes>,
    previous_disks: BTreeMap<String, DiskBaseline>,
    previous_networks: BTreeMap<String, NetworkBaseline>,
    previous_disk_time: Option<Instant>,
    previous_network_time: Option<Instant>,
    hostname: String,
    kernel: String,
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector {
    pub fn new() -> Self {
        Self::with_roots(PathBuf::from("/proc"), PathBuf::from("/sys"))
    }

    fn with_roots(proc_root: PathBuf, sys_root: PathBuf) -> Self {
        Self {
            hostname: read_trimmed(proc_root.join("sys/kernel/hostname"))
                .unwrap_or_else(|| "Linux host".into()),
            kernel: read_trimmed(proc_root.join("sys/kernel/osrelease"))
                .unwrap_or_else(|| "Unavailable".into()),
            proc_root,
            sys_root,
            previous_cpu: BTreeMap::new(),
            previous_disks: BTreeMap::new(),
            previous_networks: BTreeMap::new(),
            previous_disk_time: None,
            previous_network_time: None,
        }
    }

    pub fn sample(&mut self) -> Snapshot {
        let mut snapshot = Snapshot {
            uptime_secs: f64::NAN,
            load: [f64::NAN; 3],
            hostname: self.hostname.clone(),
            kernel: self.kernel.clone(),
            ..Snapshot::default()
        };
        if let Some(text) = read_required(&self.proc_root.join("uptime"), &mut snapshot.warnings) {
            if let Some(value) = text.split_whitespace().next().and_then(parse_nonnegative) {
                snapshot.uptime_secs = value;
            } else {
                snapshot
                    .warnings
                    .push("System uptime could not be parsed.".into());
            }
        }
        if let Some(text) = read_required(&self.proc_root.join("loadavg"), &mut snapshot.warnings) {
            for (target, token) in snapshot.load.iter_mut().zip(text.split_whitespace()) {
                *target = parse_nonnegative(token).unwrap_or(f64::NAN);
            }
            if snapshot.load.iter().any(|value| !value.is_finite()) {
                snapshot
                    .warnings
                    .push("Load averages are unavailable or incomplete.".into());
            }
        }

        let cpuinfo = read_required(&self.proc_root.join("cpuinfo"), &mut snapshot.warnings)
            .unwrap_or_default();
        let (model, frequencies) = parse_cpuinfo(&cpuinfo);
        snapshot.cpu_model = model;
        if let Some(text) = read_required(&self.proc_root.join("stat"), &mut snapshot.warnings) {
            let cpu = parse_cpu_times(&text);
            snapshot.cpu_percent = cpu.get("cpu").and_then(|current| {
                self.previous_cpu
                    .get("cpu")
                    .and_then(|previous| cpu_percent(*previous, *current))
            });
            if !cpu.contains_key("cpu") {
                snapshot
                    .warnings
                    .push("CPU utilization counters are unavailable.".into());
            }
            for (name, current) in &cpu {
                let Some(id) = name
                    .strip_prefix("cpu")
                    .and_then(|id| id.parse::<usize>().ok())
                else {
                    continue;
                };
                let percent = self
                    .previous_cpu
                    .get(name)
                    .and_then(|previous| cpu_percent(*previous, *current));
                let frequency_mhz = self
                    .cpu_frequency(id)
                    .or_else(|| frequencies.get(&id).copied());
                snapshot.cores.push(Core {
                    id,
                    percent,
                    frequency_mhz,
                });
            }
            snapshot.cores.sort_by_key(|core| core.id);
            self.previous_cpu = cpu;
        } else {
            self.previous_cpu.clear();
        }
        if !snapshot.cores.is_empty()
            && snapshot
                .cores
                .iter()
                .all(|core| core.frequency_mhz.is_none())
        {
            snapshot
                .warnings
                .push("CPU clock readings are not exposed by this system.".into());
        }

        if let Some(text) = read_required(&self.proc_root.join("meminfo"), &mut snapshot.warnings) {
            if let Some((memory, estimated)) = parse_memory(&text) {
                snapshot.memory = memory;
                if estimated {
                    snapshot.warnings.push(
                        "Available memory is estimated because MemAvailable is not exposed.".into(),
                    );
                }
            } else {
                snapshot
                    .warnings
                    .push("Memory counters are unavailable or invalid.".into());
            }
        }
        snapshot.disks = self.sample_disks(&mut snapshot.warnings);
        snapshot.networks = self.sample_networks(&mut snapshot.warnings);
        snapshot.sensors = discover_sensors(&self.sys_root, &mut snapshot.warnings);
        snapshot
    }

    fn cpu_frequency(&self, id: usize) -> Option<f64> {
        let path = self
            .sys_root
            .join(format!("devices/system/cpu/cpu{id}/cpufreq"));
        ["cpuinfo_cur_freq", "scaling_cur_freq"]
            .into_iter()
            .find_map(|file| {
                read_trimmed(path.join(file))
                    .and_then(|text| parse_positive(&text))
                    .map(|khz| khz / 1000.0)
            })
    }

    fn sample_disks(&mut self, warnings: &mut Vec<String>) -> Vec<Disk> {
        // Device names can be reused between samples. Check generations on
        // both sides of diskstats so replacement during this read cannot seed
        // a new device's baseline with the previous device's counters.
        let identities_before: BTreeMap<_, _> = fs::read_dir(self.sys_root.join("block"))
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                Some((name, disk_sequence(&entry.path())?))
            })
            .collect();
        let Some(text) = read_required(&self.proc_root.join("diskstats"), warnings) else {
            self.previous_disks.clear();
            self.previous_disk_time = None;
            return Vec::new();
        };
        let now = Instant::now();
        let elapsed = self
            .previous_disk_time
            .map(|previous| now.duration_since(previous).as_secs_f64());
        let Some(devices) = leaf_block_devices(&self.sys_root, warnings) else {
            self.previous_disks.clear();
            self.previous_disk_time = None;
            return Vec::new();
        };
        let counters: BTreeMap<_, _> = parse_diskstats(&text)
            .into_iter()
            .filter(|(name, _)| devices.contains(name))
            .collect();
        let mut next_baselines = BTreeMap::new();
        let mut unknown_identities = 0;
        let disks = counters
            .iter()
            .map(|(name, current)| {
                let diskseq = disk_sequence(&self.sys_root.join("block").join(name))
                    .filter(|sequence| identities_before.get(name) == Some(sequence));
                let previous = diskseq.and_then(|sequence| {
                    self.previous_disks
                        .get(name)
                        .filter(|previous| previous.diskseq == sequence)
                        .map(|previous| &previous.counters)
                });
                if let Some(diskseq) = diskseq {
                    next_baselines.insert(
                        name.clone(),
                        DiskBaseline {
                            diskseq,
                            counters: *current,
                        },
                    );
                } else {
                    unknown_identities += 1;
                }
                Disk {
                    name: name.clone(),
                    read_bytes_per_sec: previous
                        .and_then(|previous| counter_rate(previous.first, current.first, elapsed)),
                    write_bytes_per_sec: previous.and_then(|previous| {
                        counter_rate(previous.second, current.second, elapsed)
                    }),
                    total_read_bytes: current.first,
                    total_write_bytes: current.second,
                }
            })
            .collect::<Vec<_>>();
        if disks.is_empty() {
            warnings.push("No readable whole-disk I/O counters were found.".into());
        }
        if unknown_identities > 0 {
            warnings.push(format!(
                "{unknown_identities} disk identity reading(s) were unavailable or changed during collection; their rates are unavailable."
            ));
        }
        self.previous_disks = next_baselines;
        self.previous_disk_time = Some(now);
        disks
    }

    fn sample_networks(&mut self, warnings: &mut Vec<String>) -> Vec<Network> {
        // Check identities on both sides of the counter read so a link replaced
        // during collection cannot seed a baseline with the previous link's data.
        let identities_before: BTreeMap<_, _> = fs::read_dir(self.sys_root.join("class/net"))
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                Some((name, network_ifindex(&entry.path())?))
            })
            .collect();
        let Some(text) = read_required(&self.proc_root.join("net/dev"), warnings) else {
            self.previous_networks.clear();
            self.previous_network_time = None;
            return Vec::new();
        };
        let now = Instant::now();
        let elapsed = self
            .previous_network_time
            .map(|previous| now.duration_since(previous).as_secs_f64());
        let counters = parse_networks(&text);
        let mut next_baselines = BTreeMap::new();
        let mut unknown_identities = 0;
        let mut unknown_states = 0;
        let networks = counters
            .iter()
            .map(|(name, current)| {
                let interface = self.sys_root.join("class/net").join(name);
                let ifindex = network_ifindex(&interface)
                    .filter(|index| identities_before.get(name) == Some(index));
                let previous = ifindex.and_then(|index| {
                    self.previous_networks
                        .get(name)
                        .filter(|previous| previous.ifindex == index)
                        .map(|previous| &previous.counters)
                });
                if let Some(ifindex) = ifindex {
                    next_baselines.insert(
                        name.clone(),
                        NetworkBaseline {
                            ifindex,
                            counters: *current,
                        },
                    );
                } else {
                    unknown_identities += 1;
                }
                // IFF_UP is administrative state; unlike operstate it also describes virtual links.
                let is_up = read_trimmed(interface.join("flags"))
                    .and_then(|text| u32::from_str_radix(text.trim_start_matches("0x"), 16).ok())
                    .map(|flags| flags & 1 != 0)
                    .or_else(
                        || match read_trimmed(interface.join("operstate")).as_deref() {
                            Some("up") => Some(true),
                            Some("down" | "lowerlayerdown") => Some(false),
                            _ => None,
                        },
                    );
                if is_up.is_none() {
                    unknown_states += 1;
                }
                Network {
                    name: name.clone(),
                    received_bytes_per_sec: previous
                        .and_then(|previous| counter_rate(previous.first, current.first, elapsed)),
                    transmitted_bytes_per_sec: previous.and_then(|previous| {
                        counter_rate(previous.second, current.second, elapsed)
                    }),
                    total_received_bytes: current.first,
                    total_transmitted_bytes: current.second,
                    is_up,
                }
            })
            .collect::<Vec<_>>();
        if networks.is_empty() {
            warnings.push("No non-loopback network interfaces were found.".into());
        }
        if unknown_identities > 0 {
            warnings.push(format!(
                "{unknown_identities} network interface identity reading(s) were unavailable or changed during collection; their rates are unavailable."
            ));
        }
        if unknown_states > 0 {
            warnings.push(format!(
                "{unknown_states} network interface status reading(s) were unavailable or unknown; their status is unavailable."
            ));
        }
        self.previous_networks = next_baselines;
        self.previous_network_time = Some(now);
        networks
    }
}

fn disk_sequence(device: &Path) -> Option<u64> {
    read_trimmed(device.join("diskseq"))?
        .parse()
        .ok()
        .filter(|sequence| *sequence > 0)
}

fn network_ifindex(interface: &Path) -> Option<u32> {
    read_trimmed(interface.join("ifindex"))?
        .parse()
        .ok()
        .filter(|index| *index > 0)
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn read_required(path: &Path, warnings: &mut Vec<String>) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(error) => {
            warnings.push(format!("Cannot read {}: {error}", path.display()));
            None
        }
    }
}

fn parse_nonnegative(text: &str) -> Option<f64> {
    text.parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn parse_positive(text: &str) -> Option<f64> {
    parse_nonnegative(text).filter(|value| *value > 0.0)
}

fn parse_cpu_times(text: &str) -> BTreeMap<String, CpuTimes> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            if name != "cpu"
                && name
                    .strip_prefix("cpu")
                    .and_then(|id| id.parse::<usize>().ok())
                    .is_none()
            {
                return None;
            }
            // guest and guest_nice (columns 9/10) already occur in user/nice.
            let values: Vec<u64> = fields
                .take(8)
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?;
            if values.len() < 4 {
                return None;
            }
            let mut counters = [0; 8];
            counters[..values.len()].copy_from_slice(&values);
            Some((name.to_owned(), CpuTimes(counters)))
        })
        .collect()
}

fn cpu_percent(previous: CpuTimes, current: CpuTimes) -> Option<f64> {
    let mut delta = [0u64; 8];
    for (index, value) in delta.iter_mut().enumerate() {
        // Linux documents that iowait can decrease without a counter reset.
        *value = if index == 4 {
            current.0[index].saturating_sub(previous.0[index])
        } else {
            current.0[index].checked_sub(previous.0[index])?
        };
    }
    let total: u128 = delta.iter().map(|value| u128::from(*value)).sum();
    if total == 0 {
        return None;
    }
    let idle = u128::from(delta[3]) + u128::from(delta[4]);
    Some(((total - idle) as f64 / total as f64 * 100.0).clamp(0.0, 100.0))
}

fn parse_cpuinfo(text: &str) -> (String, BTreeMap<usize, f64>) {
    let mut model = None;
    let mut frequencies = BTreeMap::new();
    for block in text.split("\n\n") {
        let mut id = None;
        let mut frequency = None;
        for line in block.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "processor" => id = value.parse::<usize>().ok(),
                "cpu MHz" => frequency = parse_positive(value),
                "model name" | "Hardware" | "Processor" if model.is_none() => {
                    model = Some(value.to_owned())
                }
                _ => {}
            }
        }
        if let (Some(id), Some(frequency)) = (id, frequency) {
            frequencies.insert(id, frequency);
        }
    }
    (
        model.unwrap_or_else(|| "Linux processor".into()),
        frequencies,
    )
}

fn parse_memory(text: &str) -> Option<(Memory, bool)> {
    let values: BTreeMap<&str, u64> = text
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            let mut fields = value.split_whitespace();
            let amount = fields.next()?.parse::<u64>().ok()?;
            if fields.next() != Some("kB") {
                return None;
            }
            Some((name, amount.checked_mul(1024)?))
        })
        .collect();
    let total = *values.get("MemTotal")?;
    if total == 0 {
        return None;
    }
    let get = |name: &str| values.get(name).copied().unwrap_or(0);
    let cached = get("Cached")
        .saturating_add(get("SReclaimable"))
        .saturating_sub(get("Shmem"))
        .min(total);
    let estimated = !values.contains_key("MemAvailable");
    let available = if estimated {
        values
            .get("MemFree")?
            .saturating_add(get("Buffers"))
            .saturating_add(cached)
    } else {
        get("MemAvailable")
    }
    .min(total);
    let swap_total = get("SwapTotal");
    Some((
        Memory {
            total_bytes: total,
            available_bytes: available,
            used_bytes: total.saturating_sub(available),
            cached_bytes: cached,
            swap_total_bytes: swap_total,
            swap_used_bytes: swap_total.saturating_sub(get("SwapFree")),
        },
        estimated,
    ))
}

fn parse_diskstats(text: &str) -> BTreeMap<String, Counters> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 10 {
                return None;
            }
            // Diskstats always uses 512-byte sectors, even on 4K-native disks.
            let read = fields[5].parse::<u64>().ok()?.checked_mul(512)?;
            let written = fields[9].parse::<u64>().ok()?.checked_mul(512)?;
            Some((
                fields[2].to_owned(),
                Counters {
                    first: read,
                    second: written,
                },
            ))
        })
        .collect()
}

fn leaf_block_devices(sys_root: &Path, warnings: &mut Vec<String>) -> Option<BTreeSet<String>> {
    let root = sys_root.join("block");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!(
                "Cannot discover whole disks in {}: {error}",
                root.display()
            ));
            return None;
        }
    };
    let mut devices = BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if ["loop", "ram", "zram"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            continue;
        }
        // Native NVMe multipath accounts I/O on both its visible namespace
        // and hidden path disks, without linking them through slaves/. Keep
        // the namespace's counters once, including on kernels predating the
        // newer multipath/ topology links.
        match fs::read_to_string(entry.path().join("hidden")) {
            Ok(hidden) => match hidden.trim() {
                "0" => {}
                "1" => continue,
                _ => {
                    warnings.push(format!(
                        "Cannot inspect {name} disk visibility: invalid hidden flag."
                    ));
                    continue;
                }
            },
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                warnings.push(format!("Cannot inspect {name} disk visibility: {error}"));
                continue;
            }
            _ => {}
        }
        // Whole-device entries exclude partitions. Stacked DM/MD devices repeat
        // I/O already accounted by their slave disks, so only retain leaf devices.
        match fs::read_dir(entry.path().join("slaves")) {
            Ok(mut slaves) => {
                if slaves.next().is_some() {
                    continue;
                }
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                warnings.push(format!("Cannot inspect {name} disk topology: {error}"));
                continue;
            }
            _ => {}
        }
        devices.insert(name);
    }
    Some(devices)
}

fn parse_networks(text: &str) -> BTreeMap<String, Counters> {
    text.lines()
        .filter_map(|line| {
            let (name, values) = line.rsplit_once(':')?;
            let name = name.trim();
            if name.is_empty() || name == "lo" {
                return None;
            }
            let fields: Vec<&str> = values.split_whitespace().collect();
            if fields.len() < 16 {
                return None;
            }
            Some((
                name.to_owned(),
                Counters {
                    first: fields[0].parse().ok()?,
                    second: fields[8].parse().ok()?,
                },
            ))
        })
        .collect()
}

fn counter_rate(previous: u64, current: u64, elapsed: Option<f64>) -> Option<f64> {
    let seconds = elapsed.filter(|seconds| seconds.is_finite() && *seconds > 0.0)?;
    let delta = current.checked_sub(previous)?;
    let rate = delta as f64 / seconds;
    rate.is_finite().then_some(rate)
}

fn cpu_sensor_name(source: &str, label: &str) -> bool {
    let source = source.to_ascii_lowercase();
    // PECI DIMM drivers include the owning CPU socket in their name, but
    // measure memory. A channel label must not override that known identity.
    if source == "peci_dimmtemp" || source.starts_with("peci_dimmtemp.") {
        return false;
    }
    let label = label.to_ascii_lowercase();
    // CPU VRM channels measure voltage regulators, not the processor. Keep
    // their readings in hardware details without promoting them to CPU sensors.
    if source.contains("vrm") || label.contains("vrm") {
        return false;
    }
    // Nuvoton's combined channel is max(CPU, MCH), so chipset heat can
    // dominate it. Keep the reading in hardware details, outside the CPU headline.
    if source == "pch_chip_cpu_max_temp" || label == "pch_chip_cpu_max_temp" {
        return false;
    }
    [
        "coretemp",
        "k10temp",
        "k8temp",
        "zenpower",
        "cpu",
        "x86_pkg",
        "soc_thermal",
        "tctl",
        "tdie",
    ]
    .iter()
    .any(|part| source.contains(part) || label.contains(part))
}

fn read_temperature(path: &Path) -> Option<f64> {
    let value = read_trimmed(path)?.parse::<f64>().ok()? / 1000.0;
    // Reject kernel sentinel values and readings outside credible device ranges.
    (value.is_finite() && (-100.0..=250.0).contains(&value)).then_some(value)
}

fn discover_sensors(sys_root: &Path, warnings: &mut Vec<String>) -> Vec<Sensor> {
    let mut sensors = Vec::new();
    let mut hwmon_sources = BTreeSet::new();
    let mut available_sources = BTreeSet::new();
    let mut rejected_sources = BTreeSet::new();
    let mut inaccessible = 0;
    let mut unconverted_channels = BTreeMap::<&str, usize>::new();
    if let Ok(entries) = fs::read_dir(sys_root.join("class/hwmon")) {
        for entry in entries.flatten() {
            let root = entry.path();
            let driver = read_trimmed(root.join("name"))
                .or_else(|| read_trimmed(root.join("device/name")))
                .unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
            let device = fs::canonicalize(&root)
                .unwrap_or_else(|_| root.clone())
                .to_string_lossy()
                .into_owned();
            let is_peci_cpu = driver == "peci_cputemp" || driver.starts_with("peci_cputemp.");
            // Chip names from the kernel's it87_devices table. This driver
            // defines _type=0 as unused; the generic hwmon ABI does not.
            let is_it87 = [
                "it87", "it8712", "it8716", "it8718", "it8720", "it8721", "it8728", "it8732",
                "it8771", "it8772", "it8781", "it8782", "it8783", "it8786", "it8790", "it8792",
                "it8603", "it8620", "it8622", "it8628", "it8689", "it87952",
            ]
            .contains(&driver.as_str());
            // Some older drivers expose attributes under hwmonN/device.
            for directory in [root.clone(), root.join("device")] {
                let Ok(attributes) = fs::read_dir(&directory) else {
                    continue;
                };
                for attribute in attributes.flatten() {
                    let name = attribute.file_name().to_string_lossy().into_owned();
                    let Some(channel) = name.strip_suffix("_input").filter(|name| {
                        name.strip_prefix("temp").is_some_and(|id| {
                            !id.is_empty() && id.chars().all(|character| character.is_ascii_digit())
                        })
                    }) else {
                        continue;
                    };
                    let path = attribute.path();
                    let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                    if rejected_sources.contains(&canonical) {
                        continue;
                    }
                    // PECI channels 3–5 expose Tcontrol, Tthrottle, and Tjmax
                    // targets through _input files, rather than measurements.
                    if is_peci_cpu && matches!(channel, "temp3" | "temp4" | "temp5") {
                        rejected_sources.insert(canonical);
                        continue;
                    }
                    if read_trimmed(directory.join(format!("{channel}_enable"))).as_deref()
                        == Some("0")
                        || (is_it87
                            && read_trimmed(directory.join(format!("{channel}_type"))).as_deref()
                                == Some("0"))
                    {
                        rejected_sources.insert(canonical);
                        continue;
                    }
                    // These external channels require board-specific conversion,
                    // and some expose millivolts rather than millidegrees. Keep
                    // the calibrated internal diode or PMIC channels.
                    let conversion_driver = match driver.as_str() {
                        "vt1211" if channel != "temp2" => Some("VT1211"),
                        "wm831x" if channel == "temp2" => Some("WM831x"),
                        "vt8231"
                            if matches!(
                                channel,
                                "temp2" | "temp3" | "temp4" | "temp5" | "temp6"
                            ) =>
                        {
                            Some("VT8231")
                        }
                        _ => None,
                    };
                    if let Some(driver) = conversion_driver {
                        *unconverted_channels.entry(driver).or_default() += 1;
                        rejected_sources.insert(canonical);
                        continue;
                    }
                    // A plausible input value is still invalid when the driver
                    // reports a disconnected or otherwise faulted sensor.
                    if read_trimmed(directory.join(format!("{channel}_fault"))).as_deref()
                        == Some("1")
                    {
                        inaccessible += 1;
                        rejected_sources.insert(canonical);
                        continue;
                    }
                    if !hwmon_sources.insert(canonical.clone()) {
                        continue;
                    }
                    let Some(celsius) = read_temperature(&path) else {
                        inaccessible += 1;
                        continue;
                    };
                    let label = read_trimmed(directory.join(format!("{channel}_label")))
                        .unwrap_or_else(|| channel.to_owned());
                    let cpu_temperature_source = match (driver.as_str(), label.as_str()) {
                        ("k10temp" | "zenpower", "Tctl") => CpuTemperatureSource::Control {
                            device: device.clone(),
                        },
                        ("k10temp" | "zenpower", "Tdie") => CpuTemperatureSource::Die {
                            device: device.clone(),
                        },
                        _ => CpuTemperatureSource::Other,
                    };
                    let is_cpu = cpu_sensor_name(&driver, &label);
                    let label = format!("{driver} · {label}");
                    let critical_celsius =
                        read_temperature(&directory.join(format!("{channel}_crit")))
                            .filter(|value| *value > 0.0);
                    sensors.push(Sensor {
                        id: canonical.to_string_lossy().into_owned(),
                        is_cpu,
                        label,
                        celsius,
                        critical_celsius,
                        cpu_temperature_source,
                    });
                    available_sources.insert(canonical);
                }
            }
        }
    }
    // An explicit fault, disabled state, unsupported unit, or control target
    // invalidates every alias, even if another hwmon view was read first.
    sensors.retain(|sensor| !rejected_sources.contains(Path::new(&sensor.id)));
    // A driver name or matching temperature cannot identify an individual sensor.
    // Skip known aliases of accepted or rejected inputs, but retain independent
    // zones, including x86 package readings with unknown coretemp association.
    if let Ok(entries) = fs::read_dir(sys_root.join("class/thermal")) {
        for entry in entries.flatten() {
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with("thermal_zone")
            {
                continue;
            }
            let directory = entry.path();
            let label = read_trimmed(directory.join("type"))
                .unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
            let path = directory.join("temp");
            let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if available_sources.contains(&canonical) || rejected_sources.contains(&canonical) {
                continue;
            }
            let Some(celsius) = read_temperature(&path) else {
                inaccessible += 1;
                continue;
            };
            let critical_celsius = fs::read_dir(&directory)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|attribute| {
                    let name = attribute.file_name().to_string_lossy().into_owned();
                    let prefix = name
                        .strip_suffix("_type")
                        .filter(|prefix| prefix.starts_with("trip_point_"))?;
                    (read_trimmed(attribute.path()).as_deref() == Some("critical"))
                        .then(|| read_temperature(&directory.join(format!("{prefix}_temp"))))
                        .flatten()
                        .filter(|temperature| *temperature > 0.0)
                })
                .min_by(f64::total_cmp);
            sensors.push(Sensor {
                id: canonical.to_string_lossy().into_owned(),
                is_cpu: cpu_sensor_name(&label, ""),
                label,
                celsius,
                critical_celsius,
                cpu_temperature_source: CpuTemperatureSource::Other,
            });
            available_sources.insert(canonical);
        }
    }
    sensors.sort_by(|left, right| {
        right
            .is_cpu
            .cmp(&left.is_cpu)
            .then(left.label.cmp(&right.label))
            .then(left.id.cmp(&right.id))
    });
    if sensors.is_empty() {
        warnings.push("Temperature sensors are unavailable. Hardware, drivers, or permissions may limit access.".into());
    } else if inaccessible > 0 {
        warnings.push(format!(
            "{inaccessible} temperature reading(s) were unavailable or invalid."
        ));
    }
    for (driver, count) in unconverted_channels {
        warnings.push(format!(
            "{count} {driver} external sensor channel(s) require board-specific conversion and are omitted."
        ));
    }
    sensors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frequency_snapshot(frequencies: &[Option<f64>]) -> Snapshot {
        Snapshot {
            cores: frequencies
                .iter()
                .enumerate()
                .map(|(id, &frequency_mhz)| Core {
                    id,
                    frequency_mhz,
                    ..Core::default()
                })
                .collect(),
            ..Snapshot::default()
        }
    }

    #[test]
    fn cpu_frequency_stats_include_minimum_average_and_maximum() {
        let snapshot = frequency_snapshot(&[Some(3200.0), Some(800.0), Some(2000.0)]);
        assert_eq!(
            snapshot.cpu_frequency_stats(),
            Some(FrequencyStats {
                min_mhz: 800.0,
                average_mhz: 2000.0,
                max_mhz: 3200.0,
            })
        );
    }

    #[test]
    fn cpu_frequency_stats_ignore_missing_and_invalid_readings() {
        let snapshot = frequency_snapshot(&[
            None,
            Some(900.0),
            Some(f64::NAN),
            Some(0.0),
            Some(-100.0),
            Some(f64::INFINITY),
            Some(f64::NEG_INFINITY),
            Some(2700.0),
        ]);
        assert_eq!(
            snapshot.cpu_frequency_stats(),
            Some(FrequencyStats {
                min_mhz: 900.0,
                average_mhz: 1800.0,
                max_mhz: 2700.0,
            })
        );
    }

    #[test]
    fn cpu_frequency_stats_match_the_only_available_core() {
        let snapshot = frequency_snapshot(&[None, Some(2400.5), None]);
        assert_eq!(
            snapshot.cpu_frequency_stats(),
            Some(FrequencyStats {
                min_mhz: 2400.5,
                average_mhz: 2400.5,
                max_mhz: 2400.5,
            })
        );
    }

    #[test]
    fn cpu_frequency_stats_are_missing_without_valid_readings() {
        assert_eq!(Snapshot::default().cpu_frequency_stats(), None);
        assert_eq!(
            frequency_snapshot(&[None, None]).cpu_frequency_stats(),
            None
        );
        assert_eq!(
            frequency_snapshot(&[Some(0.0), Some(-1.0), Some(f64::NAN)]).cpu_frequency_stats(),
            None
        );
    }

    #[test]
    fn cpu_guest_time_is_not_counted_twice() {
        let before = parse_cpu_times("cpu 100 20 30 1000 0 0 0 0 40 10");
        let after = parse_cpu_times("cpu 150 30 40 1030 0 0 0 0 80 20");
        assert_eq!(cpu_percent(before["cpu"], after["cpu"]), Some(70.0));
    }

    #[test]
    fn cpu_counter_reset_and_unchanged_sample_are_missing() {
        let counters = CpuTimes([100, 0, 20, 50, 0, 0, 0, 0]);
        assert_eq!(cpu_percent(counters, counters), None);
        assert_eq!(
            cpu_percent(counters, CpuTimes([5, 0, 2, 10, 0, 0, 0, 0])),
            None
        );
    }

    #[test]
    fn cpu_iowait_decrease_does_not_invalidate_other_counters() {
        let before = CpuTimes([100, 0, 20, 100, 15, 0, 0, 0]);
        let after = CpuTimes([120, 0, 30, 170, 10, 0, 0, 0]);
        assert_eq!(cpu_percent(before, after), Some(30.0));
    }

    #[test]
    fn cpu_parser_ignores_other_stat_fields_and_malformed_rows() {
        let counters = parse_cpu_times(
            "cpu 1 2 3 4\ncpu0 1 2 3 4 5 6 7 8\ncpu4 1 bad 2 3\ncpufreq 10 20 30 40\nintr 500",
        );
        assert_eq!(counters.len(), 2);
        assert_eq!(counters["cpu"].0, [1, 2, 3, 4, 0, 0, 0, 0]);
    }

    #[test]
    fn rates_use_actual_elapsed_and_preserve_missing_samples() {
        assert_eq!(counter_rate(100, 300, Some(0.5)), Some(400.0));
        assert_eq!(counter_rate(100, 300, Some(5.0)), Some(40.0));
        assert_eq!(counter_rate(100, 100, Some(1.0)), Some(0.0));
        assert_eq!(counter_rate(300, 100, Some(1.0)), None);
        assert_eq!(counter_rate(100, 300, Some(0.0)), None);
        assert_eq!(counter_rate(100, 300, Some(f64::NAN)), None);
        assert_eq!(counter_rate(100, 300, None), None);
    }

    #[test]
    fn disk_sectors_are_always_512_bytes() {
        let counters = parse_diskstats(
            "259 0 nvme0n1 255999 814 12369153 47919 996852 81 36123024 425995 0 301795 580470",
        );
        assert_eq!(counters["nvme0n1"].first, 12_369_153 * 512);
        assert_eq!(counters["nvme0n1"].second, 36_123_024 * 512);
        assert!(parse_diskstats("8 0 bad short line").is_empty());
    }

    #[test]
    fn network_parser_separates_receive_transmit_and_omits_loopback() {
        let counters = parse_networks(
            "Inter-| Receive | Transmit\n lo: 99 1 0 0 0 0 0 0 99 1 0 0 0 0 0 0\n eth0: 1024 2 0 0 0 0 0 0 2048 4 0 0 0 0 0 0\n bad: 1 2",
        );
        assert_eq!(counters.len(), 1);
        assert_eq!(counters["eth0"].first, 1024);
        assert_eq!(counters["eth0"].second, 2048);
    }

    #[test]
    fn memory_uses_available_not_free_and_converts_kibibytes() {
        let (memory, estimated) = parse_memory("MemTotal: 1000 kB\nMemAvailable: 400 kB\nMemFree: 100 kB\nCached: 300 kB\nSReclaimable: 50 kB\nShmem: 20 kB\nSwapTotal: 200 kB\nSwapFree: 70 kB").unwrap();
        assert!(!estimated);
        assert_eq!(memory.total_bytes, 1_024_000);
        assert_eq!(memory.used_bytes, 600 * 1024);
        assert_eq!(memory.cached_bytes, 330 * 1024);
        assert_eq!(memory.swap_used_bytes, 130 * 1024);
    }

    #[test]
    fn memory_fallback_and_inconsistent_counters_are_safe() {
        let (memory, estimated) =
            parse_memory("MemTotal: 100 kB\nMemFree: 40 kB\nBuffers: 10 kB\nCached: 80 kB")
                .unwrap();
        assert!(estimated);
        assert_eq!(memory.available_bytes, memory.total_bytes);
        assert_eq!(memory.used_bytes, 0);
        assert!(parse_memory("MemFree: 500 kB").is_none());
        assert!(parse_memory("MemTotal: invalid kB").is_none());
    }

    #[test]
    fn cpuinfo_preserves_sparse_core_ids_and_rejects_nan() {
        let (model, frequencies) = parse_cpuinfo(
            "processor : 0\nmodel name : Example CPU\ncpu MHz : 1800.5\n\nprocessor : 8\ncpu MHz : NaN\n\nprocessor : 12\ncpu MHz : 2400\n",
        );
        assert_eq!(model, "Example CPU");
        assert_eq!(frequencies.get(&0), Some(&1800.5));
        assert!(!frequencies.contains_key(&8));
        assert_eq!(frequencies.get(&12), Some(&2400.0));
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "loadpeek-metrics-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, path: &str, text: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn collector_first_sample_and_missing_sources_do_not_invent_rates() {
        let fixture = Fixture::new();
        fixture.write(
            "proc/stat",
            "cpu 100 0 100 800 0 0 0 0\ncpu0 100 0 100 800 0 0 0 0",
        );
        fixture.write("proc/diskstats", "8 0 sda 1 0 20 0 2 0 30 0 0 0 0");
        fixture.write("sys/block/sda/slaves/.keep", "");
        fs::remove_file(fixture.0.join("sys/block/sda/slaves/.keep")).unwrap();
        fixture.write("proc/net/dev", "eth0: 100 0 0 0 0 0 0 0 200 0 0 0 0 0 0 0");
        let mut collector = Collector::with_roots(fixture.0.join("proc"), fixture.0.join("sys"));
        let sample = collector.sample();
        assert_eq!(sample.cpu_percent, None);
        assert_eq!(sample.cores[0].percent, None);
        assert_eq!(sample.disks[0].read_bytes_per_sec, None);
        assert_eq!(sample.networks[0].received_bytes_per_sec, None);
        assert!(sample.uptime_secs.is_nan());
        assert_eq!(sample.memory.total_bytes, 0);
        assert!(sample.sensors.is_empty());
        assert!(!sample.warnings.is_empty());
        fs::remove_file(fixture.0.join("proc/stat")).unwrap();
        assert!(collector.sample().cpu_percent.is_none());
        fixture.write("proc/stat", "cpu 200 0 200 1600 0 0 0 0");
        assert!(collector.sample().cpu_percent.is_none());
    }

    #[test]
    fn block_discovery_omits_memory_disks_and_stacked_devices() {
        let fixture = Fixture::new();
        fixture.write("block/nvme0n1/slaves/.keep", "");
        fs::remove_file(fixture.0.join("block/nvme0n1/slaves/.keep")).unwrap();
        fixture.write("block/dm-0/slaves/nvme0n1", "");
        fixture.write("block/loop0/stat", "");
        fixture.write("block/zram0/stat", "");
        let devices = leaf_block_devices(&fixture.0, &mut Vec::new()).unwrap();
        assert_eq!(devices, BTreeSet::from(["nvme0n1".to_owned()]));
    }

    #[test]
    fn native_nvme_multipath_counts_one_layer_and_keeps_ordinary_nvme() {
        let fixture = Fixture::new();
        for (name, diskseq, hidden) in [
            ("nvme0n1", "10", "0"),
            ("nvme0c0n1", "11", "1"),
            ("nvme0c1n1", "12", "1"),
            ("nvme1n1", "13", "0"),
        ] {
            fixture.write(&format!("sys/block/{name}/diskseq"), diskseq);
            fixture.write(&format!("sys/block/{name}/hidden"), hidden);
            fs::create_dir_all(fixture.0.join(format!("sys/block/{name}/slaves"))).unwrap();
        }
        // Native multipath does not populate slaves/. Both the visible
        // namespace and its hidden paths account for the same requests.
        fixture.write(
            "proc/diskstats",
            "259 0 nvme0n1 1 0 1000 0 1 0 500 0 0 0 0\n\
             259 1 nvme0c0n1 1 0 400 0 1 0 200 0 0 0 0\n\
             259 2 nvme0c1n1 1 0 600 0 1 0 300 0 0 0 0\n\
             259 3 nvme1n1 1 0 100 0 1 0 100 0 0 0 0",
        );
        let mut collector = Collector::with_roots(fixture.0.join("proc"), fixture.0.join("sys"));
        let mut warnings = Vec::new();
        let before = collector.sample_disks(&mut warnings);
        assert_eq!(
            before
                .iter()
                .map(|disk| disk.name.as_str())
                .collect::<Vec<_>>(),
            ["nvme0n1", "nvme1n1"]
        );
        assert!(before.iter().all(|disk| disk.read_bytes_per_sec.is_none()));
        fixture.write(
            "proc/diskstats",
            "259 0 nvme0n1 1 0 1200 0 1 0 600 0 0 0 0\n\
             259 1 nvme0c0n1 1 0 500 0 1 0 250 0 0 0 0\n\
             259 2 nvme0c1n1 1 0 700 0 1 0 350 0 0 0 0\n\
             259 3 nvme1n1 1 0 150 0 1 0 125 0 0 0 0",
        );
        let after = collector.sample_disks(&mut warnings);
        let read_delta: u64 = before
            .iter()
            .zip(&after)
            .map(|(before, after)| after.total_read_bytes - before.total_read_bytes)
            .sum();
        let write_delta: u64 = before
            .iter()
            .zip(&after)
            .map(|(before, after)| after.total_write_bytes - before.total_write_bytes)
            .sum();
        assert_eq!(read_delta, 250 * 512);
        assert_eq!(write_delta, 125 * 512);
        let read_rate: f64 = after
            .iter()
            .map(|disk| disk.read_bytes_per_sec.unwrap())
            .sum();
        let write_rate: f64 = after
            .iter()
            .map(|disk| disk.write_bytes_per_sec.unwrap())
            .sum();
        assert!((read_rate / after[0].read_bytes_per_sec.unwrap() - 1.25).abs() < 1e-12);
        assert!((write_rate / after[0].write_bytes_per_sec.unwrap() - 1.25).abs() < 1e-12);
        assert!(warnings.is_empty());
    }

    #[test]
    fn block_discovery_omits_uncertain_visibility_but_keeps_legacy_devices() {
        let fixture = Fixture::new();
        fixture.write("block/sda/diskseq", "1");
        fixture.write("block/sdb/hidden", "invalid");
        fs::create_dir_all(fixture.0.join("block/sdc/hidden")).unwrap();
        let mut warnings = Vec::new();
        let devices = leaf_block_devices(&fixture.0, &mut warnings).unwrap();
        assert_eq!(devices, BTreeSet::from(["sda".to_owned()]));
        assert_eq!(warnings.len(), 2);
        assert!(
            warnings
                .iter()
                .all(|warning| warning.contains("disk visibility"))
        );
    }

    #[test]
    fn replacing_a_disk_requires_a_new_baseline() {
        let fixture = Fixture::new();
        fixture.write("sys/block/sda/diskseq", "10");
        fixture.write("proc/diskstats", "8 0 sda 1 0 20 0 2 0 30 0 0 0 0");
        let mut collector = Collector::with_roots(fixture.0.join("proc"), fixture.0.join("sys"));
        let mut warnings = Vec::new();
        assert!(
            collector.sample_disks(&mut warnings)[0]
                .read_bytes_per_sec
                .is_none()
        );
        let steady = collector.sample_disks(&mut warnings);
        assert_eq!(steady[0].read_bytes_per_sec, Some(0.0));
        assert_eq!(steady[0].write_bytes_per_sec, Some(0.0));

        // A newly attached disk can already have larger counters by the next
        // sample, so a decreasing-counter check cannot identify replacement.
        fixture.write("sys/block/sda/diskseq", "11");
        fixture.write("proc/diskstats", "8 0 sda 1 0 1000 0 2 0 2000 0 0 0 0");
        let replacement = collector.sample_disks(&mut warnings);
        assert_eq!(replacement[0].total_read_bytes, 512_000);
        assert_eq!(replacement[0].total_write_bytes, 1_024_000);
        assert_eq!(replacement[0].read_bytes_per_sec, None);
        assert_eq!(replacement[0].write_bytes_per_sec, None);
        assert_eq!(
            collector.sample_disks(&mut warnings)[0].read_bytes_per_sec,
            Some(0.0)
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn unavailable_disk_identity_preserves_totals_and_reprimes_rates() {
        let fixture = Fixture::new();
        let identity = "sys/block/sda/diskseq";
        fixture.write(identity, "10");
        fixture.write("proc/diskstats", "8 0 sda 1 0 20 0 2 0 30 0 0 0 0");
        let mut collector = Collector::with_roots(fixture.0.join("proc"), fixture.0.join("sys"));
        let mut warnings = Vec::new();
        collector.sample_disks(&mut warnings);
        assert_eq!(
            collector.sample_disks(&mut warnings)[0].read_bytes_per_sec,
            Some(0.0)
        );
        for invalid in [None, Some(""), Some("invalid"), Some("0"), Some("-1")] {
            if let Some(value) = invalid {
                fixture.write(identity, value);
            } else {
                fs::remove_file(fixture.0.join(identity)).unwrap();
            }
            warnings.clear();
            let sample = collector.sample_disks(&mut warnings);
            assert_eq!(sample[0].total_read_bytes, 20 * 512);
            assert_eq!(sample[0].total_write_bytes, 30 * 512);
            assert_eq!(sample[0].read_bytes_per_sec, None);
            assert_eq!(sample[0].write_bytes_per_sec, None);
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.contains("disk identity"))
            );
            fixture.write(identity, "10");
            warnings.clear();
            let recovered = collector.sample_disks(&mut warnings);
            assert_eq!(recovered[0].read_bytes_per_sec, None);
            assert_eq!(recovered[0].write_bytes_per_sec, None);
            assert_eq!(
                collector.sample_disks(&mut warnings)[0].read_bytes_per_sec,
                Some(0.0)
            );
            assert!(warnings.is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn replacement_during_diskstats_read_does_not_seed_a_mixed_baseline() {
        use std::{ffi::CString, io::Write, os::unix::ffi::OsStrExt, thread};

        let fixture = Fixture::new();
        fixture.write("sys/block/sda/diskseq", "10");
        let old_counters = "8 0 sda 1 0 20 0 2 0 30 0 0 0 0";
        fixture.write("proc/diskstats", old_counters);
        let mut collector = Collector::with_roots(fixture.0.join("proc"), fixture.0.join("sys"));
        collector.sample_disks(&mut Vec::new());
        let counter_path = fixture.0.join("proc/diskstats");
        fs::remove_file(&counter_path).unwrap();
        let fifo_path = CString::new(counter_path.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo_path is a live, NUL-terminated path in this test's private directory.
        assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);
        let worker = thread::spawn(move || {
            let mut warnings = Vec::new();
            let sample = collector.sample_disks(&mut warnings);
            (collector, sample, warnings)
        });
        // Opening the writer waits until sample_disks has captured identities
        // and opened the counter file. Publish old counters only after hotplug.
        let mut writer = fs::OpenOptions::new()
            .write(true)
            .open(&counter_path)
            .unwrap();
        fixture.write("sys/block/sda/diskseq", "11");
        writer.write_all(old_counters.as_bytes()).unwrap();
        drop(writer);
        let (mut collector, raced, warnings) = worker.join().unwrap();
        assert_eq!(raced[0].read_bytes_per_sec, None);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("disk identity"))
        );

        fs::remove_file(&counter_path).unwrap();
        fixture.write("proc/diskstats", "8 0 sda 1 0 1000 0 2 0 2000 0 0 0 0");
        let mut warnings = Vec::new();
        let fresh = collector.sample_disks(&mut warnings);
        assert_eq!(fresh[0].read_bytes_per_sec, None);
        assert_eq!(fresh[0].write_bytes_per_sec, None);
        assert_eq!(
            collector.sample_disks(&mut warnings)[0].read_bytes_per_sec,
            Some(0.0)
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn replacing_a_network_interface_requires_a_new_baseline() {
        let fixture = Fixture::new();
        fixture.write("sys/class/net/tun0/ifindex", "7");
        fixture.write("sys/class/net/tun0/flags", "0x1");
        fixture.write(
            "proc/net/dev",
            "tun0: 1000 0 0 0 0 0 0 0 2000 0 0 0 0 0 0 0",
        );
        let mut collector = Collector::with_roots(fixture.0.join("proc"), fixture.0.join("sys"));
        let mut warnings = Vec::new();
        assert!(
            collector.sample_networks(&mut warnings)[0]
                .received_bytes_per_sec
                .is_none()
        );
        let steady = collector.sample_networks(&mut warnings);
        assert_eq!(steady[0].received_bytes_per_sec, Some(0.0));
        assert_eq!(steady[0].transmitted_bytes_per_sec, Some(0.0));

        // Reuse the name with larger counters: a decrease cannot detect this.
        fixture.write("sys/class/net/tun0/ifindex", "8");
        fixture.write(
            "proc/net/dev",
            "tun0: 5000 0 0 0 0 0 0 0 8000 0 0 0 0 0 0 0",
        );
        let replaced = collector.sample_networks(&mut warnings);
        assert_eq!(replaced[0].total_received_bytes, 5000);
        assert_eq!(replaced[0].total_transmitted_bytes, 8000);
        assert_eq!(replaced[0].received_bytes_per_sec, None);
        assert_eq!(replaced[0].transmitted_bytes_per_sec, None);
        let steady = collector.sample_networks(&mut warnings);
        assert_eq!(steady[0].received_bytes_per_sec, Some(0.0));
        assert_eq!(steady[0].transmitted_bytes_per_sec, Some(0.0));
        assert!(warnings.is_empty());
    }

    #[test]
    fn missing_or_invalid_interface_identity_preserves_totals_and_reprimes_rates() {
        let fixture = Fixture::new();
        fixture.write("sys/class/net/tun0/ifindex", "7");
        fixture.write("sys/class/net/eth0/ifindex", "2");
        fixture.write("sys/class/net/tun0/flags", "0x1");
        fixture.write("sys/class/net/eth0/flags", "0x1");
        fixture.write(
            "proc/net/dev",
            "tun0: 1000 0 0 0 0 0 0 0 2000 0 0 0 0 0 0 0\neth0: 3000 0 0 0 0 0 0 0 4000 0 0 0 0 0 0 0",
        );
        let mut collector = Collector::with_roots(fixture.0.join("proc"), fixture.0.join("sys"));
        let mut warnings = Vec::new();
        collector.sample_networks(&mut warnings);
        collector.sample_networks(&mut warnings);

        for identity in [None, Some(""), Some("0"), Some("-1"), Some("bad")] {
            if let Some(identity) = identity {
                fixture.write("sys/class/net/tun0/ifindex", identity);
            } else {
                fs::remove_file(fixture.0.join("sys/class/net/tun0/ifindex")).unwrap();
            }
            warnings.clear();
            let unavailable = collector.sample_networks(&mut warnings);
            let tun = unavailable
                .iter()
                .find(|network| network.name == "tun0")
                .unwrap();
            assert_eq!(tun.total_received_bytes, 1000);
            assert_eq!(tun.total_transmitted_bytes, 2000);
            assert_eq!(tun.received_bytes_per_sec, None);
            assert_eq!(tun.transmitted_bytes_per_sec, None);
            let eth = unavailable
                .iter()
                .find(|network| network.name == "eth0")
                .unwrap();
            assert_eq!(eth.received_bytes_per_sec, Some(0.0));
            assert_eq!(warnings.len(), 1);
            assert!(warnings[0].contains("identity"));

            fixture.write("sys/class/net/tun0/ifindex", "7");
            warnings.clear();
            let recovered = collector.sample_networks(&mut warnings);
            let tun = recovered
                .iter()
                .find(|network| network.name == "tun0")
                .unwrap();
            assert_eq!(tun.received_bytes_per_sec, None);
            assert_eq!(tun.transmitted_bytes_per_sec, None);
            let steady = collector.sample_networks(&mut warnings);
            let tun = steady
                .iter()
                .find(|network| network.name == "tun0")
                .unwrap();
            assert_eq!(tun.received_bytes_per_sec, Some(0.0));
            assert_eq!(tun.transmitted_bytes_per_sec, Some(0.0));
            assert!(warnings.is_empty());
        }
    }

    #[test]
    fn unavailable_network_status_preserves_counters_and_recovers() {
        let fixture = Fixture::new();
        fixture.write("sys/class/net/tun0/ifindex", "7");
        fixture.write(
            "proc/net/dev",
            "tun0: 1000 0 0 0 0 0 0 0 2000 0 0 0 0 0 0 0",
        );
        let mut collector = Collector::with_roots(fixture.0.join("proc"), fixture.0.join("sys"));
        let mut warnings = Vec::new();
        let first = collector.sample_networks(&mut warnings);
        assert_eq!(first[0].is_up, None);
        assert_eq!(first[0].received_bytes_per_sec, None);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("status"));

        for state in [None, Some("unknown"), Some(""), Some("invalid")] {
            if let Some(state) = state {
                fixture.write("sys/class/net/tun0/operstate", state);
            }
            warnings.clear();
            let networks = collector.sample_networks(&mut warnings);
            let network = &networks[0];
            assert_eq!(network.is_up, None);
            assert_eq!(network.total_received_bytes, 1000);
            assert_eq!(network.total_transmitted_bytes, 2000);
            assert_eq!(network.received_bytes_per_sec, Some(0.0));
            assert_eq!(network.transmitted_bytes_per_sec, Some(0.0));
            assert_eq!(warnings.len(), 1);
            assert!(warnings[0].contains("status"));
        }

        // A read error must behave like a missing state, even with valid identity.
        fixture.write("sys/class/net/tun0/flags", "invalid");
        fs::remove_file(fixture.0.join("sys/class/net/tun0/operstate")).unwrap();
        fs::create_dir(fixture.0.join("sys/class/net/tun0/operstate")).unwrap();
        warnings.clear();
        assert_eq!(collector.sample_networks(&mut warnings)[0].is_up, None);
        assert_eq!(warnings.len(), 1);

        // Status recovery does not discard an otherwise valid rate baseline.
        fixture.write("sys/class/net/tun0/flags", "0x1");
        warnings.clear();
        let recovered = collector.sample_networks(&mut warnings);
        assert_eq!(recovered[0].is_up, Some(true));
        assert_eq!(recovered[0].received_bytes_per_sec, Some(0.0));
        assert!(warnings.is_empty());
    }

    #[test]
    fn network_status_prefers_flags_and_requires_a_definite_fallback() {
        let fixture = Fixture::new();
        fixture.write("sys/class/net/tun0/ifindex", "7");
        fixture.write(
            "proc/net/dev",
            "tun0: 1000 0 0 0 0 0 0 0 2000 0 0 0 0 0 0 0",
        );
        let mut collector = Collector::with_roots(fixture.0.join("proc"), fixture.0.join("sys"));
        for (flags, state, expected) in [
            ("0x1", "unknown", Some(true)),
            ("0x1", "down", Some(true)),
            ("0x0", "up", Some(false)),
            ("invalid", "up", Some(true)),
            ("invalid", "down", Some(false)),
            ("invalid", "lowerlayerdown", Some(false)),
            ("invalid", "unknown", None),
            ("", "dormant", None),
            ("", "testing", None),
        ] {
            fixture.write("sys/class/net/tun0/flags", flags);
            fixture.write("sys/class/net/tun0/operstate", state);
            let mut warnings = Vec::new();
            let networks = collector.sample_networks(&mut warnings);
            assert_eq!(networks[0].is_up, expected, "flags={flags}, state={state}");
            assert_eq!(warnings.len(), usize::from(expected.is_none()));
        }
    }

    #[test]
    fn legacy_hwmon_device_names_identify_cpu_readings() {
        let fixture = Fixture::new();
        fixture.write("class/hwmon/hwmon0/device/name", "k8temp");
        fixture.write("class/hwmon/hwmon0/device/temp1_input", "45000");
        let snapshot = Snapshot {
            sensors: discover_sensors(&fixture.0, &mut Vec::new()),
            ..Snapshot::default()
        };
        assert_eq!(snapshot.sensors[0].label, "k8temp · temp1");
        assert!(snapshot.sensors[0].is_cpu);
        assert_eq!(snapshot.cpu_temperature(), Some(45.0));

        // A name provided on the hwmon class device takes precedence.
        fixture.write("class/hwmon/hwmon0/name", "coretemp");
        assert_eq!(
            discover_sensors(&fixture.0, &mut Vec::new())[0].label,
            "coretemp · temp1"
        );
    }

    fn write_amd_temperatures(fixture: &Fixture, hwmon: usize, die: Option<&str>) {
        let root = format!("class/hwmon/hwmon{hwmon}");
        fixture.write(&format!("{root}/name"), "k10temp");
        fixture.write(&format!("{root}/temp1_label"), "Tctl");
        fixture.write(&format!("{root}/temp1_input"), "75000");
        if let Some(die) = die {
            fixture.write(&format!("{root}/temp2_label"), "Tdie");
            fixture.write(&format!("{root}/temp2_input"), die);
        }
    }

    #[test]
    fn cpu_temperature_prefers_die_but_retains_control_details_and_hottest_ccd() {
        let fixture = Fixture::new();
        write_amd_temperatures(&fixture, 0, Some("55000"));
        fixture.write("class/hwmon/hwmon0/temp3_label", "Tccd1");
        fixture.write("class/hwmon/hwmon0/temp3_input", "65000");
        let snapshot = Snapshot {
            sensors: discover_sensors(&fixture.0, &mut Vec::new()),
            ..Snapshot::default()
        };
        assert_eq!(snapshot.cpu_temperature(), Some(65.0));
        assert_eq!(snapshot.sensors.len(), 3);
        let control = snapshot
            .sensors
            .iter()
            .find(|sensor| sensor.label.ends_with("Tctl"))
            .unwrap();
        assert_eq!(control.celsius, 75.0);
        assert!(control.is_cpu);
    }

    #[test]
    fn cpu_temperature_control_fallback_is_per_device_and_recovers_from_faults() {
        let fixture = Fixture::new();
        write_amd_temperatures(&fixture, 0, Some("55000"));
        write_amd_temperatures(&fixture, 1, None);
        let sample = || Snapshot {
            sensors: discover_sensors(&fixture.0, &mut Vec::new()),
            ..Snapshot::default()
        };
        // Another CPU's physical reading must not hide this CPU's only channel.
        assert_eq!(sample().cpu_temperature(), Some(75.0));
        write_amd_temperatures(&fixture, 1, Some("60000"));
        assert_eq!(sample().cpu_temperature(), Some(60.0));

        fixture.write("class/hwmon/hwmon1/temp2_fault", "1");
        assert_eq!(sample().cpu_temperature(), Some(75.0));
        fixture.write("class/hwmon/hwmon1/temp2_fault", "0");
        fixture.write("class/hwmon/hwmon1/temp2_input", "NaN");
        assert_eq!(sample().cpu_temperature(), Some(75.0));
        fixture.write("class/hwmon/hwmon1/temp2_input", "60000");
        assert_eq!(sample().cpu_temperature(), Some(60.0));
    }

    #[test]
    fn temperature_discovery_uses_labels_critical_limits_and_fallbacks() {
        let fixture = Fixture::new();
        fixture.write("class/hwmon/hwmon0/name", "coretemp");
        fixture.write("class/hwmon/hwmon0/temp1_label", "Package id 0");
        fixture.write("class/hwmon/hwmon0/temp1_input", "51500");
        fixture.write("class/hwmon/hwmon0/temp1_crit", "100000");
        fixture.write("class/thermal/thermal_zone0/type", "x86_pkg_temp");
        fixture.write("class/thermal/thermal_zone0/temp", "51500");
        fixture.write("class/thermal/thermal_zone1/type", "acpitz");
        fixture.write("class/thermal/thermal_zone1/temp", "43000");
        fixture.write("class/thermal/thermal_zone1/trip_point_0_type", "critical");
        fixture.write("class/thermal/thermal_zone1/trip_point_0_temp", "105000");
        let sensors = discover_sensors(&fixture.0, &mut Vec::new());
        // Equal readings and compatible driver names do not prove a shared input.
        assert_eq!(sensors.len(), 3);
        assert!(sensors[0].is_cpu);
        assert_eq!(sensors[0].label, "coretemp · Package id 0");
        assert_eq!(sensors[0].celsius, 51.5);
        assert_eq!(sensors[0].critical_celsius, Some(100.0));
        assert_eq!(sensors[1].label, "x86_pkg_temp");
        assert_eq!(sensors[2].critical_celsius, Some(105.0));
    }

    #[test]
    fn temperature_zones_fill_partial_hwmon_package_readings() {
        let fixture = Fixture::new();
        fixture.write("class/hwmon/hwmon0/name", "coretemp");
        fixture.write("class/hwmon/hwmon0/temp1_label", "Package id 0");
        fixture.write("class/hwmon/hwmon0/temp1_input", "40000");
        fixture.write("class/hwmon/hwmon1/name", "coretemp");
        fixture.write("class/hwmon/hwmon1/temp1_label", "Package id 1");
        fixture.write("class/hwmon/hwmon1/temp2_label", "Core 0");
        fixture.write("class/hwmon/hwmon1/temp2_input", "45000");
        fixture.write("class/thermal/thermal_zone0/type", "x86_pkg_temp");
        fixture.write("class/thermal/thermal_zone0/temp", "40000");
        fixture.write("class/thermal/thermal_zone1/type", "x86_pkg_temp");
        fixture.write("class/thermal/thermal_zone1/temp", "80000");

        // Another package and a readable core must not hide this package's zone.
        for package_input in [None, Some("invalid")] {
            if let Some(value) = package_input {
                fixture.write("class/hwmon/hwmon1/temp1_input", value);
            }
            let snapshot = Snapshot {
                sensors: discover_sensors(&fixture.0, &mut Vec::new()),
                ..Snapshot::default()
            };
            assert_eq!(snapshot.cpu_temperature(), Some(80.0));
            assert_eq!(snapshot.sensors.len(), 4);
            assert_eq!(
                snapshot
                    .sensors
                    .iter()
                    .filter(|sensor| sensor.label == "x86_pkg_temp")
                    .count(),
                2
            );
        }
    }

    #[test]
    fn temperature_zones_with_the_same_driver_remain_independent() {
        let fixture = Fixture::new();
        fixture.write("class/hwmon/hwmon0/name", "acpitz");
        fixture.write("class/hwmon/hwmon0/temp1_input", "40000");
        fixture.write("class/hwmon/hwmon0/temp2_input", "invalid");
        fixture.write("class/thermal/thermal_zone0/type", "acpitz");
        fixture.write("class/thermal/thermal_zone0/temp", "40000");
        fixture.write("class/thermal/thermal_zone1/type", "acpitz");
        fixture.write("class/thermal/thermal_zone1/temp", "80000");
        let mut warnings = Vec::new();
        let sensors = discover_sensors(&fixture.0, &mut warnings);
        assert_eq!(sensors.len(), 3);
        assert_eq!(
            sensors
                .iter()
                .filter(|sensor| sensor.celsius == 40.0)
                .count(),
            2
        );
        assert_eq!(
            sensors
                .iter()
                .filter(|sensor| sensor.celsius == 80.0)
                .count(),
            1
        );
        assert_eq!(
            warnings,
            ["1 temperature reading(s) were unavailable or invalid."]
        );
    }

    #[test]
    fn temperature_aliases_share_measurement_rejections_and_recover() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        fixture.write("class/hwmon/hwmon0/name", "acpitz");
        fixture.write("class/hwmon/hwmon0/temp1_input", "55000");
        fixture.write("class/hwmon/hwmon0/temp1_crit", "95000");
        fixture.write("class/thermal/thermal_zone0/type", "acpitz");
        fixture.write("class/thermal/thermal_zone0/trip_point_0_type", "critical");
        fixture.write("class/thermal/thermal_zone0/trip_point_0_temp", "95000");
        symlink(
            fixture.0.join("class/hwmon/hwmon0/temp1_input"),
            fixture.0.join("class/thermal/thermal_zone0/temp"),
        )
        .unwrap();

        let mut warnings = Vec::new();
        let sensors = discover_sensors(&fixture.0, &mut warnings);
        assert_eq!(sensors.len(), 1);
        assert_eq!(sensors[0].label, "acpitz · temp1");
        assert_eq!(sensors[0].critical_celsius, Some(95.0));
        assert!(warnings.is_empty());

        // A fault invalidates this input through both paths even though the
        // thermal alias does not expose its own fault flag.
        fixture.write("class/hwmon/hwmon0/temp1_fault", "1");
        let sensors = discover_sensors(&fixture.0, &mut warnings);
        assert!(sensors.is_empty());
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("Temperature sensors are unavailable"))
        );

        fixture.write("class/hwmon/hwmon0/temp1_fault", "0");
        warnings.clear();
        let sensors = discover_sensors(&fixture.0, &mut warnings);
        assert_eq!(sensors.len(), 1);
        assert_eq!(sensors[0].label, "acpitz · temp1");
        assert!(warnings.is_empty());

        fixture.write("class/hwmon/hwmon0/temp1_enable", "0");
        assert!(discover_sensors(&fixture.0, &mut Vec::new()).is_empty());
        fixture.write("class/hwmon/hwmon0/temp1_enable", "1");
        assert_eq!(discover_sensors(&fixture.0, &mut Vec::new()).len(), 1);

        // The legacy view is scanned after the modern one. A rejection found
        // later must also invalidate a reading accepted through the first view.
        fixture.write("class/hwmon/hwmon0/device/temp1_fault", "1");
        symlink(
            fixture.0.join("class/hwmon/hwmon0/temp1_input"),
            fixture.0.join("class/hwmon/hwmon0/device/temp1_input"),
        )
        .unwrap();
        assert!(discover_sensors(&fixture.0, &mut Vec::new()).is_empty());
    }

    #[test]
    fn temperature_vt1211_requires_conversion_except_for_its_internal_diode() {
        for directory in ["class/hwmon/hwmon0", "class/hwmon/hwmon0/device"] {
            let fixture = Fixture::new();
            fixture.write(&format!("{directory}/name"), "vt1211");
            // temp1 is a thermal diode requiring board-specific offset/gain.
            fixture.write(&format!("{directory}/temp1_input"), "100000");
            fixture.write(&format!("{directory}/temp2_input"), "42000");
            fixture.write(&format!("{directory}/temp2_crit"), "100000");
            for channel in 3..=7 {
                // Upstream temp_from_reg(channel - 1, 148) returns 1100 mV.
                fixture.write(&format!("{directory}/temp{channel}_input"), "1100");
            }
            let mut warnings = Vec::new();
            let sensors = discover_sensors(&fixture.0, &mut warnings);
            assert_eq!(sensors.len(), 1);
            assert_eq!(sensors[0].label, "vt1211 · temp2");
            assert_eq!(sensors[0].celsius, 42.0);
            assert_eq!(sensors[0].critical_celsius, Some(100.0));
            assert_eq!(
                warnings,
                [
                    "6 VT1211 external sensor channel(s) require board-specific conversion and are omitted."
                ]
            );

            // Channel numbers alone must not suppress another driver's sensors.
            fixture.write(&format!("{directory}/name"), "other");
            warnings.clear();
            assert_eq!(discover_sensors(&fixture.0, &mut warnings).len(), 7);
            assert!(warnings.is_empty());
        }
    }

    #[test]
    fn temperature_vt1211_conversion_notice_remains_without_usable_channels() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        fixture.write("class/hwmon/hwmon0/name", "vt1211");
        fixture.write("class/hwmon/hwmon0/temp3_input", "1100");
        fixture.write("class/thermal/thermal_zone0/type", "vt1211");
        symlink(
            fixture.0.join("class/hwmon/hwmon0/temp3_input"),
            fixture.0.join("class/thermal/thermal_zone0/temp"),
        )
        .unwrap();
        let mut warnings = Vec::new();
        assert!(discover_sensors(&fixture.0, &mut warnings).is_empty());
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("require board-specific conversion"))
        );
    }

    #[test]
    fn temperature_unconverted_wm831x_and_vt8231_channels_preserve_calibrated_inputs() {
        for (driver, notice_name, channels) in
            [("wm831x", "WM831x", 2..=2), ("vt8231", "VT8231", 2..=6)]
        {
            for directory in ["class/hwmon/hwmon0", "class/hwmon/hwmon0/device"] {
                let fixture = Fixture::new();
                fixture.write(&format!("{directory}/name"), driver);
                fixture.write(&format!("{directory}/temp1_input"), "42000");
                fixture.write(&format!("{directory}/temp1_crit"), "100000");
                for channel in channels.clone() {
                    // WM831x reports battery thermistor millivolts; VT8231's
                    // external thermistors also require board-specific conversion.
                    fixture.write(&format!("{directory}/temp{channel}_input"), "1100");
                }
                let mut warnings = Vec::new();
                let sensors = discover_sensors(&fixture.0, &mut warnings);
                assert_eq!(sensors.len(), 1, "{driver} at {directory}");
                assert_eq!(sensors[0].label, format!("{driver} · temp1"));
                assert_eq!(sensors[0].celsius, 42.0);
                assert_eq!(sensors[0].critical_celsius, Some(100.0));
                assert_eq!(
                    warnings,
                    [format!(
                        "{} {notice_name} external sensor channel(s) require board-specific conversion and are omitted.",
                        channels.clone().count()
                    )]
                );

                // Identical channel numbers on ordinary drivers remain readings.
                fixture.write(&format!("{directory}/name"), "other");
                warnings.clear();
                assert_eq!(
                    discover_sensors(&fixture.0, &mut warnings).len(),
                    channels.clone().count() + 1
                );
                assert!(warnings.is_empty());
            }
        }
    }

    #[test]
    fn temperature_unconverted_channels_reject_aliases_and_keep_availability_notices() {
        use std::os::unix::fs::symlink;

        for (driver, notice_name) in [("wm831x", "WM831x"), ("vt8231", "VT8231")] {
            for (directory, alias) in [
                ("class/hwmon/hwmon0", "class/hwmon/hwmon0/device"),
                ("class/hwmon/hwmon0/device", "class/hwmon/hwmon0"),
            ] {
                let fixture = Fixture::new();
                fixture.write(&format!("{directory}/name"), driver);
                fixture.write(&format!("{directory}/temp2_input"), "1100");
                let input = fixture.0.join(format!("{directory}/temp2_input"));
                fs::create_dir_all(fixture.0.join(alias)).unwrap();
                symlink(&input, fixture.0.join(format!("{alias}/temp2_input"))).unwrap();
                fixture.write("class/thermal/thermal_zone0/type", driver);
                symlink(&input, fixture.0.join("class/thermal/thermal_zone0/temp")).unwrap();

                let mut warnings = Vec::new();
                assert!(discover_sensors(&fixture.0, &mut warnings).is_empty());
                assert_eq!(warnings.len(), 2);
                assert!(warnings[0].contains("Temperature sensors are unavailable"));
                assert_eq!(
                    warnings[1],
                    format!(
                        "1 {notice_name} external sensor channel(s) require board-specific conversion and are omitted."
                    )
                );
            }
        }
    }

    #[test]
    fn invalid_and_disabled_temperature_inputs_stay_missing() {
        let fixture = Fixture::new();
        fixture.write("class/hwmon/hwmon0/name", "test");
        fixture.write("class/hwmon/hwmon0/temp1_input", "-2147483648");
        fixture.write("class/hwmon/hwmon0/temp2_input", "35000");
        fixture.write("class/hwmon/hwmon0/temp2_enable", "0");
        fixture.write("class/hwmon/hwmon0/temp3_input", "NaN");
        let mut warnings = Vec::new();
        assert!(discover_sensors(&fixture.0, &mut warnings).is_empty());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn faulted_temperature_channels_are_missing_until_the_fault_clears() {
        for directory in ["class/hwmon/hwmon0", "class/hwmon/hwmon0/device"] {
            let fixture = Fixture::new();
            fixture.write("class/hwmon/hwmon0/name", "max6697");
            fixture.write(&format!("{directory}/temp1_input"), "42000");
            fixture.write(&format!("{directory}/temp2_input"), "43000");
            fixture.write(&format!("{directory}/temp2_fault"), "0");
            let mut warnings = Vec::new();
            assert_eq!(discover_sensors(&fixture.0, &mut warnings).len(), 2);
            assert!(warnings.is_empty());

            fixture.write(&format!("{directory}/temp2_input"), "127000");
            fixture.write(&format!("{directory}/temp2_fault"), "1");
            let sensors = discover_sensors(&fixture.0, &mut warnings);
            assert_eq!(sensors.len(), 1);
            assert_eq!(sensors[0].label, "max6697 · temp1");
            assert_eq!(sensors[0].celsius, 42.0);
            assert_eq!(
                warnings,
                ["1 temperature reading(s) were unavailable or invalid."]
            );

            fixture.write(&format!("{directory}/temp2_input"), "44000");
            fixture.write(&format!("{directory}/temp2_fault"), "0");
            warnings.clear();
            let sensors = discover_sensors(&fixture.0, &mut warnings);
            assert_eq!(sensors.len(), 2);
            assert_eq!(sensors[1].label, "max6697 · temp2");
            assert_eq!(sensors[1].celsius, 44.0);
            assert!(warnings.is_empty());
        }
    }

    #[test]
    fn peci_control_targets_are_excluded_but_measurements_and_limits_remain() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        fixture.write("class/hwmon/hwmon0/name", "peci_cputemp.cpu0");
        for (channel, label, value) in [
            (1, "Die", "40000"),
            (2, "DTS", "42000"),
            (3, "Tcontrol", "80000"),
            (4, "Tthrottle", "90000"),
            (5, "Tjmax", "100000"),
            (6, "Core 0", "41000"),
        ] {
            fixture.write(&format!("class/hwmon/hwmon0/temp{channel}_label"), label);
            fixture.write(&format!("class/hwmon/hwmon0/temp{channel}_input"), value);
        }
        fixture.write("class/hwmon/hwmon0/temp1_crit", "100000");
        // Other drivers use these channel numbers for actual measurements.
        fixture.write("class/hwmon/hwmon1/name", "coretemp");
        fixture.write("class/hwmon/hwmon1/temp3_label", "Core 1");
        fixture.write("class/hwmon/hwmon1/temp3_input", "43000");
        // A second path to Tjmax must not turn the control target into a sensor.
        fixture.write("class/thermal/thermal_zone0/type", "peci_cputemp.cpu0");
        symlink(
            fixture.0.join("class/hwmon/hwmon0/temp5_input"),
            fixture.0.join("class/thermal/thermal_zone0/temp"),
        )
        .unwrap();
        let mut warnings = Vec::new();
        let sensors = discover_sensors(&fixture.0, &mut warnings);
        assert_eq!(sensors.len(), 4);
        assert!(sensors.iter().all(|sensor| sensor.is_cpu));
        assert_eq!(
            sensors.iter().map(|sensor| sensor.celsius).reduce(f64::max),
            Some(43.0)
        );
        let die = sensors
            .iter()
            .find(|sensor| sensor.label == "peci_cputemp.cpu0 · Die")
            .unwrap();
        assert_eq!(die.celsius, 40.0);
        assert_eq!(die.critical_celsius, Some(100.0));
        assert!(warnings.is_empty());
    }

    #[test]
    fn peci_dimm_readings_never_supply_the_cpu_headline() {
        for directory in ["class/hwmon/hwmon0", "class/hwmon/hwmon0/device"] {
            for driver in ["peci_dimmtemp", "peci_dimmtemp.cpu0", "peci_dimmtemp.cpu12"] {
                let fixture = Fixture::new();
                fixture.write(&format!("{directory}/name"), driver);
                fixture.write(&format!("{directory}/temp1_label"), "DIMM A1");
                fixture.write(&format!("{directory}/temp1_input"), "80000");
                fixture.write(&format!("{directory}/temp1_crit"), "95000");
                let sample = || Snapshot {
                    sensors: discover_sensors(&fixture.0, &mut Vec::new()),
                    ..Snapshot::default()
                };

                let dimm_only = sample();
                assert_eq!(dimm_only.sensors.len(), 1, "{driver} at {directory}");
                assert!(!dimm_only.sensors[0].is_cpu, "{driver} at {directory}");
                assert_eq!(dimm_only.cpu_temperature(), None);
                assert_eq!(dimm_only.sensors[0].celsius, 80.0);
                assert_eq!(dimm_only.sensors[0].critical_celsius, Some(95.0));
                assert_eq!(dimm_only.sensors[0].label, format!("{driver} · DIMM A1"));

                // The source remains memory even with a CPU-labelled channel,
                // or when the optional channel label is unavailable.
                fixture.write(&format!("{directory}/temp1_label"), "CPU socket 0 DIMM A1");
                assert_eq!(sample().cpu_temperature(), None);
                fs::remove_file(fixture.0.join(format!("{directory}/temp1_label"))).unwrap();
                assert_eq!(sample().cpu_temperature(), None);

                fixture.write("class/hwmon/hwmon1/name", "peci_cputemp.cpu0");
                fixture.write("class/hwmon/hwmon1/temp1_label", "Die");
                fixture.write("class/hwmon/hwmon1/temp1_input", "40000");
                let mixed = sample();
                assert_eq!(mixed.sensors.len(), 2);
                assert_eq!(mixed.cpu_temperature(), Some(40.0));
                assert_eq!(
                    mixed.sensors.iter().filter(|sensor| sensor.is_cpu).count(),
                    1
                );
            }
        }
        for source in ["peci_dimmtemp", "peci_dimmtemp.cpu0"] {
            let fixture = Fixture::new();
            fixture.write("class/thermal/thermal_zone0/type", source);
            fixture.write("class/thermal/thermal_zone0/temp", "80000");
            let snapshot = Snapshot {
                sensors: discover_sensors(&fixture.0, &mut Vec::new()),
                ..Snapshot::default()
            };
            assert_eq!(snapshot.sensors.len(), 1);
            assert_eq!(snapshot.sensors[0].celsius, 80.0);
            assert!(!snapshot.sensors[0].is_cpu);
            assert_eq!(snapshot.cpu_temperature(), None);
        }
    }

    #[test]
    fn cpu_vrm_readings_remain_visible_without_supplying_the_cpu_headline() {
        for directory in ["class/hwmon/hwmon0", "class/hwmon/hwmon0/device"] {
            let fixture = Fixture::new();
            fixture.write(&format!("{directory}/name"), "asus_wmi_sensors");
            fixture.write(&format!("{directory}/temp1_label"), "CPU VRM Temperature");
            fixture.write(&format!("{directory}/temp1_input"), "85000");
            fixture.write(&format!("{directory}/temp1_crit"), "100000");
            let sample = || {
                let mut warnings = Vec::new();
                let snapshot = Snapshot {
                    sensors: discover_sensors(&fixture.0, &mut warnings),
                    ..Snapshot::default()
                };
                assert!(warnings.is_empty(), "{warnings:?}");
                snapshot
            };

            let vrm_only = sample();
            assert_eq!(vrm_only.sensors.len(), 1);
            let vrm = &vrm_only.sensors[0];
            assert_eq!(vrm.label, "asus_wmi_sensors · CPU VRM Temperature");
            assert_eq!(vrm.celsius, 85.0);
            assert_eq!(vrm.critical_celsius, Some(100.0));
            assert_eq!(vrm_only.cpu_temperature(), None, "{directory}");
            assert!(!vrm.is_cpu);

            fixture.write(&format!("{directory}/temp2_label"), "CPU Temperature");
            fixture.write(&format!("{directory}/temp2_input"), "40000");
            let mixed = sample();
            assert_eq!(mixed.sensors.len(), 2);
            assert_eq!(mixed.cpu_temperature(), Some(40.0), "{directory}");
            assert_eq!(
                mixed.sensors.iter().filter(|sensor| sensor.is_cpu).count(),
                1
            );
        }
    }

    #[test]
    fn cpu_vrm_source_names_do_not_supply_the_cpu_headline() {
        for source in ["cpu_vrm", "CPU-VRM"] {
            let fixture = Fixture::new();
            fixture.write("class/hwmon/hwmon0/name", source);
            fixture.write("class/hwmon/hwmon0/temp1_label", "CPU");
            fixture.write("class/hwmon/hwmon0/temp1_input", "85000");
            fixture.write("class/thermal/thermal_zone0/type", source);
            fixture.write("class/thermal/thermal_zone0/temp", "86000");
            let snapshot = Snapshot {
                sensors: discover_sensors(&fixture.0, &mut Vec::new()),
                ..Snapshot::default()
            };
            assert_eq!(snapshot.sensors.len(), 2);
            assert_eq!(snapshot.cpu_temperature(), None, "{source}");
            assert!(snapshot.sensors.iter().all(|sensor| !sensor.is_cpu));
        }
    }

    #[test]
    fn combined_pch_temperatures_remain_visible_without_supplying_the_cpu_headline() {
        for directory in ["class/hwmon/hwmon0", "class/hwmon/hwmon0/device"] {
            for label in ["PCH_CHIP_CPU_MAX_TEMP", "pch_chip_cpu_max_temp"] {
                let fixture = Fixture::new();
                fixture.write(&format!("{directory}/name"), "nct6796");
                fixture.write(&format!("{directory}/temp1_label"), label);
                fixture.write(&format!("{directory}/temp1_input"), "80000");
                fixture.write(&format!("{directory}/temp1_crit"), "95000");
                let sample = || {
                    let mut warnings = Vec::new();
                    let snapshot = Snapshot {
                        sensors: discover_sensors(&fixture.0, &mut warnings),
                        ..Snapshot::default()
                    };
                    assert!(warnings.is_empty(), "{warnings:?}");
                    snapshot
                };

                let composite_only = sample();
                assert_eq!(composite_only.sensors.len(), 1);
                let composite = &composite_only.sensors[0];
                assert_eq!(composite.label, format!("nct6796 · {label}"));
                assert_eq!(composite.celsius, 80.0);
                assert_eq!(composite.critical_celsius, Some(95.0));
                assert_eq!(
                    composite_only.cpu_temperature(),
                    None,
                    "{directory}: {label}"
                );
                assert!(!composite.is_cpu);

                fixture.write(&format!("{directory}/temp2_label"), "PCH_CPU_TEMP");
                fixture.write(&format!("{directory}/temp2_input"), "45000");
                fixture.write(&format!("{directory}/temp3_label"), "PCH_MCH_TEMP");
                fixture.write(&format!("{directory}/temp3_input"), "80000");
                fixture.write("class/hwmon/hwmon1/name", "coretemp");
                fixture.write("class/hwmon/hwmon1/temp1_label", "Package id 0");
                fixture.write("class/hwmon/hwmon1/temp1_input", "40000");
                let mixed = sample();
                assert_eq!(mixed.sensors.len(), 4);
                assert_eq!(mixed.cpu_temperature(), Some(45.0), "{directory}: {label}");
                assert_eq!(
                    mixed.sensors.iter().filter(|sensor| sensor.is_cpu).count(),
                    2
                );

                // Losing the PCH CPU reading must leave the package reading
                // authoritative even while the hotter composite remains.
                fs::remove_file(fixture.0.join(format!("{directory}/temp2_input"))).unwrap();
                assert_eq!(sample().cpu_temperature(), Some(40.0));
            }
        }
    }

    #[test]
    fn combined_pch_thermal_zones_do_not_supply_the_cpu_headline() {
        for source in ["PCH_CHIP_CPU_MAX_TEMP", "pch_chip_cpu_max_temp"] {
            let fixture = Fixture::new();
            fixture.write("class/thermal/thermal_zone0/type", source);
            fixture.write("class/thermal/thermal_zone0/temp", "80000");
            fixture.write("class/thermal/thermal_zone0/trip_point_0_type", "critical");
            fixture.write("class/thermal/thermal_zone0/trip_point_0_temp", "95000");
            let mut warnings = Vec::new();
            let snapshot = Snapshot {
                sensors: discover_sensors(&fixture.0, &mut warnings),
                ..Snapshot::default()
            };
            assert!(warnings.is_empty(), "{warnings:?}");
            assert_eq!(snapshot.sensors.len(), 1);
            assert_eq!(snapshot.sensors[0].label, source);
            assert_eq!(snapshot.sensors[0].celsius, 80.0);
            assert_eq!(snapshot.sensors[0].critical_celsius, Some(95.0));
            assert!(!snapshot.sensors[0].is_cpu, "{source}");
            assert_eq!(snapshot.cpu_temperature(), None, "{source}");
        }
    }

    #[test]
    fn cpu_sensor_driver_and_channel_recognition_is_retained() {
        for (driver, label) in [
            ("coretemp", "Package id 0"),
            ("k10temp", "Tdie"),
            ("k8temp", "temp1"),
            ("zenpower", "Tctl"),
            ("peci_cputemp", "Die"),
            ("peci_cputemp.cpu12", "Die"),
            ("board_sensor", "CPU"),
            ("board_sensor", "CPU_MAX_TEMP"),
        ] {
            let fixture = Fixture::new();
            fixture.write("class/hwmon/hwmon0/name", driver);
            fixture.write("class/hwmon/hwmon0/temp1_label", label);
            fixture.write("class/hwmon/hwmon0/temp1_input", "42000");
            let snapshot = Snapshot {
                sensors: discover_sensors(&fixture.0, &mut Vec::new()),
                ..Snapshot::default()
            };
            assert_eq!(snapshot.cpu_temperature(), Some(42.0), "{driver}: {label}");
        }
        for zone_type in ["cpu-thermal", "x86_pkg_temp", "soc_thermal"] {
            let fixture = Fixture::new();
            fixture.write("class/thermal/thermal_zone0/type", zone_type);
            fixture.write("class/thermal/thermal_zone0/temp", "43000");
            let snapshot = Snapshot {
                sensors: discover_sensors(&fixture.0, &mut Vec::new()),
                ..Snapshot::default()
            };
            assert_eq!(snapshot.cpu_temperature(), Some(43.0), "{zone_type}");
        }
    }

    #[test]
    fn it87_disabled_sensor_types_are_omitted_and_reenable_recovers() {
        for directory in ["class/hwmon/hwmon0", "class/hwmon/hwmon0/device"] {
            for driver in ["it87", "it8728", "it8689", "it8603", "it87952"] {
                let fixture = Fixture::new();
                fixture.write(&format!("{directory}/name"), driver);
                // A disabled input can remain readable and look plausible.
                fixture.write(&format!("{directory}/temp1_input"), "127000");
                let sample = || discover_sensors(&fixture.0, &mut Vec::new());

                // The type attribute is optional; its absence is not disablement.
                assert_eq!(sample().len(), 1, "{driver} at {directory}");
                let type_path = fixture.0.join(format!("{directory}/temp1_type"));
                fs::create_dir(&type_path).unwrap();
                assert_eq!(sample().len(), 1, "unreadable type at {directory}");
                fs::remove_dir(&type_path).unwrap();
                fixture.write(&format!("{directory}/temp1_type"), "0");
                assert!(sample().is_empty(), "{driver} at {directory}");
                // Zero does not have a generic disablement meaning in hwmon.
                fixture.write(&format!("{directory}/name"), "other");
                assert_eq!(sample().len(), 1);
                fixture.write(&format!("{directory}/name"), driver);
                for enabled_type in ["3", "4"] {
                    fixture.write(&format!("{directory}/temp1_type"), enabled_type);
                    let sensors = sample();
                    assert_eq!(sensors.len(), 1, "{driver} at {directory}");
                    assert_eq!(sensors[0].celsius, 127.0);
                }
                fs::remove_file(fixture.0.join(format!("{directory}/temp1_type"))).unwrap();
                assert_eq!(sample().len(), 1);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn disabled_sensor_types_reject_canonical_aliases_even_when_read_later() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        fixture.write("class/hwmon/hwmon0/name", "it8728");
        fixture.write("class/hwmon/hwmon0/temp1_input", "127000");
        fixture.write("class/hwmon/hwmon0/device/temp1_type", "0");
        let input = fixture.0.join("class/hwmon/hwmon0/temp1_input");
        symlink(
            &input,
            fixture.0.join("class/hwmon/hwmon0/device/temp1_input"),
        )
        .unwrap();
        fixture.write("class/thermal/thermal_zone0/type", "it8728");
        symlink(&input, fixture.0.join("class/thermal/thermal_zone0/temp")).unwrap();
        let sample = || discover_sensors(&fixture.0, &mut Vec::new());

        // The modern view accepts the input before its legacy alias rejects it.
        assert!(sample().is_empty());
        fixture.write("class/hwmon/hwmon0/device/temp1_type", "4");
        assert_eq!(sample().len(), 1);
        // Rejection found in the first view must also survive later aliases.
        fixture.write("class/hwmon/hwmon0/temp1_type", "0");
        assert!(sample().is_empty());
        fs::remove_file(fixture.0.join("class/hwmon/hwmon0/temp1_type")).unwrap();
        assert_eq!(sample().len(), 1);
    }
}
