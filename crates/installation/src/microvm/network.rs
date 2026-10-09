//! Per-slot tap device and firewall chain of a guest.
use super::host::command;
use crate::error::{Error, Result};
use std::{process::Stdio, time::Duration};
use tokio::process::Command;

/// Destinations no guest may reach: the runner, peer VMs, LAN and cloud metadata.
const PRIVATE_SUBNETS: [&str; 8] = [
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "224.0.0.0/3",
];

pub(super) struct Network {
    pub tap: String,
    chain: String,
    pub guest: String,
    pub gateway: String,
    pub mac: String,
}

async fn iptables(args: &[&str]) -> Result<()> {
    let mut all = vec!["-w", "5"];
    all.extend_from_slice(args);
    command("iptables", &all).await
}

/// Best-effort teardown step; a rule may already be gone.
async fn quietly(binary: &str, args: &[&str]) {
    let status = Command::new(binary)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status();
    let _ = tokio::time::timeout(Duration::from_secs(10), status).await;
}

impl Network {
    pub fn new(slot: usize) -> Result<Self> {
        // Each live slot owns a distinct /30 in private 10.0.0.0/8. Check
        // address exhaustion instead of truncating slot IDs above one byte.
        let subnet = u32::try_from(slot)
            .ok()
            .filter(|slot| *slot > 0)
            .and_then(|slot| slot.checked_mul(4))
            .filter(|subnet| *subnet < (1 << 24))
            .ok_or_else(|| Error::unavailable("VM private IPv4 address space is exhausted."))?;
        let base = u32::from(std::net::Ipv4Addr::new(10, 0, 0, 0)) + subnet;
        let bytes = (slot as u32).to_be_bytes();
        Ok(Self {
            tap: format!("cairn{slot}"),
            chain: format!("CAIRN{slot}"),
            guest: std::net::Ipv4Addr::from(base + 2).to_string(),
            gateway: std::net::Ipv4Addr::from(base + 1).to_string(),
            mac: format!(
                "06:00:{:02x}:{:02x}:{:02x}:{:02x}",
                bytes[0], bytes[1], bytes[2], bytes[3]
            ),
        })
    }

    fn input_rule(&self, action: &'static str) -> [&str; 6] {
        [action, "INPUT", "-i", &self.tap, "-j", "DROP"]
    }

    fn forward_rule(&self, action: &'static str) -> [&str; 6] {
        [action, "FORWARD", "-i", &self.tap, "-j", &self.chain]
    }

    fn established_rule(&self, action: &'static str) -> [&str; 10] {
        [
            action,
            "FORWARD",
            "-o",
            &self.tap,
            "-m",
            "conntrack",
            "--ctstate",
            "ESTABLISHED,RELATED",
            "-j",
            "ACCEPT",
        ]
    }

    fn masquerade_rule(&self, action: &'static str) -> [&str; 8] {
        [
            "-t",
            "nat",
            action,
            "POSTROUTING",
            "-s",
            &self.guest,
            "-j",
            "MASQUERADE",
        ]
    }

    pub async fn create(&self, uid: u32) -> Result<()> {
        let uid = uid.to_string();
        let address = format!("{}/30", self.gateway);
        command(
            "ip",
            &[
                "tuntap", "add", "dev", &self.tap, "mode", "tap", "user", &uid,
            ],
        )
        .await?;
        command("ip", &["addr", "add", &address, "dev", &self.tap]).await?;
        command("ip", &["link", "set", &self.tap, "up"]).await?;
        iptables(&["-N", &self.chain]).await?;
        iptables(&self.input_rule("-I")).await?;
        iptables(&self.forward_rule("-I")).await?;
        iptables(&["-A", &self.chain, "!", "-s", &self.guest, "-j", "DROP"]).await?;
        for subnet in PRIVATE_SUBNETS {
            iptables(&["-A", &self.chain, "-d", subnet, "-j", "DROP"]).await?;
        }
        // Public TCP includes SSH on custom ports; UDP remains limited to DNS.
        // Keep both rules after the private-destination and source-address checks.
        iptables(&["-A", &self.chain, "-p", "tcp", "-j", "ACCEPT"]).await?;
        iptables(&[
            "-A",
            &self.chain,
            "-p",
            "udp",
            "--dport",
            "53",
            "-j",
            "ACCEPT",
        ])
        .await?;
        iptables(&["-A", &self.chain, "-j", "DROP"]).await?;
        iptables(&self.established_rule("-I")).await?;
        iptables(&self.masquerade_rule("-A")).await
    }

    pub async fn remove(&self) {
        let rules: [&[&str]; 6] = [
            &self.input_rule("-D"),
            &self.forward_rule("-D"),
            &self.established_rule("-D"),
            &self.masquerade_rule("-D"),
            &["-F", &self.chain],
            &["-X", &self.chain],
        ];
        for rule in rules {
            let mut args = vec!["-w", "5"];
            args.extend_from_slice(rule);
            quietly("iptables", &args).await;
        }
        quietly("ip", &["link", "del", &self.tap]).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_networks_remain_distinct_above_four_and_one_byte() {
        let mut addresses = std::collections::HashSet::new();
        let mut macs = std::collections::HashSet::new();
        for slot in [1, 4, 5, 12, 255, 256, 257, 65536, (1 << 22) - 1] {
            let network = Network::new(slot).unwrap();
            assert!(addresses.insert(network.guest.clone()));
            assert!(addresses.insert(network.gateway.clone()));
            assert!(macs.insert(network.mac.clone()));
            assert!(network.tap.len() < 16);
            assert_eq!(network.mac.split(':').count(), 6);
            assert!(network.mac.split(':').all(|octet| octet.len() == 2));
            let guest: std::net::Ipv4Addr = network.guest.parse().unwrap();
            let gateway: std::net::Ipv4Addr = network.gateway.parse().unwrap();
            assert!(guest.is_private());
            assert_eq!(u32::from(guest) - u32::from(gateway), 1);
            assert_eq!(u32::from(guest) / 4, u32::from(gateway) / 4);
        }
        for slot in [0, 1 << 22, usize::MAX] {
            assert!(Network::new(slot).is_err());
        }
    }
}
