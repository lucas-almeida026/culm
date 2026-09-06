//! Per-session statistics. Reading `/proc` is a side effect, so it enters through a
//! trait and a test supplies a fake.

use std::collections::HashMap;
use std::fmt;

/// Resident memory of a process and every process it started.
pub trait MemoryProbe: fmt::Debug {
    /// Bytes of resident memory for the whole tree, or zero when the tree is gone.
    fn rss_tree(&self, pid: u32) -> u64;
}

/// Reads `/proc` on Linux.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcMemoryProbe;

impl ProcMemoryProbe {
    /// Maps every process to its parent, from the third field of `/proc/<pid>/stat`.
    fn parents() -> HashMap<u32, u32> {
        let mut map = HashMap::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return map;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            // The command name sits in parentheses and may hold spaces, so the fields
            // that matter start after the last closing parenthesis.
            let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else {
                continue;
            };
            if let Some(ppid) = rest.split_whitespace().nth(1).and_then(|s| s.parse().ok()) {
                map.insert(pid, ppid);
            }
        }
        map
    }

    fn rss_of(pid: u32) -> u64 {
        let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
            return 0;
        };
        for line in status.lines() {
            if let Some(v) = line.strip_prefix("VmRSS:") {
                let kb: u64 = v
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                return kb * 1024;
            }
        }
        0
    }
}

impl MemoryProbe for ProcMemoryProbe {
    fn rss_tree(&self, pid: u32) -> u64 {
        let parents = Self::parents();
        let mut total = Self::rss_of(pid);
        // A descendant is any process whose parent chain reaches `pid`. The chain is
        // walked with a bound, so a cycle in a stale snapshot cannot hang the tick.
        for &child in parents.keys() {
            if child == pid {
                continue;
            }
            let mut cur = child;
            for _ in 0..64 {
                match parents.get(&cur) {
                    Some(&p) if p == pid => {
                        total += Self::rss_of(child);
                        break;
                    }
                    Some(&p) if p > 1 => cur = p,
                    _ => break,
                }
            }
        }
        total
    }
}

/// A short, fixed-width rendering of a byte count.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const K: u64 = 1024;
    const M: u64 = K * 1024;
    const G: u64 = M * 1024;
    if bytes >= G {
        format!("{:.1}G", bytes as f64 / G as f64)
    } else if bytes >= M {
        format!("{}M", bytes / M)
    } else if bytes >= K {
        format!("{}K", bytes / K)
    } else {
        format!("{bytes}B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_uses_megabytes_for_a_claude_process() {
        assert_eq!(format_bytes(436 * 1024 * 1024), "436M");
    }

    #[test]
    fn format_bytes_switches_unit_at_each_boundary() {
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(2048), "2K");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0G");
    }
}
