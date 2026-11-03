#![cfg(windows)]
use melissa_windows_executor::{TerminationReason, capture_host};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[test]
fn trusted_host_capture_preserves_arguments_and_bounds_both_output_streams() {
    let temp = tempfile::tempdir().unwrap();
    let executable = Path::new(env!("CARGO_BIN_EXE_melissa-executor-probe"));
    let argument = "spaces and \"quotes\" Turkish: şğü trailing\\";
    let output = capture_host(
        executable,
        &["host-capture-echo".into(), argument.into()],
        temp.path(),
        5000,
        4096,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(output.termination, TerminationReason::Exited);
    assert_eq!(output.exit_code, 0);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim_end(),
        argument
    );
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("captured-error-stream")
    );
    let output = capture_host(
        executable,
        &["flood".into()],
        temp.path(),
        5000,
        1024,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(output.termination, TerminationReason::OutputLimit);
    assert!(output.stdout.len() + output.stderr.len() <= 1024);
}

#[test]
fn trusted_host_capture_stops_on_deadline_and_midflight_cancellation() {
    let temp = tempfile::tempdir().unwrap();
    let executable = Path::new(env!("CARGO_BIN_EXE_melissa-executor-probe"));
    let started = Instant::now();
    let output = capture_host(
        executable,
        &["tree".into()],
        temp.path(),
        150,
        4096,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(output.termination, TerminationReason::Timeout);
    assert!(started.elapsed() < Duration::from_secs(5));
    let cancel = Arc::new(AtomicBool::new(false));
    let trigger = cancel.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        trigger.store(true, Ordering::Release);
    });
    let output = capture_host(
        executable,
        &["tree".into()],
        temp.path(),
        5000,
        4096,
        &cancel,
    )
    .unwrap();
    thread.join().unwrap();
    assert_eq!(output.termination, TerminationReason::Cancelled);
}
