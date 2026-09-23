//! Ending the process tree under the CLI, not only the CLI.
//!
//! Measured on `claude` 2.1.280: a Bash call runs as `zsh -c …` in a session of its own
//! (`setsid`; the shell is the leader of a new session and process group), so killing the
//! CLI, or the CLI's process group, leaves the command running. Reparented to launchd, an
//! approved `perl -e 'sleep 60' && touch marker` ran to its end after `terminate` and after
//! the application quit. The CLI's own interrupt does end that group; a SIGKILL of the CLI
//! does not.
//!
//! So `terminate` walks the tree while the CLI is still its root: stop the CLI, so it
//! starts nothing new; find its descendants by parent pid, stopping each one found and
//! looking again until a pass finds none, since a descendant that was already running can
//! still fork; then kill every process group the CLI or a descendant leads — the CLI's
//! own is dedicated to it, because the CLI is spawned as a group leader — and any other
//! descendant alone, and last the CLI. A process is identified by its pid *and* its start
//! time, and each is checked again just before the signal, so a pid or group id reused
//! since the walk is not signalled.
//!
//! What this cannot reach, and does not claim to: a descendant that left the tree before
//! the walk (a daemon that double-forked, or anything reparented because the process
//! between it and the CLI had already exited), and every descendant of a CLI that ended
//! without `terminate` — a crash reparents them before the core learns of it. What an
//! approved command already did is not undone either.

/// Stops the tree under `root`, a live, unreaped child of this process that was spawned as
/// the leader of its own process group, and kills it. Never signals this process's own
/// group. Returns once the signals are sent; the caller reaps `root`.
#[cfg(target_os = "macos")]
pub(super) fn kill(root: u32) {
    use std::collections::{HashMap, HashSet};
    use std::time::{Duration, Instant};

    /// Bounds the walk against a tree that forks faster than it can be stopped. Past it,
    /// what was found is killed anyway.
    const WALK_FOR: Duration = Duration::from_millis(500);

    let Ok(root) = i32::try_from(root) else {
        return;
    };
    let own_group = unsafe { libc::getpgrp() };
    unsafe { libc::kill(root, libc::SIGSTOP) };

    // Every descendant found, by pid, with the start time that identifies it.
    let mut found: HashMap<i32, Proc> = HashMap::new();
    let deadline = Instant::now() + WALK_FOR;
    loop {
        let table = snapshot();
        let mut fresh = false;
        let mut frontier = vec![root];
        let mut seen: HashSet<i32> = HashSet::from([root]);
        while let Some(parent) = frontier.pop() {
            for p in table.values().filter(|p| p.ppid == parent) {
                if !seen.insert(p.pid) {
                    continue;
                }
                frontier.push(p.pid);
                if found.get(&p.pid).map(|f| f.start) != Some(p.start) {
                    unsafe { libc::kill(p.pid, libc::SIGSTOP) };
                    found.insert(p.pid, *p);
                    fresh = true;
                }
            }
        }
        if !fresh || Instant::now() >= deadline {
            break;
        }
    }

    // Checked against a fresh table: only a process found in the walk, the same one by
    // start time, is signalled, and a group is signalled whole only when its leader is
    // one — the CLI, or a descendant that made a group of its own. A descendant that
    // joined some other group (this process's, or a terminal's job) is signalled alone.
    let table = snapshot();
    let alive = |pid: i32| {
        found
            .get(&pid)
            .zip(table.get(&pid))
            .is_some_and(|(then, now)| then.start == now.start)
    };
    let mut groups: HashSet<i32> = HashSet::from([root]);
    for pid in found.keys().copied().filter(|&pid| alive(pid)) {
        let group = table[&pid].pgid;
        if group != own_group && (group == root || alive(group)) {
            groups.insert(group);
        } else {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
    for group in groups {
        unsafe { libc::killpg(group, libc::SIGKILL) };
    }
    unsafe { libc::kill(root, libc::SIGKILL) };

    #[derive(Clone, Copy)]
    struct Proc {
        pid: i32,
        ppid: i32,
        pgid: i32,
        start: (u64, u64),
    }

    fn snapshot() -> HashMap<i32, Proc> {
        let mut pids: Vec<i32> = Vec::new();
        // The count can grow between the sizing call and the listing; ask with room.
        let mut capacity = 1024usize;
        loop {
            pids.resize(capacity, 0);
            let bytes = i32::try_from(capacity * std::mem::size_of::<i32>()).unwrap_or(i32::MAX);
            let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
            if n < 0 {
                return HashMap::new();
            }
            // `proc_listallpids` returns a count of pids, not bytes.
            let n = n as usize;
            if n < capacity {
                pids.truncate(n);
                break;
            }
            capacity *= 2;
        }
        let mut table = HashMap::with_capacity(pids.len());
        for pid in pids.into_iter().filter(|&p| p > 0) {
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
            let got = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&mut info as *mut libc::proc_bsdinfo).cast(),
                    size,
                )
            };
            if got != size {
                continue;
            }
            table.insert(
                pid,
                Proc {
                    pid,
                    ppid: info.pbi_ppid as i32,
                    pgid: info.pbi_pgid as i32,
                    start: (info.pbi_start_tvsec, info.pbi_start_tvusec),
                },
            );
        }
        table
    }
}

/// Elsewhere only the CLI's own group is killed: a descendant that made a session of its
/// own is not reached. stanchion ships on macOS; this keeps the crate building.
#[cfg(not(target_os = "macos"))]
pub(super) fn kill(root: u32) {
    if let Ok(root) = i32::try_from(root) {
        unsafe { libc::killpg(root, libc::SIGKILL) };
        unsafe { libc::kill(root, libc::SIGKILL) };
    }
}
