//! Kill Switch: a registry of agent process groups and one global STOP.
//!
//! Every agent (and every tool process it spawns) runs in its own process group, set up
//! with [`configure`]. [`KillSwitch::stop_all`] sends SIGTERM to every registered group,
//! waits up to 2 s, then SIGKILLs survivors. Signalling the *group* reaches grandchildren
//! (shells, test runners) that would otherwise be orphaned.
//!
//! Callers should [`KillSwitch::unregister`] a group once its leader has been reaped, so
//! a recycled PGID can never be signalled.

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const DEFAULT_GRACE: Duration = Duration::from_secs(2);

/// Put the child in a new process group whose PGID equals its PID.
pub fn configure(cmd: &mut Command) {
    cmd.process_group(0);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub pgid: i32,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StopReport {
    /// Groups that received SIGTERM.
    pub terminated: Vec<Entry>,
    /// Groups still alive after the grace period that received SIGKILL.
    pub killed: Vec<Entry>,
}

#[derive(Default)]
pub struct KillSwitch {
    groups: parking_mutex::Mutex<BTreeMap<i32, String>>,
    tripped: AtomicBool,
}

mod parking_mutex {
    /// std Mutex that ignores poisoning: the kill switch must work after a panic.
    #[derive(Default)]
    pub struct Mutex<T>(std::sync::Mutex<T>);
    impl<T> Mutex<T> {
        pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
            self.0.lock().unwrap_or_else(|e| e.into_inner())
        }
    }
}

fn group_alive(pgid: i32) -> bool {
    // SAFETY: kill with signal 0 only checks permissions/existence.
    let r = unsafe { libc::kill(-pgid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// True when the leader has exited (zombie or reaped). Does not reap.
fn leader_exited(pid: i32) -> bool {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: valid pointer to a zeroed siginfo_t; WNOWAIT leaves the child waitable.
    let r = unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) };
    if r != 0 {
        // ECHILD: not our child (or already reaped). Fall back to existence.
        return unsafe { libc::kill(pid, 0) } != 0;
    }
    info.si_pid != 0
}

fn signal_group(pgid: i32, sig: i32) -> bool {
    if pgid <= 1 {
        return false; // never signal init or "every process"
    }
    // SAFETY: plain syscall; pgid > 1 so this targets exactly one group.
    unsafe { libc::kill(-pgid, sig) == 0 }
}

impl KillSwitch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process-wide instance used by the app's STOP button.
    pub fn global() -> &'static KillSwitch {
        static G: OnceLock<KillSwitch> = OnceLock::new();
        G.get_or_init(KillSwitch::new)
    }

    /// Register a group leader spawned with [`configure`] (PGID == PID).
    pub fn register(&self, pid: u32, label: impl Into<String>) {
        if let Ok(p) = i32::try_from(pid)
            && p > 1
        {
            self.groups.lock().insert(p, label.into());
        }
    }

    pub fn register_child(&self, child: &Child, label: impl Into<String>) {
        self.register(child.id(), label);
    }

    pub fn unregister(&self, pid: u32) {
        if let Ok(p) = i32::try_from(pid) {
            self.groups.lock().remove(&p);
        }
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.groups.lock().iter().map(|(p, l)| Entry { pgid: *p, label: l.clone() }).collect()
    }

    /// True after a STOP until [`KillSwitch::reset`]. Spawners should refuse new agents.
    pub fn is_tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }

    pub fn reset(&self) {
        self.tripped.store(false, Ordering::SeqCst);
    }

    /// SIGTERM every group, wait 2 s, SIGKILL survivors.
    pub fn stop_all(&self) -> StopReport {
        self.stop_all_with_grace(DEFAULT_GRACE)
    }

    pub fn stop_all_with_grace(&self, grace: Duration) -> StopReport {
        self.tripped.store(true, Ordering::SeqCst);
        let entries: Vec<Entry> = std::mem::take(&mut *self.groups.lock()).into_iter().map(|(p, l)| Entry { pgid: p, label: l }).collect();
        let mut report = StopReport::default();
        for e in &entries {
            if signal_group(e.pgid, libc::SIGTERM) {
                // Stopped processes do not act on SIGTERM until continued.
                signal_group(e.pgid, libc::SIGCONT);
                report.terminated.push(e.clone());
            }
        }
        let deadline = Instant::now() + grace;
        loop {
            let pending = entries.iter().any(|e| !leader_exited(e.pgid) && group_alive(e.pgid));
            if !pending || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        for e in &entries {
            let leader_gone = leader_exited(e.pgid);
            if group_alive(e.pgid) && signal_group(e.pgid, libc::SIGKILL) && !leader_gone {
                report.killed.push(e.clone());
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_sleeping_child_and_grandchild() {
        let ks = KillSwitch::new();
        let mut cmd = Command::new("/bin/sh");
        // The grandchild `sleep` shares the group; SIGTERM to the group kills both.
        cmd.args(["-c", "sleep 30 & wait"]);
        configure(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let pid = child.id() as i32;
        ks.register_child(&child, "sh");
        std::thread::sleep(Duration::from_millis(100));
        // own group
        assert_eq!(unsafe { libc::getpgid(pid) }, pid);
        let t0 = Instant::now();
        let r = ks.stop_all();
        assert_eq!(r.terminated.len(), 1);
        assert!(t0.elapsed() < Duration::from_millis(1900), "should not need the full grace period");
        let st = child.wait().unwrap();
        assert!(!st.success());
        std::thread::sleep(Duration::from_millis(50));
        assert!(!group_alive(pid), "group must be gone");
        assert!(ks.is_tripped());
        assert!(ks.entries().is_empty());
    }

    #[test]
    fn sigkills_term_ignoring_child() {
        let ks = KillSwitch::new();
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "trap '' TERM; while :; do sleep 0.05; done"]);
        configure(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        ks.register_child(&child, "stubborn");
        std::thread::sleep(Duration::from_millis(150));
        let r = ks.stop_all_with_grace(Duration::from_millis(300));
        assert_eq!(r.terminated.len(), 1);
        assert_eq!(r.killed.len(), 1);
        let st = child.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(st.signal(), Some(libc::SIGKILL));
    }

    #[test]
    fn refuses_dangerous_pids() {
        let ks = KillSwitch::new();
        ks.register(0, "zero");
        ks.register(1, "init");
        assert!(ks.entries().is_empty());
        assert!(!signal_group(1, 0));
    }
}
