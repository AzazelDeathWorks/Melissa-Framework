use crate::{Error, Result};
use melissa_windows_executor::{HostCapture, TerminationReason};
use std::{path::Path, sync::atomic::AtomicBool};

pub(crate) fn capture(
    executable: &Path,
    arguments: &[String],
    cwd: &Path,
    cancel: &AtomicBool,
) -> Result<HostCapture> {
    let result = melissa_windows_executor::capture_host(
        executable,
        arguments,
        cwd,
        120_000,
        32 * 1024 * 1024,
        cancel,
    )
    .map_err(|e| match e {
        melissa_windows_executor::ExecutorError::CancelledBeforeLaunch => Error::Cancelled,
        _ => Error::Denied("owned bounded host inspection failed"),
    })?;
    match result.termination {
        TerminationReason::Exited => Ok(result),
        TerminationReason::Cancelled => Err(Error::Cancelled),
        TerminationReason::Timeout => Err(Error::Invalid("host inspection deadline exceeded")),
        TerminationReason::OutputLimit => {
            Err(Error::Invalid("host inspection output limit exceeded"))
        }
        _ => Err(Error::Denied("host inspection could not be completed")),
    }
}
