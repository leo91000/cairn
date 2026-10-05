use ipnet::IpNet;
use std::{net::IpAddr, str::FromStr};

/// Exact addresses or CIDRs of proxies that append the actual peer to X-Forwarded-For.
/// An empty configuration never trusts a browser-supplied forwarding header.
#[derive(Clone, Default)]
pub struct TrustedProxies(Vec<IpNet>);

impl FromStr for TrustedProxies {
    type Err = &'static str;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.trim().is_empty() {
            return Ok(Self::default());
        }

        let entries = input.split(',').map(|entry| {
            let entry = entry.trim();
            entry.parse::<IpNet>().or_else(|_| entry.parse::<IpAddr>().map(IpNet::from))
                .map_err(|_| "LEO_OFFICIAL_TRUSTED_PROXIES requires comma-separated IP addresses or CIDRs")
        }).collect::<Result<Vec<_>, _>>()?;

        Ok(Self(entries))
    }
}

impl TrustedProxies {
    fn contains(&self, address: IpAddr) -> bool {
        self.0
            .iter()
            .any(|network| network.contains(&address) || network.contains(&address.to_canonical()))
    }

    fn client_address(&self, peer: IpAddr, headers: &axum::http::HeaderMap) -> IpAddr {
        if !self.contains(peer) {
            return peer.to_canonical();
        }

        // Traverse only the trusted suffix. Untrusted left-hand text cannot
        // force fallback to the shared proxy quota or select a different client.
        for value in headers.get_all("x-forwarded-for").iter().rev() {
            let Ok(value) = value.to_str() else {
                return peer.to_canonical();
            };

            for entry in value.rsplit(',') {
                let Ok(address) = entry.trim().parse::<IpAddr>() else {
                    return peer.to_canonical();
                };

                if !self.contains(address) {
                    return address.to_canonical();
                }
            }
        }

        peer.to_canonical()
    }
}

pub(super) async fn client_peer(
    axum::extract::State(proxies): axum::extract::State<TrustedProxies>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if let Some(axum::extract::ConnectInfo(peer)) = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .copied()
    {
        let address = proxies.client_address(peer.ip(), request.headers());
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::new(
                address,
                peer.port(),
            )));
    }

    next.run(request).await
}
