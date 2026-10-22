//! AWS credential chain and SigV4 from the official SDK; bounded service requests.
use aws_credential_types::provider::ProvideCredentials;

use anyhow::{Result, ensure};
use aws_config::BehaviorVersion;
use aws_sigv4::{
    http_request::{SignableBody, SignableRequest, SigningSettings, sign},
    sign::v4,
};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime};

#[derive(Clone)]
pub struct Aws {
    config: aws_config::SdkConfig,
    client: reqwest::Client,
    pub region: String,
}
impl Aws {
    pub async fn new() -> Result<Self> {
        let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".into());
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region(aws_config::Region::new(region.clone()))
            .load()
            .await;
        Ok(Self {
            config,
            region,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(180))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
    pub async fn request(
        &self,
        service: &str,
        method: &str,
        url: &str,
        headers: &[(&str, String)],
        body: Vec<u8>,
    ) -> Result<reqwest::Response> {
        let provider = self
            .config
            .credentials_provider()
            .ok_or_else(|| anyhow::anyhow!("AWS credentials unavailable"))?;
        let identity = provider.provide_credentials().await?.into();
        let mut settings = SigningSettings::default();
        if service == "s3" {
            settings.payload_checksum_kind =
                aws_sigv4::http_request::PayloadChecksumKind::XAmzSha256;
            settings.uri_path_normalization_mode =
                aws_sigv4::http_request::UriPathNormalizationMode::Disabled;
            settings.percent_encoding_mode = aws_sigv4::http_request::PercentEncodingMode::Single;
        }
        let params = v4::SigningParams::builder()
            .identity(&identity)
            .region(&self.region)
            .name(service)
            .time(SystemTime::now())
            .settings(settings)
            .build()?
            .into();
        let signable = SignableRequest::new(
            method,
            url,
            headers.iter().map(|(k, v)| (*k, v.as_str())),
            SignableBody::Bytes(&body),
        )?;
        let (instructions, _) = sign(signable, &params)?.into_parts();
        let mut request = self.client.request(method.parse()?, url).body(body);
        for (k, v) in headers {
            request = request.header(*k, v);
        }
        for (k, v) in instructions.headers() {
            request = request.header(k, v);
        }
        Ok(request.send().await?)
    }
    pub async fn json(
        &self,
        service: &str,
        target: Option<&str>,
        path: &str,
        body: &Value,
    ) -> Result<Value> {
        let host = if service == "bedrock" {
            "bedrock-runtime"
        } else {
            service
        };
        let url = format!("https://{host}.{}.amazonaws.com{path}", self.region);
        let mut headers = vec![(
            "content-type",
            if service == "dynamodb" {
                "application/x-amz-json-1.0"
            } else if target.is_some() {
                "application/x-amz-json-1.1"
            } else {
                "application/json"
            }
            .into(),
        )];
        if let Some(target) = target {
            headers.push(("x-amz-target", target.into()));
        }
        let response = self
            .request(service, "POST", &url, &headers, serde_json::to_vec(body)?)
            .await?;
        let status = response.status();
        let data: Value = response.json().await?;
        // Do not log response bodies; they can contain source content or sensitive data.
        ensure!(
            status.is_success(),
            "AWS {service} HTTP {status}: {}",
            data["__type"].as_str().unwrap_or("service error")
        );
        Ok(data)
    }
    pub async fn parameter(&self, name: &str) -> Result<String> {
        let data = self
            .json(
                "ssm",
                Some("AmazonSSM.GetParameter"),
                "/",
                &json!({"Name":name,"WithDecryption":true}),
            )
            .await?;
        Ok(data["Parameter"]["Value"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing secret"))?
            .into())
    }
    pub async fn invoke(&self, function: &str, payload: &Value) -> Result<()> {
        let url = format!(
            "https://lambda.{}.amazonaws.com/2015-03-31/functions/{function}/invocations",
            self.region
        );
        let response = self
            .request(
                "lambda",
                "POST",
                &url,
                &[
                    ("content-type", "application/json".into()),
                    ("x-amz-invocation-type", "Event".into()),
                ],
                serde_json::to_vec(payload)?,
            )
            .await?;
        ensure!(
            response.status().is_success(),
            "Publisher invocation failed: {}",
            response.status()
        );
        Ok(())
    }
}
