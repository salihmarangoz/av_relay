//! Finding the paired Mac again when its address changed (DHCP): its
//! Bonjour service `_avrelay._tcp` carries its device ID in the TXT record
//! (docs/protocol.md, section 3).

use std::net::IpAddr;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent};

const SERVICE: &str = "_avrelay._tcp.local.";

/// Looks for the Mac with `device_id` for up to `wait`; returns its address
/// and port. Blocking: run it off the async threads.
pub fn find_mac(device_id: &str, wait: Duration) -> Option<(IpAddr, u16)> {
    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            log::warn!("mDNS unavailable: {e}");
            return None;
        }
    };
    let found = (|| {
        let events = daemon.browse(SERVICE).ok()?;
        let deadline = Instant::now() + wait;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Ok(event) = events.recv_timeout(left) else { break };
            if let ServiceEvent::ServiceResolved(service) = event {
                if service.get_property_val_str("id") != Some(device_id) {
                    continue;
                }
                // Prefer IPv4: the user typed one when pairing, and it's what
                // ICE will use.
                let mut addrs: Vec<IpAddr> = service.get_addresses().iter().map(|a| a.to_ip_addr()).collect();
                addrs.sort_by_key(|a| (!a.is_ipv4(), a.to_string()));
                if let Some(addr) = addrs.into_iter().find(|a| !is_link_local(a)) {
                    return Some((addr, service.get_port()));
                }
            }
        }
        None
    })();
    let _ = daemon.shutdown();
    found
}

fn is_link_local(addr: &IpAddr) -> bool {
    match addr {
        IpAddr::V4(a) => a.is_link_local(),
        IpAddr::V6(a) => (a.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// An AV Relay Mac found on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundMac {
    pub name: String,
    pub address: IpAddr,
    pub port: u16,
    pub device_id: Option<String>,
}

/// Lists the AV Relay Macs that answer within `wait`, to offer them when
/// pairing. Blocking.
pub fn browse(wait: Duration) -> Vec<FoundMac> {
    let Ok(daemon) = ServiceDaemon::new() else { return Vec::new() };
    let mut found: Vec<FoundMac> = Vec::new();
    if let Ok(events) = daemon.browse(SERVICE) {
        let deadline = Instant::now() + wait;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Ok(event) = events.recv_timeout(left) else { break };
            let ServiceEvent::ServiceResolved(service) = event else { continue };
            let mut addrs: Vec<IpAddr> = service.get_addresses().iter().map(|a| a.to_ip_addr()).collect();
            addrs.sort_by_key(|a| (!a.is_ipv4(), a.to_string()));
            let Some(address) = addrs.into_iter().find(|a| !is_link_local(a)) else { continue };
            let name = service.fullname.strip_suffix(&format!(".{SERVICE}")).unwrap_or(&service.fullname).replace("\\ ", " ");
            let mac = FoundMac {
                name,
                address,
                port: service.get_port(),
                device_id: service.get_property_val_str("id").map(str::to_owned),
            };
            if !found.contains(&mac) {
                found.push(mac);
            }
        }
    }
    let _ = daemon.shutdown();
    found
}
