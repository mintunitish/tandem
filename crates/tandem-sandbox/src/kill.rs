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

/// Process groups of every descendant of the processes in group `pgid`, other
/// than `pgid` itself. Agents (e.g. opencode/bun with `detached: true`) start
/// tool shells in their own groups, so signalling the agent's group alone
/// leaves them running. Must be called *before* signalling: once a parent dies
/// its children are reparented to launchd and the link is lost.
pub fn descendant_groups(pgid: i32) -> Vec<i32> {
    if pgid <= 1 {
        return vec![];
    }
    let Ok(out) = Command::new("/bin/ps").args(["-axo", "pid=,ppid=,pgid="]).output() else { return vec![] };
    let rows: Vec<(i32, i32, i32)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace().map(|x| x.parse::<i32>().ok());
            Some((it.next()??, it.next()??, it.next()??))
        })
        .collect();
    // SAFETY: plain getter.
    let own = unsafe { libc::getpgrp() };
    let mut seen: Vec<i32> = rows.iter().filter(|r| r.2 == pgid).map(|r| r.0).collect();
    let mut i = 0;
    while i < seen.len() {
        let parent = seen[i];
        for r in rows.iter().filter(|r| r.1 == parent) {
            if !seen.contains(&r.0) {
                seen.push(r.0);
            }
        }
        i += 1;
    }
    let mut groups: Vec<i32> = Vec::new();
    for r in rows.iter().filter(|r| seen.contains(&r.0)) {
        if r.2 != pgid && r.2 > 1 && r.2 != own && !groups.contains(&r.2) {
            groups.push(r.2);
        }
    }
    groups
}

/// Stops one process group and the groups its descendants moved into: SIGTERM (+SIGCONT)
/// now, SIGKILL whatever is still alive after `grace`. Blocks for at most `grace`.
/// Returns the groups that were signalled.
pub fn stop_group(pgid: i32, grace: Duration) -> Vec<i32> {
    if pgid <= 1 {
        return vec![];
    }
    // Collect descendants first: once the leader dies they are reparented and lost.
    let mut groups = vec![pgid];
    groups.extend(descendant_groups(pgid));
    groups.retain(|g| signal_group(*g, libc::SIGTERM));
    for g in &groups {
        signal_group(*g, libc::SIGCONT);
    }
    let deadline = Instant::now() + grace;
    while groups.iter().any(|g| !leader_exited(*g) && group_alive(*g)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    for g in &groups {
        if group_alive(*g) {
            signal_group(*g, libc::SIGKILL);
        }
    }
    groups
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
        let mut entries: Vec<Entry> = std::mem::take(&mut *self.groups.lock()).into_iter().map(|(p, l)| Entry { pgid: p, label: l }).collect();
        // Tool processes the agent moved into their own groups.
        for e in entries.clone() {
            for g in descendant_groups(e.pgid) {
                if !entries.iter().any(|x| x.pgid == g) {
                    entries.push(Entry { pgid: g, label: format!("{} (child group)", e.label) });
                }
            }
        }
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
    fn stops_child_in_its_own_group() {
        let ks = KillSwitch::new();
        let mut cmd = Command::new("/bin/sh");
        // `set -m` (job control) puts the background `sleep` in a new process group.
        cmd.args(["-c", "set -m; sleep 30 & echo $!; wait"]);
        cmd.stdout(std::process::Stdio::piped());
        configure(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let pid = child.id() as i32;
        ks.register_child(&child, "sh");
        let mut line = String::new();
        use std::io::BufRead;
        std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let sleeper: i32 = line.trim().parse().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_ne!(unsafe { libc::getpgid(sleeper) }, pid, "sleep must be in its own group");
        assert!(descendant_groups(pid).contains(&sleeper));
        let r = ks.stop_all_with_grace(Duration::from_millis(500));
        assert!(r.terminated.iter().any(|e| e.pgid == sleeper), "{r:?}");
        let _ = child.wait();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!group_alive(sleeper), "detached child group must be gone");
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
