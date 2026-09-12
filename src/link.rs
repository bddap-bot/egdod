//! Anyone holding the (public) node id can compute the PSK and join the access
//! point. That buys them a link to an agent that dials only the controller's
//! node id and serves nothing before approval — the same position as anyone on
//! any network the target ever joins. The approval-by-public-key boundary is
//! untouched; the PSK exists to keep casual radios off the link, not to
//! authenticate anyone.

use crate::proto::hex;
use crate::state::StateDir;
use anyhow::{bail, Context, Result};
use iroh::EndpointId;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::net::Ipv4Addr;
use std::time::Duration;

pub const PREFIX_LEN: u8 = 16;
const JOIN_WAIT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Link {
    pub ssid: String,
    pub psk: String,
    pub target: Ipv4Addr,
    pub controller: Ipv4Addr,
    pub port: u16,
}

fn hash(label: &str, id: &EndpointId) -> [u8; 32] {
    Sha256::new()
        .chain_update(label.as_bytes())
        .chain_update(id.as_bytes())
        .finalize()
        .into()
}

fn host_octet(b: u8) -> u8 {
    1 + b % 254
}

pub fn derive(id: &EndpointId) -> Link {
    let ssid = hash("egdod-link-ssid", id);
    let psk = hash("egdod-link-psk", id);
    let a = hash("egdod-link-addr", id);
    let x = host_octet(a[0]);
    let y = host_octet(a[1]);
    let mut z = host_octet(a[2]);
    if z == y {
        z = host_octet(z);
    }
    Link {
        ssid: format!("egdod-{}", hex(&ssid[..4])),
        psk: hex(&psk),
        target: Ipv4Addr::new(169, 254, x, y),
        controller: Ipv4Addr::new(169, 254, x, z),
        port: 49152 + u16::from_be_bytes([a[3], a[4]]) % 16384,
    }
}

impl Link {
    pub fn hostapd_conf(&self, iface: &str) -> String {
        format!(
            "interface={iface}\ndriver=nl80211\nssid={}\nhw_mode=g\nchannel=6\nwpa=2\nwpa_key_mgmt=WPA-PSK\nrsn_pairwise=CCMP\nwpa_psk={}\n",
            self.ssid, self.psk
        )
    }

    pub fn supplicant_conf(&self) -> String {
        format!(
            "network={{\n\tssid=\"{}\"\n\tpsk={}\n\tkey_mgmt=WPA-PSK\n}}\n",
            self.ssid, self.psk
        )
    }

    pub fn lines(&self) -> String {
        format!(
            "ssid={}\npsk={}\ntarget={}\ncontroller={}\nprefix={PREFIX_LEN}\nport={}\n",
            self.ssid, self.psk, self.target, self.controller, self.port
        )
    }
}

pub enum Show {
    Lines,
    Json,
    Hostapd(String),
    Supplicant,
}

pub fn show(id: &EndpointId, what: Show) -> Result<()> {
    let link = derive(id);
    let text = match what {
        Show::Lines => link.lines(),
        Show::Json => format!("{}\n", serde_json::to_string_pretty(&link)?),
        Show::Hostapd(iface) => link.hostapd_conf(&iface),
        Show::Supplicant => link.supplicant_conf(),
    };
    print!("{text}");
    Ok(())
}

fn ip(args: &[&str]) -> Result<()> {
    let status = std::process::Command::new("ip")
        .args(args)
        .status()
        .with_context(|| format!("running ip {}", args.join(" ")))?;
    if !status.success() {
        bail!("ip {} failed with {status}", args.join(" "));
    }
    Ok(())
}

fn carrier(iface: &str) -> bool {
    std::fs::read_to_string(format!("/sys/class/net/{iface}/carrier"))
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

pub async fn join(state: &StateDir, iface: &str) -> Result<()> {
    let link = derive(&state.load_key()?.public());
    eprintln!("egdod: joining {} on {iface}; the target dials {}:{}", link.ssid, link.controller, link.port);
    let conf = state.root().join("link.conf");
    std::fs::write(&conf, link.supplicant_conf())
        .with_context(|| format!("writing {}", conf.display()))?;
    let cidr = format!("{}/{PREFIX_LEN}", link.controller);
    ip(&["link", "set", "dev", iface, "up"])?;
    ip(&["addr", "replace", &cidr, "dev", iface])?;
    let mut child = tokio::process::Command::new("wpa_supplicant")
        .args(["-i", iface, "-c"])
        .arg(&conf)
        .kill_on_drop(true)
        .spawn()
        .context("spawning wpa_supplicant")?;
    let outcome = async {
        let deadline = tokio::time::Instant::now() + JOIN_WAIT;
        while !carrier(iface) {
            if tokio::time::Instant::now() > deadline {
                bail!("{} not in range within {JOIN_WAIT:?}", link.ssid);
            }
            if let Some(status) = child.try_wait()? {
                bail!("wpa_supplicant exited with {status}");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        eprintln!(
            "egdod: joined {} on {iface} as {cidr}; the target dials {}:{}. Ctrl-C to leave.",
            link.ssid, link.controller, link.port
        );
        tokio::select! {
            r = tokio::signal::ctrl_c() => r.context("waiting for ctrl-c"),
            s = child.wait() => bail!("wpa_supplicant exited with {}", s?),
        }
    }
    .await;
    let _ = child.kill().await;
    let _ = ip(&["addr", "del", &cidr, "dev", iface]);
    let _ = std::fs::remove_file(&conf);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    const VECTOR_ID: &str = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";

    fn id(byte: u8) -> EndpointId {
        SecretKey::from_bytes(&[byte; 32]).public()
    }

    #[test]
    fn derivation_is_a_pure_function_of_the_node_id() {
        assert_eq!(derive(&id(1)), derive(&id(1)));
        assert_ne!(derive(&id(1)).psk, derive(&id(2)).psk);
        assert_ne!(derive(&id(1)).ssid, derive(&id(2)).ssid);
    }

    #[test]
    fn known_vector() {
        let id = crate::proto::parse_pubkey(VECTOR_ID).unwrap();
        let l = derive(&id);
        assert_eq!(l.ssid.len(), "egdod-".len() + 8);
        assert_eq!(l.psk.len(), 64);
        let text = l.lines();
        let expected = concat!(
            "ssid=egdod-dd18e097\n",
            "psk=1ecadfc44a85a62d00e8038907d4da4aece04de0d4577df6e468b0417bb94038\n",
            "target=169.254.200.131\n",
            "controller=169.254.200.129\n",
            "prefix=16\n",
            "port=58263\n"
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn addresses_are_link_local_distinct_and_never_reserved() {
        for b in 0..=255u8 {
            let l = derive(&id(b));
            assert_ne!(l.target, l.controller);
            for a in [l.target, l.controller] {
                let o = a.octets();
                assert_eq!((o[0], o[1]), (169, 254));
                assert!((1..=254).contains(&o[2]), "{a}");
                assert!((1..=254).contains(&o[3]), "{a}");
            }
            assert!(l.port >= 49152);
        }
    }

    #[test]
    fn confs_carry_the_raw_psk_not_a_passphrase() {
        let l = derive(&id(3));
        let h = l.hostapd_conf("wlan0");
        assert!(h.contains(&format!("wpa_psk={}\n", l.psk)));
        assert!(!h.contains("wpa_passphrase"));
        assert!(h.starts_with("interface=wlan0\n"));
        let ssid = format!("ssid={}", l.ssid);
        for line in ["driver=nl80211", "wpa=2", "wpa_key_mgmt=WPA-PSK", "rsn_pairwise=CCMP", ssid.as_str()] {
            assert!(h.lines().any(|x| x == line), "hostapd conf lacks {line}");
        }
        assert!(!h.contains("TKIP"));
        let s = l.supplicant_conf();
        assert!(s.contains(&format!("psk={}\n", l.psk)));
        assert!(s.contains(&format!("ssid=\"{}\"\n", l.ssid)));
        assert!(s.lines().any(|x| x.trim() == "key_mgmt=WPA-PSK"));
    }
}
