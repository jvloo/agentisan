use crate::{fixture::read_credential, model::Query};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{net::IpAddr, path::Path, time::Duration};

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: reqwest::Url,
    token: Option<String>,
}

impl Client {
    pub fn new(endpoint: &str, credential_file: Option<&Path>) -> Result<Self> {
        let mut url = reqwest::Url::parse(endpoint).context("invalid service endpoint")?;
        let host: IpAddr = url
            .host_str()
            .unwrap_or("")
            .trim_matches(['[', ']'])
            .parse()
            .context("endpoint must use a numeric loopback IP")?;
        if url.scheme() != "http"
            || !host.is_loopback()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            bail!("endpoint must be an HTTP loopback origin without a path or credentials");
        }
        url.set_path("/v1/inspect");
        let token = credential_file.map(read_credential).transpose()?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            http,
            endpoint: url,
            token,
        })
    }

    pub async fn inspect(&self, query: Query) -> Result<Value> {
        let mut request = self.http.post(self.endpoint.clone()).json(&query);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .context("cannot reach Agentisan service")?;
        let status = response.status();
        if !status.is_success() {
            bail!("inspection failed: HTTP {status}");
        }
        let bytes = response.bytes().await?;
        if bytes.len() > 4_194_304 {
            bail!("service response is too large");
        }
        serde_json::from_slice(&bytes).context("invalid service response")
    }
}
