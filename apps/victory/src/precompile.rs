use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use victory_gpu::pipeline;
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};

static STOP: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn interrupt(event: u32) -> i32 {
    if matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT) {
        STOP.store(true, Ordering::Release);
        return 1;
    }
    0
}

pub(super) fn run(cache: Option<PathBuf>, check: bool) -> Result<()> {
    let root = match cache {
        Some(path) => path,
        None => victory_core::Config::load()?.cache_dir,
    };
    for dir in ["shaders", "pipelines"] {
        victory_core::ensure_outside_repo(&root.join(dir))?;
    }
    if check {
        println!("precompile-check {}", pipeline::outstanding(&root)?);
        return Ok(());
    }
    // A desktop child has a pipe and may have no console. Closing the write
    // end requests the same drain and save as a console interrupt.
    // A process group may inherit an instruction to ignore Ctrl+C. Restore
    // delivery in this child after installing the handler that drains workers.
    unsafe {
        SetConsoleCtrlHandler(Some(interrupt), 1);
        SetConsoleCtrlHandler(None, 0);
    }
    std::thread::Builder::new()
        .name("precompile-input".into())
        .spawn(|| {
            let mut buf = [0; 128];
            let mut input = io::stdin().lock();
            loop {
                match input.read(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            STOP.store(true, Ordering::Release);
        })?;
    let mut stdout = io::stdout().lock();
    pipeline::precompile(&root, &STOP, |done, total, failed| {
        writeln!(stdout, "precompile {done} {total} {failed}")?;
        stdout.flush()
    })?;
    Ok(())
}
