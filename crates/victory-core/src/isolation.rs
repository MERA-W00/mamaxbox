//! Kernel-level network isolation audit and namespace execution.
//!
//! Provides OS kernel-enforced network boundaries so that running titles,
//! native libraries, or translation layers cannot send or receive external
//! network packets.
//!
//! * In a kernel-isolated network namespace (`unshare --net --map-root-user`),
//!   all physical and wireless network interfaces are detached, no default
//!   gateway exists, and outbound packets are dropped by the Linux kernel
//!   (`ENETUNREACH` / `WSAENETUNREACH`).
//! * The loopback interface (`lo` / 127.0.0.1) is explicitly activated,
//!   preserving local IPC (such as the DualSense bridge on `127.0.0.1:28777`).

use std::fs;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

/// Configured network isolation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkIsolation {
    /// Linux kernel network namespace isolation: loopback only, 0 external interfaces/routes.
    #[default]
    Kernel,
    /// WinSock / WinHTTP HLE interception only (user-space emulation).
    Hle,
    /// Network isolation disabled (not recommended).
    Off,
}

impl NetworkIsolation {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "kernel" | "strict" | "namespace" | "netns" => Some(Self::Kernel),
            "hle" | "intercept" => Some(Self::Hle),
            "off" | "none" | "disabled" => Some(Self::Off),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Kernel => "kernel",
            Self::Hle => "hle",
            Self::Off => "off",
        }
    }
}

/// Network configuration settings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetworkConfig {
    pub isolation: NetworkIsolation,
}

/// Results of an active network isolation audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationStatus {
    /// Whether kernel-level network isolation is confirmed active.
    pub is_kernel_isolated: bool,
    /// Whether local loopback (127.0.0.1) is operational for IPC.
    pub loopback_available: bool,
    /// Non-loopback network interfaces detected in the active namespace.
    pub external_interfaces: Vec<String>,
    /// Number of routing entries present in `/proc/net/route`.
    pub routes_present: usize,
    /// Whether outbound non-loopback connection attempts are rejected by the kernel.
    pub outbound_blocked: bool,
    /// Human-readable diagnostic details.
    pub details: String,
}

impl IsolationStatus {
    pub fn summary(&self) -> String {
        if self.is_kernel_isolated {
            format!(
                "Kernel network isolation: ACTIVE (loopback: ok, external interfaces: none, routes: 0, outbound: kernel-blocked)"
            )
        } else {
            format!(
                "Kernel network isolation: INACTIVE (external interfaces: {:?}, routes: {}, outbound blocked: {})",
                self.external_interfaces, self.routes_present, self.outbound_blocked
            )
        }
    }
}

impl std::fmt::Display for IsolationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.summary())
    }
}

/// Locates `/proc/net` files whether running natively on Linux or under Wine/Proton.
fn find_proc_file(subpath: &str) -> Option<PathBuf> {
    let native = PathBuf::from(format!("/proc/net/{subpath}"));
    if native.is_file() {
        return Some(native);
    }
    let wine_z = PathBuf::from(format!("Z:\\proc\\net\\{subpath}"));
    if wine_z.is_file() {
        return Some(wine_z);
    }
    let wine_z_slash = PathBuf::from(format!("Z:/proc/net/{subpath}"));
    if wine_z_slash.is_file() {
        return Some(wine_z_slash);
    }
    None
}

/// Inspects `/proc/net/dev` to list all network interfaces present in the current network namespace.
pub fn scan_interfaces() -> Vec<String> {
    let mut interfaces = Vec::new();
    if let Some(path) = find_proc_file("dev") {
        if let Ok(content) = fs::read_to_string(&path) {
            for line in content.lines() {
                if let Some((iface, _)) = line.split_once(':') {
                    let name = iface.trim().to_string();
                    if !name.is_empty() {
                        interfaces.push(name);
                    }
                }
            }
        }
    }
    interfaces
}

/// Counts routes present in `/proc/net/route`. In an isolated netns, this count is 0.
pub fn count_routes() -> usize {
    if let Some(path) = find_proc_file("route") {
        if let Ok(content) = fs::read_to_string(&path) {
            let lines: Vec<&str> = content.lines().collect();
            // First line is header (`Iface Destination...`), subsequent lines are active routes
            return lines.len().saturating_sub(1);
        }
    }
    0
}

/// Probes whether the OS kernel immediately denies outbound non-loopback connections.
///
/// Uses RFC 5737 TEST-NET-2 address `198.51.100.1:80`. In a network namespace with no
/// default route and no external interface, the kernel rejects connection setup with
/// `ENETUNREACH` / `WSAENETUNREACH`.
pub fn probe_outbound_blocked() -> bool {
    let target: SocketAddr = "198.51.100.1:80".parse().unwrap();
    match UdpSocket::bind("0.0.0.0:0") {
        Ok(socket) => match socket.connect(target) {
            Ok(_) => false, // Successfully routed: network interface/gateway is available
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                // Linux ENETUNREACH = 101, EHOSTUNREACH = 113, Windows WSAENETUNREACH = 10051
                code == 101 || code == 113 || code == 10051 || e.kind() == io::ErrorKind::NetworkUnreachable
            }
        },
        Err(_) => true,
    }
}

/// Probes whether local loopback (127.0.0.1) is available for localhost IPC.
pub fn probe_loopback_available() -> bool {
    UdpSocket::bind("127.0.0.1:0").is_ok()
}

/// Performs a comprehensive audit of the active network environment.
pub fn audit_network_isolation() -> IsolationStatus {
    let all_interfaces = scan_interfaces();
    let external_interfaces: Vec<String> = all_interfaces
        .into_iter()
        .filter(|name| name != "lo")
        .collect();
    let routes_present = count_routes();
    let outbound_blocked = probe_outbound_blocked();
    let loopback_available = probe_loopback_available();

    let has_no_external_ifaces = external_interfaces.is_empty();
    let has_no_routes = routes_present == 0;

    let is_kernel_isolated = has_no_external_ifaces && has_no_routes && outbound_blocked && loopback_available;

    let details = if is_kernel_isolated {
        "Linux network namespace active with loopback only. Outbound packets are dropped at kernel level.".to_string()
    } else {
        format!(
            "Process has access to host network interfaces: {:?} with {} active route(s).",
            external_interfaces, routes_present
        )
    };

    IsolationStatus {
        is_kernel_isolated,
        loopback_available,
        external_interfaces,
        routes_present,
        outbound_blocked,
        details,
    }
}

/// Verifies that kernel-level network isolation is currently active, returning an error if not.
pub fn require_kernel_isolation() -> Result<IsolationStatus> {
    let status = audit_network_isolation();
    if !status.is_kernel_isolated {
        bail!(
            "Kernel-level network isolation check failed:\n  {}\n  Outbound blocked: {}\n  Run under an isolated namespace using `unshare --net --map-root-user` or `scripts/victory-isolate.sh`.",
            status.details,
            status.outbound_blocked
        );
    }
    Ok(status)
}

/// Constructs a `std::process::Command` configured to execute the given target inside
/// an isolated Linux user+network namespace with loopback enabled.
pub fn build_isolated_command<P: AsRef<Path>, I: IntoIterator<Item = S>, S: AsRef<std::ffi::OsStr>>(
    program: P,
    args: I,
) -> Command {
    let program = program.as_ref();
    let mut cmd = Command::new("unshare");
    cmd.args(["--net", "--map-root-user", "sh", "-c", "ip link set lo up 2>/dev/null || true; exec \"$@\"", "--"]);
    cmd.arg(program);
    cmd.args(args);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_isolation_parse() {
        assert_eq!(NetworkIsolation::parse("kernel"), Some(NetworkIsolation::Kernel));
        assert_eq!(NetworkIsolation::parse("STRICT"), Some(NetworkIsolation::Kernel));
        assert_eq!(NetworkIsolation::parse("netns"), Some(NetworkIsolation::Kernel));
        assert_eq!(NetworkIsolation::parse("hle"), Some(NetworkIsolation::Hle));
        assert_eq!(NetworkIsolation::parse("off"), Some(NetworkIsolation::Off));
        assert_eq!(NetworkIsolation::parse("none"), Some(NetworkIsolation::Off));
        assert_eq!(NetworkIsolation::parse("invalid"), None);
    }

    #[test]
    fn loopback_is_available() {
        assert!(probe_loopback_available());
    }

    #[test]
    fn audit_status_runs_without_panic() {
        let status = audit_network_isolation();
        assert!(status.loopback_available);
        let summary = status.summary();
        assert!(!summary.is_empty());
    }
}

