//! Endpoint construction, path classification, and the reachability canary.

use crate::proto::{PathKind, PROBE_ALPN, PROBE_PING, PROBE_PONG};
use anyhow::{ensure, Context, Result};
use iroh::address_lookup::{DnsAddressLookup, PkarrPublisher};
use iroh::endpoint::{presets, Connection};
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, RelayUrl, SecretKey, TransportAddr,
};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// Where the relay comes from. `Urls` exists so a target with no working DNS can
/// be handed an IP-literal relay URL on the command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayChoice {
    N0,
    Disabled,
    Urls(Vec<String>),
}

impl RelayChoice {
    pub fn from_flags(no_relay: bool, urls: &[String]) -> Self {
        match (no_relay, urls.is_empty()) {
            (true, _) => RelayChoice::Disabled,
            (false, true) => RelayChoice::N0,
            (false, false) => RelayChoice::Urls(urls.to_vec()),
        }
    }

    fn mode(&self) -> Result<RelayMode> {
        Ok(match self {
            RelayChoice::N0 => RelayMode::Default,
            RelayChoice::Disabled => RelayMode::Disabled,
            RelayChoice::Urls(urls) => RelayMode::Custom(
                RelayMap::try_from_iter(urls.iter().map(String::as_str))
                    .context("parsing --relay url")?,
            ),
        })
    }

    pub fn enabled(&self) -> bool {
        !matches!(self, RelayChoice::Disabled)
    }
}

/// How an endpoint participates in n0's DNS-based address lookup.
///
/// Split into publish and resolve because the two roles need different halves:
/// the controller must publish so agents can find it by NodeId, the agent must
/// never publish (a target should leave no record of itself) and need not
/// resolve at all when it was given `--direct`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lookup {
    pub publish: bool,
    pub resolve: bool,
}

impl Lookup {
    pub const NONE: Lookup = Lookup {
        publish: false,
        resolve: false,
    };
    pub const RESOLVE: Lookup = Lookup {
        publish: false,
        resolve: true,
    };
    pub const BOTH: Lookup = Lookup {
        publish: true,
        resolve: true,
    };
}

/// `bind_addr` pins the UDP socket. A controller that keeps its port across
/// restarts is what lets a target be given a fixed `--direct` address and no DNS
/// at all; leave it `None` to let the OS choose, which is right everywhere else.
pub async fn bind(
    secret: Option<SecretKey>,
    relay: &RelayChoice,
    lookup: Lookup,
    bind_addr: Option<SocketAddr>,
) -> Result<Endpoint> {
    let mut b = Endpoint::builder(presets::Minimal).relay_mode(relay.mode()?);
    if let Some(addr) = bind_addr {
        b = b
            .clear_ip_transports()
            .bind_addr(addr)
            .map_err(|e| anyhow::anyhow!("binding {addr}: {e}"))?;
    }
    if let Some(secret) = secret {
        b = b.secret_key(secret);
    }
    if lookup.publish {
        b = b.address_lookup(PkarrPublisher::n0_dns());
    }
    if lookup.resolve {
        b = b.address_lookup(DnsAddressLookup::n0_dns());
    }
    let ep = b.bind().await.context("binding iroh endpoint")?;
    if relay.enabled() {
        // Holepunching needs a home relay, but a slow or unreachable relay must
        // not wedge startup: with relays disabled `online()` never returns at all.
        let _ = tokio::time::timeout(Duration::from_secs(10), ep.online()).await;
    }
    Ok(ep)
}

pub fn classify(addr: &TransportAddr) -> PathKind {
    match addr {
        TransportAddr::Relay(_) => PathKind::Relayed,
        TransportAddr::Ip(sa) => classify_ip(sa.ip()),
        // Never guess: a path we cannot identify is reported as unknown rather
        // than as direct. Over-reporting "direct" is the failure mode that matters.
        _ => PathKind::Unknown,
    }
}

pub fn classify_ip(ip: IpAddr) -> PathKind {
    if ip.is_loopback() {
        return PathKind::DirectLocal;
    }
    let private = match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_unspecified(),
        // fc00::/7 (unique local) and fe80::/10 (link local).
        IpAddr::V6(v6) => {
            let o = v6.octets();
            (o[0] & 0xfe) == 0xfc || (o[0] == 0xfe && (o[1] & 0xc0) == 0x80) || v6.is_unspecified()
        }
    };
    if private {
        PathKind::DirectLan
    } else {
        PathKind::DirectWan
    }
}

pub fn describe(addr: &TransportAddr) -> String {
    match addr {
        TransportAddr::Ip(sa) => sa.to_string(),
        TransportAddr::Relay(url) => url.to_string(),
        other => format!("{other:?}"),
    }
}

/// The path a connection is actually transmitting on right now.
///
/// iroh may hold several paths open at once (typically relay plus a direct one
/// discovered by holepunching); the *selected* path is the one carrying data, so
/// that is what gets reported. `None` only while no path is open at all, which a
/// live connection does not stay in.
pub fn session_path(conn: &Connection) -> Option<(PathKind, String)> {
    let paths = conn.paths();
    let selected = paths.iter().find(|p| p.is_selected());
    let path = selected.or_else(|| paths.iter().next())?;
    let addr = path.remote_addr();
    Some((classify(addr), describe(addr)))
}

/// The one line every session must report, with no way to accidentally omit it.
pub fn session_path_report(conn: &Connection) -> (PathKind, String) {
    session_path(conn).unwrap_or((PathKind::Unknown, "no open path".to_string()))
}

pub fn endpoint_addr(
    id: EndpointId,
    direct: &[SocketAddr],
    relay: Option<&RelayUrl>,
) -> EndpointAddr {
    let mut addr = EndpointAddr::new(id);
    for a in direct {
        addr = addr.with_ip_addr(*a);
    }
    if let Some(url) = relay {
        addr = addr.with_relay_url(url.clone());
    }
    addr
}

/// How a probe addressed the controller — worth recording, because a probe that
/// was handed the answer proves much less than one that had to look it up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeMode {
    /// Dialled by NodeId alone: exercises publish + lookup, the failure the
    /// prior art actually hit (a controller with a healthy relay that nobody
    /// could resolve for minutes).
    Discovery,
    /// Dialled by NodeId plus the controller's own socket addresses: proves the
    /// socket is reachable, proves nothing about discovery. Used when discovery
    /// is switched off.
    Direct,
}

/// Dials the controller's own NodeId from a throwaway endpoint with a fresh key,
/// i.e. as a stranger would. Success means "an agent booting right now could
/// reach me"; that is the only claim worth making about reachability.
pub async fn probe(
    target: EndpointId,
    mode: ProbeMode,
    direct: &[SocketAddr],
    relay: &RelayChoice,
    timeout: Duration,
) -> Result<PathKind> {
    let lookup = match mode {
        ProbeMode::Discovery => Lookup::RESOLVE,
        ProbeMode::Direct => Lookup::NONE,
    };
    let ep = bind(None, relay, lookup, None).await?;
    let addr = match mode {
        ProbeMode::Discovery => EndpointAddr::new(target),
        ProbeMode::Direct => endpoint_addr(target, direct, None),
    };
    let result = tokio::time::timeout(timeout, async {
        let conn = ep.connect(addr, PROBE_ALPN).await.context("dialling self")?;
        let (path, _) = session_path_report(&conn);
        let (mut send, mut recv) = conn.open_bi().await?;
        send.write_all(PROBE_PING).await?;
        send.finish()?;
        let pong = recv.read_to_end(PROBE_PONG.len()).await?;
        ensure!(pong == PROBE_PONG, "unexpected probe response");
        conn.close(0u32.into(), b"probe done");
        anyhow::Ok(path)
    })
    .await;
    ep.close().await;
    match result {
        Ok(r) => r,
        Err(_) => anyhow::bail!("probe timed out after {timeout:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn path_classification() {
        assert_eq!(
            classify_ip(Ipv4Addr::LOCALHOST.into()),
            PathKind::DirectLocal
        );
        assert_eq!(classify_ip(Ipv6Addr::LOCALHOST.into()), PathKind::DirectLocal);
        assert_eq!(
            classify_ip(Ipv4Addr::new(192, 168, 1, 7).into()),
            PathKind::DirectLan
        );
        assert_eq!(
            classify_ip(Ipv4Addr::new(10, 0, 2, 15).into()),
            PathKind::DirectLan
        );
        assert_eq!(
            classify_ip("fe80::1".parse::<Ipv6Addr>().unwrap().into()),
            PathKind::DirectLan
        );
        assert_eq!(
            classify_ip("fd00::1".parse::<Ipv6Addr>().unwrap().into()),
            PathKind::DirectLan
        );
        assert_eq!(
            classify_ip(Ipv4Addr::new(93, 184, 216, 34).into()),
            PathKind::DirectWan
        );
    }

    #[test]
    fn relay_addr_is_never_reported_as_direct() {
        let url: RelayUrl = "https://relay.example.invalid./".parse().unwrap();
        assert_eq!(classify(&TransportAddr::Relay(url)), PathKind::Relayed);
        assert_eq!(
            classify(&TransportAddr::Ip("127.0.0.1:9".parse().unwrap())),
            PathKind::DirectLocal
        );
    }

    #[test]
    fn relay_choice_flags() {
        assert_eq!(RelayChoice::from_flags(true, &[]), RelayChoice::Disabled);
        assert_eq!(RelayChoice::from_flags(false, &[]), RelayChoice::N0);
        assert_eq!(
            RelayChoice::from_flags(false, &["https://1.2.3.4/".to_string()]),
            RelayChoice::Urls(vec!["https://1.2.3.4/".to_string()])
        );
        // An IP-literal relay URL is the DNS-free path and must parse.
        assert!(RelayChoice::Urls(vec!["https://1.2.3.4/".into()])
            .mode()
            .is_ok());
    }

    /// The canary must *fail* against a NodeId that is not there, or it cannot
    /// distinguish "nobody dialled me today" from "nobody can find me".
    #[tokio::test]
    async fn probe_of_a_nonexistent_endpoint_fails() {
        let nowhere = SecretKey::from_bytes(&[9u8; 32]).public();
        let err = probe(
            nowhere,
            ProbeMode::Direct,
            &["127.0.0.1:1".parse().unwrap()],
            &RelayChoice::Disabled,
            Duration::from_secs(3),
        )
        .await
        .unwrap_err();
        let _ = err;
    }
}
