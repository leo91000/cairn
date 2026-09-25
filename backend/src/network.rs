use crate::error::{Error, Result};
use futures_util::StreamExt;
use serde_json::Value;
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

type ClientCache = HashMap<(String, Vec<SocketAddr>), (Instant, reqwest::Client)>;

fn pinned_client(host: &str, addresses: &[SocketAddr]) -> Result<reqwest::Client> {
    static CLIENTS: OnceLock<Mutex<ClientCache>> = OnceLock::new();
    let mut addresses = addresses.to_vec();
    addresses.sort_unstable();
    addresses.dedup();
    let key = (host.to_owned(), addresses);
    let mut clients = CLIENTS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| Error::internal("HTTP client cache unavailable"))?;
    clients.retain(|_, (used, _)| used.elapsed() < Duration::from_secs(60));
    if let Some((used, client)) = clients.get_mut(&key) {
        *used = Instant::now();
        return Ok(client.clone());
    }
    if clients.len() >= 64
        && let Some(oldest) = clients
            .iter()
            .min_by_key(|(_, (used, _))| *used)
            .map(|(key, _)| key.clone())
    {
        clients.remove(&oldest);
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, &key.1)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(4)
        .build()
        .map_err(|_| Error::internal("Unable to create MCP HTTP client"))?;
    clients.insert(key, (Instant::now(), client.clone()));
    Ok(client)
}

pub fn private(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let n = u32::from(ip);
            [
                (0, 8),
                (0x0a000000, 8),
                (0x64400000, 10),
                (0x7f000000, 8),
                (0xa9fe0000, 16),
                (0xac100000, 12),
                (0xc0a80000, 16),
                (0xc0000000, 24),
                (0xc6120000, 15),
                (0xe0000000, 4),
                (0xf0000000, 4),
            ]
            .iter()
            .any(|(base, bits)| n >> (32 - bits) == base >> (32 - bits))
        }
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return private(IpAddr::V4(ip));
            }
            let bytes = ip.octets();
            ip.is_unspecified()
                || ip.is_loopback()
                || bytes[0] & 0xfe == 0xfc
                || (bytes[0] == 0xfe && bytes[1] & 0xc0 == 0x80)
                || bytes[0] == 0xff
        }
    }
}

fn metadata(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => ip == Ipv4Addr::new(169, 254, 169, 254),
        IpAddr::V6(ip) => {
            ip.to_ipv4_mapped()
                .is_some_and(|ip| metadata(IpAddr::V4(ip)))
                || ip == "fd00:ec2::254".parse::<std::net::Ipv6Addr>().unwrap()
        }
    }
}

pub struct Response {
    pub status: u16,
    pub headers: reqwest::header::HeaderMap,
    pub bytes: Vec<u8>,
}

impl Response {
    pub fn json(&self) -> Result<Value> {
        serde_json::from_slice(&self.bytes)
            .map_err(|_| Error::new(502, "The endpoint returned invalid JSON."))
    }
}

pub async fn fetch(
    url: &str,
    method: reqwest::Method,
    headers: reqwest::header::HeaderMap,
    body: Option<Vec<u8>>,
    allow_private: bool,
) -> Result<Response> {
    let url = url::Url::parse(url).map_err(|_| Error::bad("Unsupported MCP endpoint."))?;
    if !["https", "http"].contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::bad("Unsupported MCP endpoint."));
    }
    let hostname = url
        .host_str()
        .ok_or_else(|| Error::bad("Unsupported MCP endpoint."))?
        .trim_matches(['[', ']'])
        .to_owned();
    tokio::time::timeout(Duration::from_secs(60), async {
        let port = url.port_or_known_default().unwrap();
        let addresses = if let Ok(ip) = hostname.parse::<IpAddr>() {
            vec![SocketAddr::new(ip, port)]
        } else {
            tokio::net::lookup_host((hostname.as_str(), port))
                .await
                .map_err(|_| Error::new(502, "Unable to resolve MCP endpoint."))?
                .collect::<Vec<_>>()
        };
        if addresses.is_empty()
            || (!allow_private
                && (url.scheme() != "https" || addresses.iter().any(|a| private(a.ip()))))
        {
            return Err(Error::bad(
                "Private network access is disabled for this connection.",
            ));
        }
        if addresses.iter().any(|a| metadata(a.ip())) {
            return Err(Error::bad("Instance metadata endpoints are unavailable."));
        }
        // DNS and network policy are checked on every request, before consulting
        // the pool. Connections are reusable only for the same validated IP set.
        let client = pinned_client(&hostname, &addresses)?;
        let request_id = body
            .as_ref()
            .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
            .and_then(|value| value.get("id").cloned());
        let mut request = client.request(method, url).headers(headers);
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| Error::new(502, "Could not connect to the MCP endpoint."))?;
        if response.status().is_redirection() {
            return Err(Error::bad(
                "The endpoint redirects. Configure its final URL.",
            ));
        }
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let event_stream = headers
            .get("content-type")
            .is_some_and(|v| v.to_str().unwrap_or("").starts_with("text/event-stream"));
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| Error::new(502, "MCP response ended unexpectedly."))?;
            if bytes.len() + chunk.len() > 8 * 1024 * 1024 {
                return Err(Error::new(502, "MCP response exceeds 8 MB."));
            }
            bytes.extend_from_slice(&chunk);
            if event_stream
                && request_id
                    .as_ref()
                    .is_some_and(|id| crate::mcp_client::sse_result(&bytes, id).is_ok())
            {
                break;
            }
        }
        Ok(Response {
            status,
            headers,
            bytes,
        })
    })
    .await
    .map_err(|_| Error::new(504, "MCP endpoint timed out."))?
}
