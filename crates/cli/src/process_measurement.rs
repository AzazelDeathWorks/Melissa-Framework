//! Host process observations. Never mislabel process I/O as physical disk I/O.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub observed_at_ms: u64,
    pub working_set_bytes: Option<u64>,
    pub process_lifetime_peak_working_set_bytes: Option<u64>,
    pub read_transfer_bytes: Option<u64>,
    pub write_transfer_bytes: Option<u64>,
    #[serde(default)]
    pub cpu_time_us: Option<u64>,
    #[serde(default)]
    pub post_exec_peak_working_set_bytes: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measurement {
    pub schema_version: String,
    pub baseline: Sample,
    pub latest: Sample,
    pub elapsed_ms: Option<u64>,
    pub read_transfer_delta: Option<u64>,
    pub write_transfer_delta: Option<u64>,
    #[serde(default)]
    pub cpu_time_delta_us: Option<u64>,
    pub scope: String,
}
impl Measurement {
    pub fn observe(baseline: &Sample) -> Self {
        Self::between(baseline, sample())
    }
    fn between(baseline: &Sample, latest: Sample) -> Self {
        Self {
            schema_version: "melissa.process-measurement/v1".into(),
            elapsed_ms: latest.observed_at_ms.checked_sub(baseline.observed_at_ms),
            read_transfer_delta: delta(latest.read_transfer_bytes, baseline.read_transfer_bytes),
            write_transfer_delta: delta(latest.write_transfer_bytes, baseline.write_transfer_bytes),
            cpu_time_delta_us: delta(latest.cpu_time_us, baseline.cpu_time_us),
            baseline: baseline.clone(),
            latest,
            scope: SCOPE.into(),
        }
    }
}
pub const SCOPE: &str = "CLI process only; CPU is cumulative user + system time, not CPU percent; lifetime peak can include launcher memory before exec; post-exec peak is separate where supported; transfer I/O includes non-disk operations; workers not included";
fn delta(latest: Option<u64>, baseline: Option<u64>) -> Option<u64> {
    latest.zip(baseline).and_then(|(a, b)| a.checked_sub(b))
}
pub fn sample() -> Sample {
    let mut result = Sample {
        observed_at_ms: crate::app::now().unwrap_or(0),
        working_set_bytes: None,
        process_lifetime_peak_working_set_bytes: None,
        read_transfer_bytes: None,
        write_transfer_bytes: None,
        cpu_time_us: None,
        post_exec_peak_working_set_bytes: None,
    };
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{GetCurrentProcess, GetProcessIoCounters, GetProcessTimes, IO_COUNTERS},
        };
        let process = GetCurrentProcess();
        let mut memory: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        memory.cb = std::mem::size_of_val(&memory) as u32;
        if GetProcessMemoryInfo(process, &mut memory, std::mem::size_of_val(&memory) as u32) != 0 {
            result.working_set_bytes = Some(memory.WorkingSetSize as u64);
            result.process_lifetime_peak_working_set_bytes = Some(memory.PeakWorkingSetSize as u64);
            result.post_exec_peak_working_set_bytes = Some(memory.PeakWorkingSetSize as u64);
        }
        let mut io: IO_COUNTERS = std::mem::zeroed();
        if GetProcessIoCounters(process, &mut io) != 0 {
            result.read_transfer_bytes = Some(io.ReadTransferCount);
            result.write_transfer_bytes = Some(io.WriteTransferCount);
        }
        let (mut created, mut exited, mut kernel, mut user) = std::mem::zeroed();
        if GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) != 0 {
            let ticks = |t: windows_sys::Win32::Foundation::FILETIME| {
                ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
            };
            result.cpu_time_us = ticks(kernel).checked_add(ticks(user)).map(|n| n / 10);
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) == 0 {
            let micros = |t: libc::timeval| -> Option<u64> {
                let seconds = u64::try_from(t.tv_sec).ok()?;
                let fraction = u64::try_from(t.tv_usec).ok().filter(|n| *n < 1_000_000)?;
                seconds.checked_mul(1_000_000)?.checked_add(fraction)
            };
            result.cpu_time_us = micros(usage.ru_utime)
                .zip(micros(usage.ru_stime))
                .and_then(|(a, b)| a.checked_add(b));
            let scale = if cfg!(target_os = "macos") { 1 } else { 1024 };
            result.process_lifetime_peak_working_set_bytes = u64::try_from(usage.ru_maxrss)
                .ok()
                .and_then(|n| n.checked_mul(scale));
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            result.working_set_bytes = proc_value(&status, "VmRSS:", true);
            result.post_exec_peak_working_set_bytes = proc_value(&status, "VmHWM:", true);
        }
        if let Ok(io) = std::fs::read_to_string("/proc/self/io") {
            result.read_transfer_bytes = proc_value(&io, "rchar:", false);
            result.write_transfer_bytes = proc_value(&io, "wchar:", false);
        }
    }
    #[cfg(target_os = "macos")]
    unsafe {
        let mut info: libc::mach_task_basic_info = std::mem::zeroed();
        let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
        if libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            (&mut info as *mut libc::mach_task_basic_info).cast(),
            &mut count,
        ) == libc::KERN_SUCCESS
        {
            result.working_set_bytes = Some(info.resident_size);
        }
    }
    result
}

#[cfg(any(test, target_os = "linux"))]
fn proc_value(text: &str, key: &str, kib: bool) -> Option<u64> {
    let mut matches = text.lines().filter_map(|line| line.strip_prefix(key));
    let value = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let mut fields = value.split_whitespace();
    let value = fields.next()?.parse::<u64>().ok()?;
    if kib && fields.next()? != "kB" {
        return None;
    }
    if fields.next().is_some() {
        return None;
    }
    value.checked_mul(if kib { 1024 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proc_fields_reject_bad_units_overflow_and_duplicates() {
        assert_eq!(proc_value("VmRSS: 12 kB\n", "VmRSS:", true), Some(12288));
        assert_eq!(proc_value("rchar: 12\n", "rchar:", false), Some(12));
        for text in [
            "",
            "VmRSS: -1 kB",
            "VmRSS: 12 MB",
            "VmRSS: 18446744073709551615 kB",
            "VmRSS: 12 kB extra",
            "VmRSS: 1 kB\nVmRSS: 2 kB",
        ] {
            assert_eq!(proc_value(text, "VmRSS:", true), None, "{text}");
        }
    }
    #[test]
    fn older_measurements_and_missing_or_reversed_counters_remain_unknown() {
        let baseline: Sample = serde_json::from_str(r#"{"observed_at_ms":10,"working_set_bytes":null,"process_lifetime_peak_working_set_bytes":null,"read_transfer_bytes":10,"write_transfer_bytes":null}"#).unwrap();
        assert_eq!(baseline.cpu_time_us, None);
        let mut latest = baseline.clone();
        latest.observed_at_ms = 9;
        latest.read_transfer_bytes = Some(9);
        let measured = Measurement::between(&baseline, latest);
        assert_eq!(measured.elapsed_ms, None);
        assert_eq!(measured.cpu_time_delta_us, None);
        assert_eq!(measured.read_transfer_delta, None);
        assert_eq!(measured.write_transfer_delta, None);
        assert_eq!(delta(Some(12), Some(10)), Some(2));
        assert_eq!(delta(Some(0), Some(0)), Some(0));
    }
    #[test]
    fn real_host_observations_have_memory_and_monotonic_cpu() {
        let before = sample();
        let after = sample();
        #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
        {
            assert!(before.working_set_bytes.is_some_and(|n| n > 0));
            assert!(
                before
                    .process_lifetime_peak_working_set_bytes
                    .is_some_and(|n| n > 0)
            );
            assert!(after.cpu_time_us.unwrap() >= before.cpu_time_us.unwrap());
        }
        #[cfg(any(windows, target_os = "linux"))]
        assert!(
            before
                .post_exec_peak_working_set_bytes
                .is_some_and(|n| n > 0)
        );
    }
}
