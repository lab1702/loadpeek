//! Read-only process telemetry from Linux procfs.
//!
//! Field definitions: https://docs.kernel.org/filesystems/proc.html.
//! CPU percentages follow top's usual convention: 100% is one logical CPU.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

const MAX_PROC_FILE_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Default)]
pub struct ProcessSnapshot {
    pub processes: Vec<Process>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Process {
    pub pid: u32,
    pub ppid: u32,
    pub uid: Option<u32>,
    pub user: String,
    pub state: char,
    pub priority: i64,
    pub nice: i64,
    pub threads: u64,
    pub virtual_bytes: Option<u64>,
    pub resident_bytes: Option<u64>,
    pub shared_bytes: Option<u64>,
    /// 100% means one logical CPU; multithreaded processes can exceed 100%.
    pub cpu_percent: Option<f64>,
    /// Resident memory as a percentage of physical system memory.
    pub memory_percent: Option<f64>,
    /// User plus system CPU time, excluding reaped children.
    pub cpu_time_secs: f64,
    pub elapsed_secs: Option<f64>,
    pub name: String,
    pub command: String,
    /// Together with PID, identifies a process across refreshes and PID reuse.
    pub start_time_ticks: u64,
}

#[derive(Clone, Copy, Debug)]
struct PreviousProcess {
    start_time_ticks: u64,
    cpu_ticks: u64,
    sampled_at: Instant,
}

#[derive(Debug)]
pub struct ProcessCollector {
    proc_root: PathBuf,
    ticks_per_second: Option<f64>,
    page_bytes: Option<u64>,
    users: HashMap<u32, String>,
    previous: HashMap<u32, PreviousProcess>,
}

impl Default for ProcessCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessCollector {
    pub fn new() -> Self {
        // SAFETY: sysconf takes only a numeric selector and retains no pointers.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        // SAFETY: sysconf takes only a numeric selector and retains no pointers.
        let page_bytes = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        Self {
            proc_root: PathBuf::from("/proc"),
            ticks_per_second: (ticks > 0).then_some(ticks as f64),
            page_bytes: u64::try_from(page_bytes).ok().filter(|value| *value > 0),
            // Avoid NSS lookups, which can block on directory/network services.
            users: fs::read("/etc/passwd")
                .map(|bytes| parse_users(&String::from_utf8_lossy(&bytes)))
                .unwrap_or_default(),
            previous: HashMap::new(),
        }
    }

    pub fn sample(&mut self, total_memory_bytes: u64) -> ProcessSnapshot {
        self.sample_with_clock(total_memory_bytes, Instant::now)
    }

    fn sample_with_clock(
        &mut self,
        total_memory_bytes: u64,
        mut clock: impl FnMut() -> Instant,
    ) -> ProcessSnapshot {
        let mut snapshot = ProcessSnapshot::default();
        let uptime = read_limited(&self.proc_root.join("uptime"))
            .ok()
            .and_then(|bytes| {
                String::from_utf8_lossy(&bytes)
                    .split_whitespace()
                    .next()?
                    .parse::<f64>()
                    .ok()
            })
            .filter(|value| value.is_finite() && *value >= 0.0);
        if self.ticks_per_second.is_none() {
            snapshot
                .warnings
                .push("The system clock tick rate is unavailable; process CPU times and rates cannot be measured.".into());
        }
        if self.page_bytes.is_none() {
            snapshot.warnings.push(
                "The system page size is unavailable; process resident and shared memory cannot be measured.".into(),
            );
        }
        if uptime.is_none() {
            snapshot.warnings.push(
                "System uptime is unavailable; process elapsed times cannot be measured.".into(),
            );
        }
        let entries = match fs::read_dir(&self.proc_root) {
            Ok(entries) => entries,
            Err(_) => {
                self.previous.clear();
                snapshot.warnings.push(
                    "Processes are unavailable because the process directory cannot be read."
                        .into(),
                );
                return snapshot;
            }
        };
        let mut previous = HashMap::with_capacity(self.previous.len());
        let mut unreadable = 0usize;
        let mut incomplete = 0usize;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    unreadable += 1;
                    continue;
                }
            };
            let filename = entry.file_name();
            let Some(pid) = filename.to_str().and_then(parse_pid_directory) else {
                continue;
            };
            let path = entry.path();
            let initial = match read_stat(&path.join("stat"), pid) {
                Ok(stat) => stat,
                Err(error) if process_disappeared(&error) => continue,
                Err(_) => {
                    unreadable += 1;
                    continue;
                }
            };
            let status = read_limited(&path.join("status"));
            let statm = read_limited(&path.join("statm"));
            let cmdline = read_limited(&path.join("cmdline"));
            // Recheck identity after the other reads: a process may exit and its
            // PID may be reused during the scan. Do not combine two processes.
            let stat = match read_stat(&path.join("stat"), pid) {
                Ok(stat) if stat.start_time_ticks == initial.start_time_ticks => stat,
                Ok(_) => continue,
                Err(error) if process_disappeared(&error) => continue,
                Err(_) => {
                    unreadable += 1;
                    continue;
                }
            };
            let sampled_at = clock();
            let uid = status
                .as_ref()
                .ok()
                .and_then(|bytes| effective_uid(&String::from_utf8_lossy(bytes)));
            let shared_pages = statm.as_ref().ok().and_then(|bytes| {
                String::from_utf8_lossy(bytes)
                    .split_whitespace()
                    .nth(2)?
                    .parse::<u64>()
                    .ok()
            });
            let resident_bytes = stat.resident_pages.and_then(|pages| {
                self.page_bytes
                    .and_then(|page_bytes| pages.checked_mul(page_bytes))
            });
            let shared_bytes = shared_pages.and_then(|pages| {
                self.page_bytes
                    .and_then(|page_bytes| pages.checked_mul(page_bytes))
            });
            if uid.is_none()
                || shared_pages.is_none()
                || cmdline.is_err()
                || stat.virtual_bytes.is_none()
                || stat.resident_pages.is_none()
                || (self.page_bytes.is_some()
                    && (resident_bytes.is_none() || shared_bytes.is_none()))
            {
                incomplete += 1;
            }
            let cpu_percent = self
                .previous
                .get(&pid)
                .and_then(|old| cpu_percent(*old, &stat, sampled_at, self.ticks_per_second?));
            previous.insert(
                pid,
                PreviousProcess {
                    start_time_ticks: stat.start_time_ticks,
                    cpu_ticks: stat.cpu_ticks,
                    sampled_at,
                },
            );
            let name = sanitize(&stat.name);
            let command = format_command(cmdline.as_deref().ok(), &name);
            snapshot.processes.push(Process {
                pid,
                ppid: stat.ppid,
                uid,
                user: uid
                    .map(|uid| {
                        self.users
                            .get(&uid)
                            .cloned()
                            .unwrap_or_else(|| uid.to_string())
                    })
                    .unwrap_or_else(|| "Unavailable".into()),
                state: stat.state,
                priority: stat.priority,
                nice: stat.nice,
                threads: stat.threads,
                virtual_bytes: stat.virtual_bytes,
                resident_bytes,
                shared_bytes,
                cpu_percent,
                memory_percent: resident_bytes.and_then(|bytes| {
                    (total_memory_bytes > 0)
                        .then(|| bytes as f64 / total_memory_bytes as f64 * 100.0)
                }),
                cpu_time_secs: self
                    .ticks_per_second
                    .map(|ticks| stat.cpu_ticks as f64 / ticks)
                    .unwrap_or(f64::NAN),
                elapsed_secs: uptime.and_then(|uptime| {
                    let elapsed = uptime - stat.start_time_ticks as f64 / self.ticks_per_second?;
                    // A process may start just after uptime was read this scan.
                    Some(elapsed.max(0.0))
                }),
                name,
                command,
                start_time_ticks: stat.start_time_ticks,
            });
        }
        // Replacing this map discards exited processes and bounds state by the
        // currently readable process count rather than the lifetime PID count.
        self.previous = previous;
        if unreadable > 0 {
            snapshot.warnings.push(format!(
                "{unreadable} process entries could not be read. Permissions or procfs restrictions may limit the list."
            ));
        }
        if incomplete > 0 {
            snapshot.warnings.push(format!(
                "{incomplete} processes have incomplete details; unavailable values are shown as unavailable."
            ));
        }
        snapshot
    }
}

#[derive(Debug)]
struct ProcessStat {
    name: String,
    state: char,
    ppid: u32,
    cpu_ticks: u64,
    priority: i64,
    nice: i64,
    threads: u64,
    start_time_ticks: u64,
    virtual_bytes: Option<u64>,
    resident_pages: Option<u64>,
}

fn read_stat(path: &Path, expected_pid: u32) -> io::Result<ProcessStat> {
    let bytes = read_limited(path)?;
    parse_stat(&String::from_utf8_lossy(&bytes), expected_pid)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid process stat"))
}

fn parse_stat(text: &str, expected_pid: u32) -> Option<ProcessStat> {
    let start = text.find('(')?;
    let end = text.rfind(')')?;
    if end <= start || text[..start].trim().parse::<u32>().ok()? != expected_pid {
        return None;
    }
    // comm is parenthesized but may itself contain spaces, newlines, and ')'.
    // All following fields are numeric except state, so the last ')' delimits it.
    let fields: Vec<_> = text[end + 1..].split_whitespace().collect();
    let mut state = fields.first()?.chars();
    let state_char = state.next()?;
    if !state_char.is_ascii_alphabetic() || state.next().is_some() {
        return None;
    }
    Some(ProcessStat {
        name: text[start + 1..end].to_owned(),
        state: state_char,
        ppid: fields.get(1)?.parse().ok()?,
        // utime includes guest time already. Do not add guest or child times.
        cpu_ticks: fields
            .get(11)?
            .parse::<u64>()
            .ok()?
            .checked_add(fields.get(12)?.parse::<u64>().ok()?)?,
        priority: fields.get(15)?.parse().ok()?,
        nice: fields.get(16)?.parse().ok()?,
        threads: fields.get(17)?.parse().ok()?,
        start_time_ticks: fields.get(19)?.parse().ok()?,
        virtual_bytes: fields.get(20)?.parse().ok(),
        resident_pages: fields
            .get(21)?
            .parse::<i64>()
            .ok()
            .and_then(|pages| u64::try_from(pages).ok()),
    })
}

fn cpu_percent(
    previous: PreviousProcess,
    current: &ProcessStat,
    sampled_at: Instant,
    ticks_per_second: f64,
) -> Option<f64> {
    if previous.start_time_ticks != current.start_time_ticks
        || !ticks_per_second.is_finite()
        || ticks_per_second <= 0.0
    {
        return None;
    }
    let elapsed = sampled_at
        .checked_duration_since(previous.sampled_at)?
        .as_secs_f64();
    if elapsed <= 0.0 {
        return None;
    }
    let ticks = current.cpu_ticks.checked_sub(previous.cpu_ticks)?;
    let percent = ticks as f64 / ticks_per_second / elapsed * 100.0;
    percent.is_finite().then_some(percent)
}

fn effective_uid(status: &str) -> Option<u32> {
    status.lines().find_map(|line| {
        line.strip_prefix("Uid:")?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    })
}

fn parse_users(passwd: &str) -> HashMap<u32, String> {
    let mut users = HashMap::new();
    for line in passwd.lines() {
        let mut fields = line.split(':');
        let Some(name) = fields.next().filter(|name| !name.is_empty()) else {
            continue;
        };
        let Some(uid) = fields.nth(1).and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        users.entry(uid).or_insert_with(|| sanitize(name));
    }
    users
}

fn parse_pid_directory(name: &str) -> Option<u32> {
    if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    name.parse::<u32>().ok().filter(|pid| *pid > 0)
}

fn read_limited(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_PROC_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn process_disappeared(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
}

fn sanitize(text: &str) -> String {
    text.chars()
        .map(|character| match character {
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => '\u{fffd}',
            '\u{2028}' | '\u{2029}' => ' ',
            character if character.is_control() => ' ',
            character => character,
        })
        .collect()
}

fn format_command(bytes: Option<&[u8]>, name: &str) -> String {
    let Some(bytes) = bytes.filter(|bytes| bytes.iter().any(|byte| *byte != 0)) else {
        return format!("[{name}]");
    };
    let truncated = bytes.len() > MAX_PROC_FILE_BYTES as usize;
    let bytes = &bytes[..bytes.len().min(MAX_PROC_FILE_BYTES as usize)];
    let mut args: Vec<_> = bytes.split(|byte| *byte == 0).collect();
    while args.last().is_some_and(|arg| arg.is_empty()) {
        args.pop();
    }
    let mut command = args
        .iter()
        .map(|arg| {
            if arg.is_empty() {
                "\"\"".into()
            } else {
                sanitize(&String::from_utf8_lossy(arg))
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if truncated {
        command.push_str(" …");
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let serial = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "loadpeek-process-test-{}-{serial}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("uptime"), "1000.00 1234.00\n").unwrap();
            Self(path)
        }

        fn process(&self, pid: u32, cpu_ticks: u64, start_ticks: u64) {
            let path = self.0.join(pid.to_string());
            fs::create_dir_all(&path).unwrap();
            fs::write(
                path.join("stat"),
                stat_line(pid, "worker ) test", cpu_ticks, start_ticks),
            )
            .unwrap();
            fs::write(path.join("status"), "Uid:\t1000\t1001\t1002\t1003\n").unwrap();
            fs::write(path.join("statm"), "2000 25 10 0 0 0 0\n").unwrap();
            fs::write(path.join("cmdline"), b"/bin/worker\0--test\0").unwrap();
        }

        fn collector(&self) -> ProcessCollector {
            ProcessCollector {
                proc_root: self.0.clone(),
                ticks_per_second: Some(250.0),
                page_bytes: Some(16_384),
                users: HashMap::from([(1001, "effective-user".into())]),
                previous: HashMap::new(),
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn stat_line(pid: u32, name: &str, cpu_ticks: u64, start_ticks: u64) -> String {
        // Fields 3..24, including large child CPU times to catch double counting.
        format!(
            "{pid} ({name}) S 7 0 0 0 0 0 0 0 0 0 {} 5 9999 8888 -5 -10 4 0 {start_ticks} 1048576 25\n",
            cpu_ticks - 5
        )
    }

    #[test]
    fn stat_handles_parentheses_spaces_and_newlines_in_comm() {
        let text = stat_line(42, "odd ) (name\n)", 255, 500);
        let stat = parse_stat(&text, 42).unwrap();
        assert_eq!(stat.name, "odd ) (name\n)");
        assert_eq!(stat.state, 'S');
        assert_eq!(stat.ppid, 7);
        assert_eq!(stat.cpu_ticks, 255);
        assert_eq!(stat.priority, -5);
        assert_eq!(stat.nice, -10);
        assert_eq!(stat.threads, 4);
        assert_eq!(stat.start_time_ticks, 500);
        assert_eq!(stat.virtual_bytes, Some(1_048_576));
        assert_eq!(stat.resident_pages, Some(25));
        assert!(parse_stat(&text, 99).is_none());
        assert!(parse_stat("42 (cut off) R 1", 42).is_none());
        assert!(parse_stat("42 no parentheses", 42).is_none());
    }

    #[test]
    fn cpu_uses_real_elapsed_time_and_does_not_clamp_multicore_usage() {
        let now = Instant::now();
        let previous = PreviousProcess {
            start_time_ticks: 500,
            cpu_ticks: 255,
            sampled_at: now,
        };
        let stat = parse_stat(&stat_line(42, "worker", 505, 500), 42).unwrap();
        assert_eq!(
            cpu_percent(previous, &stat, now + Duration::from_millis(500), 250.0),
            Some(200.0)
        );
        assert_eq!(
            cpu_percent(previous, &stat, now + Duration::from_secs(5), 250.0),
            Some(20.0)
        );
        assert!(cpu_percent(previous, &stat, now, 250.0).is_none());
        assert!(cpu_percent(previous, &stat, now + Duration::from_secs(1), 0.0).is_none());
    }

    #[test]
    fn cpu_resets_baseline_for_reused_pids_and_decreasing_counters() {
        let now = Instant::now();
        let previous = PreviousProcess {
            start_time_ticks: 500,
            cpu_ticks: 255,
            sampled_at: now,
        };
        let reused = parse_stat(&stat_line(42, "new", 555, 600), 42).unwrap();
        let reset = parse_stat(&stat_line(42, "old", 105, 500), 42).unwrap();
        assert!(cpu_percent(previous, &reused, now + Duration::from_secs(1), 250.0).is_none());
        assert!(cpu_percent(previous, &reset, now + Duration::from_secs(1), 250.0).is_none());
    }

    #[test]
    fn sample_uses_kernel_units_effective_uid_and_physical_memory() {
        let fixture = Fixture::new();
        fixture.process(42, 255, 500);
        let mut collector = fixture.collector();
        let first = collector.sample_with_clock(1_638_400, Instant::now);
        assert!(first.warnings.is_empty());
        let process = &first.processes[0];
        assert_eq!(process.pid, 42);
        assert_eq!(process.uid, Some(1001));
        assert_eq!(process.user, "effective-user");
        assert_eq!(process.resident_bytes, Some(409_600));
        assert_eq!(process.shared_bytes, Some(163_840));
        assert_eq!(process.virtual_bytes, Some(1_048_576));
        assert_eq!(process.memory_percent, Some(25.0));
        assert_eq!(process.cpu_time_secs, 1.02);
        assert_eq!(process.elapsed_secs, Some(998.0));
        assert_eq!(process.cpu_percent, None);
        assert_eq!(process.command, "/bin/worker --test");
    }

    #[test]
    fn sample_cleans_up_exits_and_treats_reused_pids_as_new() {
        let fixture = Fixture::new();
        fixture.process(42, 255, 500);
        let mut collector = fixture.collector();
        let now = Instant::now();
        collector.sample_with_clock(1024, || now);
        fixture.process(42, 505, 500);
        let second = collector.sample_with_clock(1024, || now + Duration::from_secs(1));
        assert_eq!(second.processes[0].cpu_percent, Some(100.0));
        fixture.process(42, 755, 600);
        let reused = collector.sample_with_clock(1024, || now + Duration::from_secs(2));
        assert_eq!(reused.processes[0].cpu_percent, None);
        fs::remove_dir_all(fixture.0.join("42")).unwrap();
        // A directory can still be in readdir while its stat has disappeared.
        fs::create_dir(fixture.0.join("43")).unwrap();
        let exited = collector.sample_with_clock(1024, || now + Duration::from_secs(3));
        assert!(exited.processes.is_empty());
        assert!(exited.warnings.is_empty());
        assert!(collector.previous.is_empty());
    }

    #[test]
    fn live_process_with_missing_optional_files_stays_visible_with_one_warning() {
        let fixture = Fixture::new();
        fixture.process(42, 255, 500);
        for filename in ["status", "statm", "cmdline"] {
            fs::remove_file(fixture.0.join("42").join(filename)).unwrap();
        }
        let snapshot = fixture.collector().sample(0);
        assert_eq!(snapshot.processes.len(), 1);
        assert_eq!(snapshot.warnings.len(), 1);
        assert!(snapshot.warnings[0].contains("1 processes have incomplete details"));
        let process = &snapshot.processes[0];
        assert_eq!(process.uid, None);
        assert_eq!(process.shared_bytes, None);
        assert_eq!(process.memory_percent, None);
        assert_eq!(process.command, "[worker ) test]");
    }

    #[test]
    fn exit_or_pid_reuse_between_proc_file_reads_does_not_mix_processes() {
        use std::ffi::CString;
        use std::io::Write;
        use std::os::unix::ffi::OsStrExt;

        for reused in [false, true] {
            let fixture = Fixture::new();
            fixture.process(42, 255, 500);
            let mut collector = fixture.collector();
            collector.sample(1024);
            let path = fixture.0.join("42");
            fs::remove_file(path.join("statm")).unwrap();
            let fifo = CString::new(path.join("statm").as_os_str().as_bytes()).unwrap();
            // SAFETY: the CString is NUL-terminated and lives through this call.
            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
            let writer = std::thread::spawn(move || {
                // Opening the FIFO rendezvous with the statm read, after the
                // collector's first stat read but before its identity recheck.
                let mut fifo = File::create(path.join("statm")).unwrap();
                if reused {
                    fs::write(path.join("stat"), stat_line(42, "replacement", 755, 600)).unwrap();
                } else {
                    fs::remove_dir_all(path).unwrap();
                }
                fifo.write_all(b"2000 25 10 0 0 0 0\n").unwrap();
            });
            let snapshot = collector.sample(1024);
            writer.join().unwrap();
            assert!(snapshot.processes.is_empty());
            assert!(snapshot.warnings.is_empty());
            assert!(collector.previous.is_empty());
        }
    }

    #[test]
    fn local_passwd_lookup_uses_numeric_fallback() {
        let users =
            parse_users("root:x:0:0:root:/root:/bin/sh\nalice:x:1001:1001::/:/bin/sh\ninvalid\n");
        assert_eq!(users.get(&1001).map(String::as_str), Some("alice"));
        assert_eq!(users.len(), 2);
        let fixture = Fixture::new();
        fixture.process(42, 255, 500);
        let mut collector = fixture.collector();
        collector.users.clear();
        assert_eq!(collector.sample(0).processes[0].user, "1001");
    }

    #[test]
    fn command_fallback_and_sanitization_are_safe_for_display() {
        assert_eq!(format_command(Some(b""), "kworker"), "[kworker]");
        assert_eq!(format_command(None, "gone"), "[gone]");
        assert_eq!(format_command(Some(b"\0\0"), "zombie"), "[zombie]");
        assert_eq!(
            format_command(Some(b"tool\0\0a\nb\t\x1b\0\xff\0"), "tool"),
            "tool \"\" a b   \u{fffd}"
        );
        assert_eq!(sanitize("a\u{202e}b\u{2028}c"), "a\u{fffd}b c");
        let long = vec![b'x'; MAX_PROC_FILE_BYTES as usize + 1];
        let command = format_command(Some(&long), "long");
        assert!(command.ends_with(" …"));
        assert_eq!(command.chars().count(), MAX_PROC_FILE_BYTES as usize + 2);
    }

    #[test]
    fn bad_units_and_malformed_stat_do_not_invent_measurements() {
        let fixture = Fixture::new();
        fixture.process(42, 255, 500);
        let mut collector = fixture.collector();
        collector.ticks_per_second = None;
        collector.page_bytes = None;
        let snapshot = collector.sample(1_000_000);
        assert_eq!(snapshot.warnings.len(), 2);
        let process = &snapshot.processes[0];
        assert_eq!(process.resident_bytes, None);
        assert_eq!(process.memory_percent, None);
        assert_eq!(process.elapsed_secs, None);
        assert!(process.cpu_time_secs.is_nan());
        fs::write(fixture.0.join("42/stat"), "malformed").unwrap();
        let malformed = collector.sample(1_000_000);
        assert!(malformed.processes.is_empty());
        assert!(
            malformed
                .warnings
                .iter()
                .any(|warning| warning.contains("could not be read"))
        );
        assert!(collector.previous.is_empty());
    }

    #[test]
    fn pid_directories_are_strictly_numeric() {
        assert_eq!(parse_pid_directory("42"), Some(42));
        for name in ["", "0", "self", "thread-self", "+42", "-42", "42junk"] {
            assert_eq!(parse_pid_directory(name), None);
        }
    }
}
