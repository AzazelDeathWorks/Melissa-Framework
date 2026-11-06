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
    pub scope: String,
}
impl Measurement {
    pub fn observe(baseline: &Sample) -> Self {
        let latest = sample();
        Self{schema_version:"melissa.process-measurement/v1".into(),elapsed_ms:latest.observed_at_ms.checked_sub(baseline.observed_at_ms),read_transfer_delta:latest.read_transfer_bytes.zip(baseline.read_transfer_bytes).and_then(|(a,b)|a.checked_sub(b)),write_transfer_delta:latest.write_transfer_bytes.zip(baseline.write_transfer_bytes).and_then(|(a,b)|a.checked_sub(b)),baseline:baseline.clone(),latest,scope:"CLI process only; lifetime peak includes setup; transfer I/O includes non-disk operations; workers not included".into()}
    }
}
pub fn sample() -> Sample {
    let mut result = Sample {
        observed_at_ms: crate::app::now().unwrap_or(0),
        working_set_bytes: None,
        process_lifetime_peak_working_set_bytes: None,
        read_transfer_bytes: None,
        write_transfer_bytes: None,
    };
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{GetCurrentProcess, GetProcessIoCounters, IO_COUNTERS},
        };
        let process = GetCurrentProcess();
        let mut memory: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        memory.cb = std::mem::size_of_val(&memory) as u32;
        if GetProcessMemoryInfo(process, &mut memory, std::mem::size_of_val(&memory) as u32) != 0 {
            result.working_set_bytes = Some(memory.WorkingSetSize as u64);
            result.process_lifetime_peak_working_set_bytes = Some(memory.PeakWorkingSetSize as u64);
        }
        let mut io: IO_COUNTERS = std::mem::zeroed();
        if GetProcessIoCounters(process, &mut io) != 0 {
            result.read_transfer_bytes = Some(io.ReadTransferCount);
            result.write_transfer_bytes = Some(io.WriteTransferCount);
        }
    }
    result
}
