use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs, io,
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub struct SystemStats {
    pub cpu: Option<f64>,
    pub cores: Vec<f64>,
    pub memory_used: u64,
    pub memory_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub network_rx_per_sec: Option<f64>,
    pub network_tx_per_sec: Option<f64>,
    pub disk_read_per_sec: Option<f64>,
    pub disk_write_per_sec: Option<f64>,
    pub load: String,
    pub cpu_history: VecDeque<u64>,
    pub error: Option<String>,
}

#[derive(Clone, Copy)]
struct CpuTicks {
    total: u64,
    idle: u64,
}

// Keep each device's identity and counters so topology changes and individual
// counter resets cannot be mistaken for traffic on the previous device set.
type DeviceCounters = BTreeMap<String, (u64, u64)>;

#[derive(Default)]
struct IoCounters {
    network: Option<DeviceCounters>,
    disk: Option<DeviceCounters>,
}

const BLOCK_DEVICE_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct SystemSampler {
    previous: Vec<CpuTicks>,
    previous_io: Option<(IoCounters, Instant)>,
    cached_devices: Option<(HashSet<String>, Instant)>,
    pub stats: SystemStats,
}

impl SystemSampler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sample(&mut self) {
        match self.read() {
            Ok(()) => self.stats.error = None,
            Err(_) => self.stats.error = Some("系统指标读取失败".into()),
        }
    }

    fn read(&mut self) -> io::Result<()> {
        let current = cpu_ticks(&fs::read_to_string("/proc/stat")?);
        let values: Vec<f64> = current
            .iter()
            .zip(&self.previous)
            .map(|(cur, prev)| {
                let total = cur.total.saturating_sub(prev.total);
                let idle = cur.idle.saturating_sub(prev.idle);
                if total == 0 {
                    0.0
                } else {
                    (total.saturating_sub(idle)) as f64 * 100.0 / total as f64
                }
            })
            .collect();
        self.stats.cpu = values.first().copied();
        self.stats.cores = values.into_iter().skip(1).collect();
        if let Some(cpu) = self.stats.cpu {
            self.stats.cpu_history.push_back(cpu.round() as u64);
            if self.stats.cpu_history.len() > 120 {
                self.stats.cpu_history.pop_front();
            }
        }
        self.previous = current;
        let (used, total, swap_used, swap_total) = memory(&fs::read_to_string("/proc/meminfo")?);
        self.stats.memory_used = used;
        self.stats.memory_total = total;
        self.stats.swap_used = swap_used;
        self.stats.swap_total = swap_total;
        self.stats.load = fs::read_to_string("/proc/loadavg")?
            .split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join("  ");
        self.sample_io();
        Ok(())
    }

    fn sample_io(&mut self) {
        let now = Instant::now();
        let is_fresh = self.cached_devices.as_ref().is_some_and(|(_, sampled_at)| {
            now.duration_since(*sampled_at) < BLOCK_DEVICE_CACHE_TTL
        });
        if !is_fresh {
            let devices = top_level_block_devices("/sys/block");
            self.cached_devices = Some((devices, now));
        }
        let devices = &self
            .cached_devices
            .as_ref()
            .expect("cached devices populated")
            .0;
        self.sample_io_at(io_counters(devices), now);
    }

    fn sample_io_at(&mut self, current: IoCounters, now: Instant) {
        let (network, disk) = self.previous_io.as_ref().map_or(
            ((None, None), (None, None)),
            |(previous, sampled_at)| {
                let seconds = now.duration_since(*sampled_at).as_secs_f64();
                (
                    device_rates(current.network.as_ref(), previous.network.as_ref(), seconds),
                    device_rates(current.disk.as_ref(), previous.disk.as_ref(), seconds),
                )
            },
        );
        (self.stats.network_rx_per_sec, self.stats.network_tx_per_sec) = network;
        (self.stats.disk_read_per_sec, self.stats.disk_write_per_sec) = disk;
        self.previous_io = Some((current, now));
    }
}

fn device_rates(
    current: Option<&DeviceCounters>,
    previous: Option<&DeviceCounters>,
    seconds: f64,
) -> (Option<f64>, Option<f64>) {
    let rates = current.zip(previous).and_then(|(current, previous)| {
        if current.is_empty() || !current.keys().eq(previous.keys()) {
            return None;
        }
        current
            .iter()
            .try_fold((0.0, 0.0), |(read, write), (name, counters)| {
                let prev = previous.get(name)?;
                Some((
                    read + counter_rate(counters.0, prev.0, seconds)?,
                    write + counter_rate(counters.1, prev.1, seconds)?,
                ))
            })
    });
    rates.map_or((None, None), |(read, write)| (Some(read), Some(write)))
}

fn io_counters(devices: &HashSet<String>) -> IoCounters {
    let route = fs::read_to_string("/proc/net/route").unwrap_or_default();
    let network = fs::read_to_string("/proc/net/dev")
        .ok()
        .and_then(|text| network_counters(&text, default_interface(&route).as_deref()));
    let disk = fs::read_to_string("/proc/diskstats")
        .ok()
        .and_then(|text| disk_counters(&text, devices));
    IoCounters { network, disk }
}

fn default_interface(route: &str) -> Option<String> {
    route.lines().skip(1).find_map(|line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let flags = u16::from_str_radix(fields.get(3)?, 16).ok()?;
        (fields.get(1) == Some(&"00000000") && flags & 1 == 1).then(|| fields[0].to_owned())
    })
}

fn network_counters(text: &str, interface: Option<&str>) -> Option<DeviceCounters> {
    let mut counters = DeviceCounters::new();
    for line in text.lines().skip(2) {
        let Some((name, values)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if interface.is_some_and(|selected| name != selected)
            || (interface.is_none() && name == "lo")
        {
            continue;
        }
        let fields = values.split_whitespace().collect::<Vec<_>>();
        let Some(rx) = fields.first().and_then(|f| f.parse::<u64>().ok()) else {
            continue;
        };
        let Some(tx) = fields.get(8).and_then(|f| f.parse::<u64>().ok()) else {
            continue;
        };
        counters.insert(name.to_owned(), (rx, tx));
    }
    (!counters.is_empty()).then_some(counters)
}

fn top_level_block_devices(path: &str) -> HashSet<String> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("loop") || name.starts_with("ram") || name.starts_with("zram") {
                return None;
            }
            let has_holder = fs::read_dir(entry.path().join("holders"))
                .ok()
                .and_then(|mut entries| entries.next())
                .is_some();
            (!has_holder).then_some(name)
        })
        .collect()
}

fn disk_counters(text: &str, devices: &HashSet<String>) -> Option<DeviceCounters> {
    let mut counters = DeviceCounters::new();
    for line in text.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let Some(name) = fields.get(2).copied() else {
            continue;
        };
        if !devices.contains(name) {
            continue;
        }
        // Linux diskstats sectors are defined as 512 bytes for these fields.
        let Some(read) = fields
            .get(5)
            .and_then(|f| f.parse::<u64>().ok())
            .map(|s| s.saturating_mul(512))
        else {
            continue;
        };
        let Some(write) = fields
            .get(9)
            .and_then(|f| f.parse::<u64>().ok())
            .map(|s| s.saturating_mul(512))
        else {
            continue;
        };
        counters.insert(format!("{}:{}:{name}", fields[0], fields[1]), (read, write));
    }
    (!counters.is_empty()).then_some(counters)
}

fn counter_rate(current: u64, previous: u64, seconds: f64) -> Option<f64> {
    (seconds > 0.0 && current >= previous).then(|| (current - previous) as f64 / seconds)
}

fn cpu_ticks(text: &str) -> Vec<CpuTicks> {
    text.lines()
        .take_while(|line| line.starts_with("cpu"))
        .filter_map(|line| {
            // Guest time is already included in user/nice; do not count it twice.
            let ticks: Vec<u64> = line
                .split_whitespace()
                .skip(1)
                .take(8)
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?;
            if ticks.len() < 4 {
                return None;
            }
            Some(CpuTicks {
                total: ticks.iter().sum(),
                idle: ticks[3] + ticks.get(4).copied().unwrap_or(0),
            })
        })
        .collect()
}

fn memory(text: &str) -> (u64, u64, u64, u64) {
    let fields: HashMap<&str, u64> = text
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((
                name,
                value.split_whitespace().next()?.parse::<u64>().ok()? * 1024,
            ))
        })
        .collect();
    let get = |key| fields.get(key).copied().unwrap_or(0);
    let total = get("MemTotal");
    let available = fields.get("MemAvailable").copied().unwrap_or_else(|| {
        get("MemFree") + get("Buffers") + get("Cached") + get("SReclaimable")
            - get("Shmem").min(get("Cached"))
    });
    let swap = get("SwapTotal");
    (
        total.saturating_sub(available),
        total,
        swap.saturating_sub(get("SwapFree")),
        swap,
    )
}

pub fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memory_uses_available_not_free() {
        let (used, total, used_swap, swap) = memory(
            "MemTotal: 1000 kB\nMemFree: 100 kB\nMemAvailable: 400 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n",
        );
        assert_eq!(
            (used, total, used_swap, swap),
            (600 * 1024, 1000 * 1024, 0, 0)
        );
    }
    #[test]
    fn cpu_does_not_double_count_guest() {
        let rows = cpu_ticks("cpu 100 20 30 400 10 5 5 0 40 10\ncpu0 1 2 3 4\nintr 0\n");
        assert_eq!(rows[0].total, 570);
        assert_eq!(rows[0].idle, 410);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn network_uses_default_route_interface_without_double_counting() {
        let route = "Iface Destination Gateway Flags\neth0 00000000 0100007F 0003\namn0 00000001 00000000 0001\n";
        let dev = "header\nheader\nlo: 999 0 0 0 0 0 0 0 888 0\neth0: 1000 0 0 0 0 0 0 0 250 0\namn0: 700 0 0 0 0 0 0 0 600 0\n";
        let interface = default_interface(route);
        assert_eq!(interface.as_deref(), Some("eth0"));
        assert_eq!(
            network_counters(dev, interface.as_deref()),
            Some(BTreeMap::from([("eth0".into(), (1000, 250))]))
        );
    }

    #[test]
    fn disk_counts_selected_whole_devices_once() {
        let devices = HashSet::from(["sda".to_owned(), "nvme0n1".to_owned()]);
        let stats = "8 0 sda 1 0 10 0 2 0 20 0 0\n8 1 sda1 1 0 100 0 2 0 200 0 0\n259 0 nvme0n1 1 0 4 0 2 0 8 0 0\n";
        assert_eq!(
            disk_counters(stats, &devices),
            Some(BTreeMap::from([
                ("8:0:sda".into(), (10 * 512, 20 * 512)),
                ("259:0:nvme0n1".into(), (4 * 512, 8 * 512)),
            ]))
        );
    }

    #[test]
    fn disk_and_net_counters_tolerate_malformed_lines() {
        // Test network_counters with malformed lines (no colon, fewer columns, non-numeric values)
        let dev = "header\nheader\nmalformed_line_without_colon\neth0: 1000 0 0 0 0 0 0 0 250 0\nshort: 1 2 3\nbad_rx: abc 0 0 0 0 0 0 0 100 0\n";
        let counters = network_counters(dev, Some("eth0"));
        assert_eq!(
            counters,
            Some(BTreeMap::from([("eth0".into(), (1000, 250))]))
        );

        // Test disk_counters with malformed lines (empty line, fewer fields, unparseable numbers)
        let devices = HashSet::from(["sda".to_owned()]);
        let stats = "\nshort line\n8 0\n8 0 sda invalid_field\n8 0 sda 1 0 10 0 2 0 20 0 0\n";
        assert_eq!(
            disk_counters(stats, &devices),
            Some(BTreeMap::from([("8:0:sda".into(), (10 * 512, 20 * 512)),]))
        );
    }
    #[test]
    fn rates_handle_elapsed_time_and_counter_reset() {
        assert_eq!(counter_rate(300, 100, 2.0), Some(100.0));
        assert_eq!(counter_rate(50, 100, 1.0), None);
        assert_eq!(counter_rate(100, 100, 0.0), None);
    }
    fn counters(name: &str, read: u64, write: u64) -> Option<DeviceCounters> {
        Some(BTreeMap::from([(name.into(), (read, write))]))
    }

    #[test]
    fn topology_changes_rebaseline_only_the_affected_metric() {
        let mut sampler = SystemSampler::new();
        let now = Instant::now();
        let sample = |net, disk| IoCounters { network: net, disk };
        sampler.sample_io_at(
            sample(counters("eth0", 100, 200), counters("8:0:sda", 1000, 2000)),
            now,
        );
        assert_eq!(sampler.stats.network_rx_per_sec, None);
        sampler.sample_io_at(
            sample(
                counters("wlan0", 9000, 10000),
                counters("8:0:sda", 1100, 2200),
            ),
            now + std::time::Duration::from_secs(1),
        );
        assert_eq!(sampler.stats.network_rx_per_sec, None);
        assert_eq!(sampler.stats.disk_read_per_sec, Some(100.));
        sampler.sample_io_at(
            sample(
                counters("wlan0", 9050, 10075),
                counters("8:16:sdb", 90000, 100000),
            ),
            now + std::time::Duration::from_secs(2),
        );
        assert_eq!(sampler.stats.network_rx_per_sec, Some(50.));
        assert_eq!(sampler.stats.network_tx_per_sec, Some(75.));
        assert_eq!(sampler.stats.disk_read_per_sec, None);
        sampler.sample_io_at(
            sample(None, counters("8:16:sdb", 90010, 100020)),
            now + std::time::Duration::from_secs(3),
        );
        assert_eq!(sampler.stats.network_rx_per_sec, None);
        assert_eq!(sampler.stats.disk_read_per_sec, Some(10.));
        sampler.sample_io_at(
            sample(counters("wlan0", 9999, 12000), None),
            now + std::time::Duration::from_secs(4),
        );
        assert_eq!(sampler.stats.network_rx_per_sec, None);
        assert_eq!(sampler.stats.disk_read_per_sec, None);
        sampler.sample_io_at(
            sample(counters("wlan0", 10049, 12075), None),
            now + std::time::Duration::from_secs(5),
        );
        assert_eq!(sampler.stats.network_rx_per_sec, Some(50.));
    }

    #[test]
    fn device_order_is_stable_and_one_reset_cannot_hide_in_the_total() {
        let previous = BTreeMap::from([("a".into(), (100, 100)), ("b".into(), (200, 200))]);
        let current = BTreeMap::from([("b".into(), (220, 240)), ("a".into(), (110, 130))]);
        assert_eq!(
            device_rates(Some(&current), Some(&previous), 2.),
            (Some(15.), Some(35.))
        );
        let reset = BTreeMap::from([("a".into(), (1, 1)), ("b".into(), (1000, 1000))]);
        assert_eq!(
            device_rates(Some(&reset), Some(&previous), 1.),
            (None, None)
        );
        assert_eq!(
            device_rates(counters("a", 110, 130).as_ref(), Some(&previous), 1.),
            (None, None)
        );
        assert_eq!(
            device_rates(Some(&current), Some(&previous), 0.),
            (None, None)
        );
    }
}
