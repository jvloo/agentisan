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
        self.request(self.endpoint.clone(), &query).await
    }

    pub async fn act(&self, action: crate::teams::Action) -> Result<Value> {
        let mut url = self.endpoint.clone();
        url.set_path("/v1/team-action");
        self.request(url, &action).await
    }

    pub async fn start_run(&self, args: crate::teams::ControllerStartArgs) -> Result<Value> {
        let mut url = self.endpoint.clone();
        url.set_path("/v1/controller/run-start");
        self.request(url, &args).await
    }

    async fn request(&self, url: reqwest::Url, body: &impl serde::Serialize) -> Result<Value> {
        let mut request = self.http.post(url).json(body);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .context("cannot reach Agentisan service")?;
        let status = response.status();
        let bytes = response.bytes().await?;
        if bytes.len() > 4_194_304 {
            bail!("service response is too large");
        }
        if !status.is_success() {
            let message = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_else(|| "request rejected".into());
            bail!("request failed: HTTP {status}: {message}");
        }
        serde_json::from_slice(&bytes).context("invalid service response")
    }
}
