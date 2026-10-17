//! Public plugin callback addresses use the claimed Beacon identity or an explicit
//! owner-selected HTTPS origin, never forwarded headers or provider message content.
use crate::{
    error::{Error, Result},
    service::Service,
};

pub(crate) enum Addresses {
    Direct(String),
    Beacon(String),
}

impl Addresses {
    pub(crate) async fn load(s: &Service, direct: Option<&str>) -> Result<Self> {
        if let Some(origin) = direct {
            let url = url::Url::parse(origin)
                .map_err(|_| Error::bad("Enter an HTTPS installation origin."))?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || origin.trim_end_matches('/') != url.origin().ascii_serialization()
            {
                return Err(Error::bad(
                    "Enter an HTTPS installation origin without a path or query.",
                ));
            }
            return Ok(Self::Direct(url.origin().ascii_serialization()));
        }
        let (origin, installation) = crate::relay::beacon_address(&s.config.data_dir)
            .await?
            .ok_or_else(|| {
                Error::unavailable("Claim this installation before connecting a plugin.")
            })?;
        Ok(Self::Beacon(format!(
            "{origin}/api/public/installations/{installation}"
        )))
    }

    pub(crate) fn oauth(&self, instance: &str) -> String {
        match self {
            Self::Direct(origin) => format!("{origin}/api/plugin-oauth/{instance}/callback"),
            Self::Beacon(base) => format!("{base}/plugin-oauth/{instance}/callback"),
        }
    }

    pub(crate) fn webhook(&self, instance: &str, endpoint: &str) -> String {
        match self {
            Self::Direct(origin) => format!("{origin}/api/plugin-webhooks/{instance}/{endpoint}"),
            Self::Beacon(base) => format!("{base}/plugins/{instance}/{endpoint}"),
        }
    }
}
