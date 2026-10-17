//! What a window is running underneath its shell: every descendant process
//! and the TCP ports each of them listens on.
//!
//! The kernel lists listening sockets per network namespace in
//! `/proc/net/tcp{,6}` by inode, and a process holds a socket as an fd
//! pointing at `socket:[inode]`, so "who serves :5173" is those two lists
//! joined. No `ss`/`lsof` fork, no netlink.

use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::mem::ProcTable;

/// One process in a window's tree, as the monitor shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    /// 0 for the pane's own process, 1 for its children, ...
    pub depth: usize,
    /// What to call it: the program, or for an interpreter the script it
    /// runs (`node server.js`), because `comm` is `node-MainThread`.
    pub name: String,
    /// Full command line, or the name for a process that hides it.
    pub cmd: String,
}

/// The tree under `root`, depth first in launch order, root included.
pub fn tree(root: u32, table: &ProcTable) -> Vec<Proc> {
    let mut out = Vec::new();
    let mut stack = vec![(root, 0)];
    while let Some((pid, depth)) = stack.pop() {
        // Reversed so the first-launched child is popped (and shown) first.
        stack.extend(
            table
                .children_of(pid)
                .into_iter()
                .rev()
                .map(|c| (c, depth + 1)),
        );
        let Some(name) = fs::read_to_string(format!("/proc/{pid}/comm")).ok() else {
            continue; // Exited between the table read and now.
        };
        let comm = name.trim_end();
        let cmd = crate::mem::cmdline(pid).unwrap_or_else(|| comm.to_string());
        let name = program(&cmd, comm);
        out.push(Proc {
            pid,
            depth,
            name,
            cmd,
        });
    }
    out
}

/// Runtimes whose own name says nothing: the script they run is the program.
const INTERPRETERS: &[&str] = &[
    "node", "python", "bun", "deno", "ruby", "perl", "php", "npx", "bash", "sh", "zsh",
];

/// A readable name for `cmd`: the executable's base name (a login shell's
/// `-bash` without its dash), or `interpreter script` / `python -m module`.
/// `comm` when the command line is hidden.
pub fn program(cmd: &str, comm: &str) -> String {
    let base = |s: &str| s.rsplit('/').next().unwrap_or(s).to_string();
    let mut args = cmd.split_whitespace();
    let bin = args.next().map(base).unwrap_or_default();
    let bin = bin.trim_start_matches('-');
    if bin.is_empty() {
        return comm.to_string();
    }
    // `python3.12` is `python`.
    let family = bin.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    if !INTERPRETERS.contains(&family) {
        return bin.to_string();
    }
    while let Some(arg) = args.next() {
        match arg {
            // Inline code: no script to name.
            "-c" | "-e" | "--eval" => break,
            "-m" => {
                return args
                    .next()
                    .map_or(bin.to_string(), |m| format!("{bin} {m}"));
            }
            _ if arg.starts_with('-') => {}
            _ => return format!("{bin} {}", base(arg)),
        }
    }
    bin.to_string()
}

/// Which processes listen where. Kept across readings so the fd walk - one
/// `readlink` per open fd of every pane process - runs only when a socket
/// was opened or closed somewhere, or a process came or went.
#[derive(Debug, Default)]
pub struct Ports {
    key: (Vec<u64>, Vec<u32>),
    owners: HashMap<u32, Vec<SocketAddr>>,
}

impl Ports {
    /// Re-attribute the machine's listening sockets to `pids`.
    pub fn update(&mut self, pids: &[u32]) {
        let listening = listening();
        let mut inodes: Vec<u64> = listening.keys().copied().collect();
        inodes.sort_unstable();
        let mut pids = pids.to_vec();
        pids.sort_unstable();
        pids.dedup();
        let key = (inodes, pids);
        if key == self.key {
            return;
        }
        self.owners.clear();
        if !listening.is_empty() {
            for &pid in &key.1 {
                let mut addrs: Vec<SocketAddr> = socket_inodes(pid)
                    .iter()
                    .filter_map(|inode| listening.get(inode).copied())
                    .collect();
                if !addrs.is_empty() {
                    // One per port, v4 first: the `[::]` twin of a
                    // `0.0.0.0` bind is the same server.
                    addrs.sort_by_key(|a| (a.port(), a.is_ipv6()));
                    addrs.dedup_by_key(|a| a.port());
                    self.owners.insert(pid, addrs);
                }
            }
        }
        self.key = key;
    }

    /// What `pid` listens on, as of the last `update`.
    pub fn of(&self, pid: u32) -> &[SocketAddr] {
        self.owners.get(&pid).map_or(&[], Vec::as_slice)
    }
}

/// Distinct port numbers among `addrs`, ascending: a server bound to both
/// `0.0.0.0:8080` and `[::]:8080` is one server.
pub fn ports(addrs: &[SocketAddr]) -> Vec<u16> {
    let mut ports: Vec<u16> = addrs.iter().map(SocketAddr::port).collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Where a browser on this machine reaches `addr`: a wildcard or loopback
/// bind is `localhost`, anything else its own address.
pub fn url(addr: &SocketAddr) -> String {
    let ip = addr.ip();
    if ip.is_unspecified() || ip.is_loopback() {
        format!("http://localhost:{}", addr.port())
    } else {
        format!("http://{addr}")
    }
}

/// `*:5173` for a wildcard bind, else the address as written.
pub fn label(addr: &SocketAddr) -> String {
    if addr.ip().is_unspecified() {
        format!("*:{}", addr.port())
    } else {
        addr.to_string()
    }
}

/// Every listening TCP socket in this network namespace, by inode.
fn listening() -> HashMap<u64, SocketAddr> {
    let mut out = HashMap::new();
    for (path, v6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
        if let Ok(text) = fs::read_to_string(path) {
            out.extend(parse_net(&text, v6));
        }
    }
    out
}

/// LISTEN rows of a `/proc/net/tcp{,6}` table: `local_address` is field 1,
/// `st` field 3 (`0A` is LISTEN), `inode` field 9.
fn parse_net(text: &str, v6: bool) -> impl Iterator<Item = (u64, SocketAddr)> + '_ {
    text.lines().skip(1).filter_map(move |line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.get(3) != Some(&"0A") {
            return None;
        }
        let (ip, port) = f.get(1)?.split_once(':')?;
        let port = u16::from_str_radix(port, 16).ok()?;
        let ip = if v6 { parse_v6(ip)? } else { parse_v4(ip)? };
        let inode = f.get(9)?.parse().ok()?;
        Some((inode, SocketAddr::new(ip, port)))
    })
}

/// The kernel prints the address's 32-bit words with `%08X` in host byte
/// order, so reading each word back natively recovers the network bytes.
fn parse_v4(hex: &str) -> Option<IpAddr> {
    let word = u32::from_str_radix(hex, 16).ok()?;
    Some(IpAddr::V4(Ipv4Addr::from(word.to_ne_bytes())))
}

fn parse_v6(hex: &str) -> Option<IpAddr> {
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (i, chunk) in bytes.chunks_mut(4).enumerate() {
        let word = u32::from_str_radix(&hex[i * 8..i * 8 + 8], 16).ok()?;
        chunk.copy_from_slice(&word.to_ne_bytes());
    }
    let ip = Ipv6Addr::from(bytes);
    // `::ffff:127.0.0.1` is how a dual-stack socket reports a v4 bind.
    Some(match ip.to_ipv4_mapped() {
        Some(v4) => IpAddr::V4(v4),
        None => IpAddr::V6(ip),
    })
}

/// Inodes of the sockets `pid` holds open. Empty for a process that is gone
/// or not ours to look into.
fn socket_inodes(pid: u32) -> Vec<u64> {
    let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Vec::new();
    };
    fds.filter_map(|fd| {
        let target = fs::read_link(fd.ok()?.path()).ok()?;
        target
            .to_str()?
            .strip_prefix("socket:[")?
            .strip_suffix(']')?
            .parse()
            .ok()
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1 0000000000000000 100 0 0 10 0
   1: 00000000:1435 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4343 1 0000000000000000 100 0 0 10 0
   2: 0100007F:A2B4 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 4444 1 0000000000000000 20 4 30 10 -1
";
    const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000000000000:1435 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 5151 1 0000000000000000 100 0 0 10 0
   1: 0000000000000000FFFF00000100007F:0BB8 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 5252 1 0000000000000000 100 0 0 10 0
";

    #[test]
    #[cfg(target_endian = "little")]
    fn listen_rows_parse_and_connections_do_not() {
        let v4: HashMap<u64, SocketAddr> = parse_net(TCP, false).collect();
        assert_eq!(v4.len(), 2, "the ESTABLISHED row is not a server");
        assert_eq!(v4[&4242], "127.0.0.1:8080".parse().unwrap());
        assert_eq!(v4[&4343], "0.0.0.0:5173".parse().unwrap());

        let v6: HashMap<u64, SocketAddr> = parse_net(TCP6, true).collect();
        assert_eq!(v6[&5151], "[::]:5173".parse().unwrap());
        assert_eq!(v6[&5252], "127.0.0.1:3000".parse().unwrap());
    }

    #[test]
    fn a_dual_stack_server_is_one_port_and_opens_on_localhost() {
        let addrs: Vec<SocketAddr> = vec![
            "0.0.0.0:5173".parse().unwrap(),
            "[::]:5173".parse().unwrap(),
        ];
        assert_eq!(ports(&addrs), vec![5173]);
        assert_eq!(url(&addrs[1]), "http://localhost:5173");
        assert_eq!(label(&addrs[0]), "*:5173");
        let lan: SocketAddr = "192.168.1.4:8000".parse().unwrap();
        assert_eq!(url(&lan), "http://192.168.1.4:8000");
    }

    #[test]
    fn a_tree_lists_children_under_their_parent_in_launch_order() {
        // This test process and its parent are real, so comm is readable.
        let me = std::process::id();
        let parent = std::os::unix::process::parent_id();
        let table = ProcTable::from_parents(&[(parent, 1), (me, parent)]);
        let tree = tree(parent, &table);
        assert_eq!(
            tree.iter().map(|p| (p.pid, p.depth)).collect::<Vec<_>>(),
            vec![(parent, 0), (me, 1)]
        );
    }

    #[test]
    fn a_process_is_named_after_its_program_or_the_script_it_runs() {
        assert_eq!(
            program("/usr/lib/chromium/chromium --type=gpu", "chromium"),
            "chromium"
        );
        assert_eq!(program("-bash", "bash"), "bash");
        assert_eq!(
            program(
                "node --inspect /home/u/app/server.js --port 3",
                "node-MainThread"
            ),
            "node server.js"
        );
        assert_eq!(
            program("python3.12 -m http.server 8080", "python3"),
            "python3.12 http.server"
        );
        assert_eq!(program("sh -c sleep 1000 & python3 x.py", "sh"), "sh");
        assert_eq!(program("", "kworker/0:1"), "kworker/0:1");
    }

    #[test]
    fn our_own_listener_is_found_through_its_fd() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let mut ports = Ports::default();
        ports.update(&[std::process::id()]);
        assert!(
            ports
                .of(std::process::id())
                .iter()
                .any(|a| a.port() == port),
            "{:?}",
            ports.of(std::process::id())
        );
        drop(server);
        ports.update(&[std::process::id()]);
        assert!(
            ports
                .of(std::process::id())
                .iter()
                .all(|a| a.port() != port)
        );
    }
}
