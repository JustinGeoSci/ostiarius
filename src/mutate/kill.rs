//  src/mutate/kill.rs

//! A command that hangs is killed together with every process it started,
//! because killing the command alone is not enough.
//!
//! `cargo test` runs each test binary as its own process, and a mutation that
//! makes a test loop forever leaves that binary running after Cargo is gone. On
//! Windows the binary would also keep its executable locked, so the next build
//! could not replace it. Both platforms are handled by the system's own tools
//! rather than by `unsafe` code. On Windows, `taskkill /T /F` walks the tree of
//! processes from the command's id. Elsewhere the command starts as the leader
//! of its own process group (see `exec`), so the group holds the whole tree,
//! and `kill -KILL -- -<group>` signals all of it.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

// ===========================================================================
// Killing
// ===========================================================================

/// Kills `child` and its descendants, then reaps `child`.
pub(crate) fn kill_tree(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    let pid = child.id().to_string();
    let (tool, status) = if cfg!(windows) {
        (
            "taskkill",
            quiet(Command::new("taskkill").args(["/T", "/F", "/PID", &pid])),
        )
    } else {
        (
            "kill",
            quiet(Command::new("kill").args(["-KILL", "--", &format!("-{pid}")])),
        )
    };
    match status {
        Ok(status) if status.success() => {}
        Ok(_) if matches!(child.try_wait(), Ok(Some(_))) => {}
        Ok(status) => {
            eprintln!(
                "warning: `{tool}` failed ({status}); killing the command alone, so a test process may still be running"
            );
            let _ = child.kill();
        }
        Err(e) => {
            eprintln!(
                "warning: could not run `{tool}` ({e}); killing the command alone, so a test process may still be running"
            );
            let _ = child.kill();
        }
    }
    reap(child, Duration::from_secs(30));
}

fn quiet(cmd: &mut Command) -> std::io::Result<std::process::ExitStatus> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
}

/// Waits up to `limit` for `child` to exit, then kills it directly.
fn reap(child: &mut Child, limit: Duration) {
    let start = Instant::now();
    while start.elapsed() < limit {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(all(test, unix))]
mod tests {
    use super::kill_tree;
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use std::time::{Duration, Instant};

    /// Whether a process is running: not gone, and not a zombie waiting to
    /// be reaped.
    fn alive(pid: u32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat
                .rsplit_once(") ")
                .is_some_and(|(_, rest)| !rest.starts_with('Z')),
            Err(_) => Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .is_ok_and(|s| s.success()),
        }
    }

    #[test]
    fn kill_tree_kills_grandchild() {
        let pid_file =
            std::env::temp_dir().join(format!("ostiarius-kill-{}.pid", std::process::id()));
        let _ = std::fs::remove_file(&pid_file);
        let script = format!("sleep 60 & echo $! > '{}'; wait", pid_file.display());
        let mut child = Command::new("sh")
            .args(["-c", &script])
            .process_group(0)
            .spawn()
            .expect("spawn");
        let start = Instant::now();
        let grandchild = loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
            {
                break pid;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the grandchild never started"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(alive(grandchild));
        kill_tree(&mut child);
        let start = Instant::now();
        while alive(grandchild) && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(grandchild), "the grandchild outlived kill_tree");
        assert!(
            child.try_wait().expect("try_wait").is_some(),
            "the child was reaped"
        );
        let _ = std::fs::remove_file(&pid_file);
    }
}
