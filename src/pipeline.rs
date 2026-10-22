use crate::{
    aws::Aws,
    model::Edition,
    state::{Budget, State, Store},
};
use anyhow::{Result, ensure};
use chrono::{DateTime, Datelike, FixedOffset};
use serde_json::{Value, json};
use std::sync::Arc;

pub struct Runtime {
    pub aws: Arc<Aws>,
    pub settings: Value,
    pub state: State,
    pub evidence: Store,
    pub public: Store,
}
impl Runtime {
    pub async fn cloud() -> Result<Self> {
        ensure!(
            std::env::var("PRODUCTION_ENABLED").as_deref() == Ok("true"),
            "Production disabled"
        );
        let aws = Arc::new(Aws::new().await?);
        let mut settings = crate::config("settings")?;
        settings["site_url"] = json!(std::env::var("SITE_URL")?);
        Ok(Self {
            state: State::Dynamo {
                aws: aws.clone(),
                table: std::env::var("STATE_TABLE")?,
            },
            evidence: Store::S3 {
                aws: aws.clone(),
                bucket: std::env::var("EVIDENCE_BUCKET")?,
            },
            public: Store::S3 {
                aws: aws.clone(),
                bucket: std::env::var("PUBLIC_BUCKET")?,
            },
            aws,
            settings,
        })
    }
    pub async fn secret(&self, name: &str) -> Result<Option<String>> {
        match std::env::var(format!("{name}_PARAMETER")) {
            Ok(p) if !p.is_empty() => Ok(Some(self.aws.parameter(&p).await?)),
            _ => Ok(std::env::var(name).ok()),
        }
    }
    pub async fn analyze(
        &self,
        at: DateTime<FixedOffset>,
        kind: &str,
        exa: Option<&str>,
        x: Option<&str>,
    ) -> Result<Edition> {
        let id = format!("{kind}-{}", at.date_naive());
        let key = format!("editions/{id}.json");
        if let Some(e) = self.evidence.json(&key).await? {
            let e: Edition = serde_json::from_value(e)?;
            e.validate()?;
            return Ok(e);
        }
        ensure!(
            self.state.claim(&format!("analysis:{id}"), 900).await?,
            "Run active or completed"
        );
        let profile = self
            .evidence
            .json("config/interests.json")
            .await?
            .unwrap_or(crate::config("interests")?);
        let budget = Budget::new(self.state.clone(), self.settings.clone());
        let mut collection = crate::collect::collect(
            &crate::config("sources")?,
            &self.settings,
            &profile,
            at,
            &budget,
            &self.evidence,
            &id,
            kind == "weekly",
            exa,
            x,
        )
        .await?;
        let mut history = vec![];
        let mut historical_sources = std::collections::BTreeMap::<String, Value>::new();
        let lower = (at - chrono::Duration::days(35)).date_naive().to_string();
        for key in self
            .evidence
            .keys("editions/")
            .await?
            .into_iter()
            .rev()
            .take(40)
        {
            if let Some(old) = self.evidence.json(&key).await? {
                let date = old["date"].as_str().unwrap_or("");
                if date >= lower.as_str() && date < at.date_naive().to_string().as_str() {
                    for s in old["sources"].as_array().unwrap() {
                        historical_sources.insert(s["id"].as_str().unwrap().into(), s.clone());
                    }
                    let mut old = old;
                    for s in old["sources"].as_array_mut().unwrap() {
                        s.as_object_mut().unwrap().remove("excerpt");
                    }
                    for p in old["pages"].as_array_mut().unwrap() {
                        for s in p["stories"].as_array_mut().unwrap() {
                            s.as_object_mut().unwrap().remove("evidence");
                        }
                    }
                    history.push(old);
                }
            }
        }
        if kind == "weekly" {
            let (_, end, _) = crate::interests::coverage(
                at,
                kind,
                self.settings["started_at"].as_str().unwrap(),
            )?;
            for s in collection["records"].as_array().unwrap() {
                historical_sources.insert(s["id"].as_str().unwrap().into(), s.clone());
            }
            collection["records"] = json!(
                historical_sources
                    .into_values()
                    .filter(|s| s["published"].as_str().is_some_and(|d| d
                        .get(..10)
                        .is_some_and(|d| d < end.date_naive().to_string().as_str())))
                    .collect::<Vec<_>>()
            );
        }
        let edition = crate::compose::Composer {
            aws: self.aws.clone(),
            settings: &self.settings,
            budget: &budget,
            store: &self.evidence,
            run_id: &id,
        }
        .compose(&collection, &profile, at, kind, json!(history))
        .await?;
        self.evidence
            .put_json(&key, &serde_json::to_value(&edition)?)
            .await?;
        self.state.finish(&format!("analysis:{id}")).await?;
        Ok(edition)
    }
    pub async fn collector(&self, event: Value, request_id: &str) -> Result<Value> {
        let at = event["cutoff"]
            .as_str()
            .map(DateTime::parse_from_rfc3339)
            .transpose()?
            .unwrap_or_else(|| {
                chrono::Utc::now()
                    .with_timezone(&chrono_tz::America::Lima)
                    .fixed_offset()
            })
            .with_timezone(&chrono_tz::America::Lima)
            .fixed_offset();
        let exa = self.secret("EXA_API_KEY").await?;
        let x = self.secret("X_BEARER_TOKEN").await?;
        if event["action"] == "check_search" {
            let budget = Budget::new(self.state.clone(), self.settings.clone());
            let (records, errors, queries) = crate::search::discover(
                &self.settings,
                at,
                &budget,
                &self.evidence,
                &format!("exa-check-{request_id}"),
                false,
                exa.as_deref(),
            )
            .await?;
            return Ok(
                json!({"provider":"exa","results":records.len(),"errors":errors,"queries":queries,"runtime":"rust"}),
            );
        }
        if event["action"] == "check_pipeline" {
            let id = format!("rust-check-{request_id}");
            let budget = Budget::new(self.state.clone(), self.settings.clone());
            let profile = self
                .evidence
                .json("config/interests.json")
                .await?
                .unwrap_or(crate::config("interests")?);
            let collection = crate::collect::collect(
                &crate::config("sources")?,
                &self.settings,
                &profile,
                at,
                &budget,
                &self.evidence,
                &id,
                false,
                exa.as_deref(),
                x.as_deref(),
            )
            .await?;
            let edition = crate::compose::Composer {
                aws: self.aws.clone(),
                settings: &self.settings,
                budget: &budget,
                store: &self.evidence,
                run_id: &id,
            }
            .compose(&collection, &profile, at, "daily", json!([]))
            .await?;
            let key = format!("checks/{id}/edition.json");
            self.evidence
                .put_json(&key, &serde_json::to_value(&edition)?)
                .await?;
            return Ok(
                json!({"runtime":"rust", "pdf":"typst", "validated":true, "edition_key":key,
                "records":collection["records"].as_array().map(Vec::len), "search":collection["search_provenance"],
                "coverage_notes":collection["coverage_notes"]}),
            );
        }
        let kinds = if let Some(k) = event["kind"].as_str() {
            ensure!(matches!(k, "daily" | "weekly"), "Invalid kind");
            vec![k]
        } else if at.weekday() == chrono::Weekday::Mon {
            vec!["daily", "weekly"]
        } else {
            vec!["daily"]
        };
        let mut results = vec![];
        for kind in kinds {
            let edition = self.analyze(at, kind, exa.as_deref(), x.as_deref()).await?;
            self.aws
                .invoke(
                    &std::env::var("PUBLISHER_FUNCTION")?,
                    &json!({"edition_key":format!("editions/{}.json",edition.id)}),
                )
                .await?;
            results.push(edition.id);
        }
        Ok(json!({"analyzed":results,"runtime":"rust"}))
    }
    pub async fn publisher(&self, event: Value) -> Result<Value> {
        ensure!(
            self.state.claim("publication-lock", 150).await?,
            "Publication busy; retry later"
        );
        let result = self.publish_inner(event).await;
        self.state.release("publication-lock").await?;
        result
    }
    async fn publish_inner(&self, event: Value) -> Result<Value> {
        let key = event["edition_key"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing edition key"))?;
        ensure!(
            key.starts_with("editions/") && !key.contains(".."),
            "Invalid edition key"
        );
        let edition: Edition = serde_json::from_value(
            self.evidence
                .json(key)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Missing edition"))?,
        )?;
        edition.validate()?;
        let dir = tempfile::tempdir()?;
        for key in ["latest.json", "archive.json"] {
            if let Some(v) = self.public.get(key).await? {
                std::fs::write(dir.path().join(key), v)?;
            }
        }
        let release = format!("editions/{}/r{}", edition.id, edition.revision);
        if let Some(old) = self.public.json(&format!("{release}/edition.json")).await? {
            ensure!(
                old == edition.public()?,
                "Published revision immutable; increment revision"
            );
        }
        crate::render::render(
            &edition,
            dir.path(),
            self.settings["site_url"].as_str().unwrap(),
        )?;
        let mut files = walkdir::WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .collect::<Vec<_>>();
        files.sort_by_key(|p| p.file_name().is_some_and(|s| s == "latest.json"));
        for file in files {
            let key = file.strip_prefix(dir.path())?.to_string_lossy().to_string();
            let immutable = key.starts_with("editions/") || key.starts_with("assets/");
            // Recover interrupted uploads without replacing already-published immutable bytes.
            if immutable && self.public.get(&key).await?.is_some() {
                continue;
            }
            let mime = match file.extension().and_then(|s| s.to_str()) {
                Some("html") => "text/html; charset=utf-8",
                Some("css") => "text/css",
                Some("js") => "text/javascript",
                Some("pdf") => "application/pdf",
                _ => "application/json",
            };
            let disposition = key.ends_with(".pdf").then(|| {
                format!(
                    "{}; filename=\"signal-and-sound-{}.pdf\"",
                    if key.ends_with("/print.pdf") {
                        "inline"
                    } else {
                        "attachment"
                    },
                    edition.date
                )
            });
            self.public
                .put(
                    &key,
                    std::fs::read(&file)?,
                    mime,
                    key.starts_with("editions/") || key.starts_with("assets/"),
                    disposition,
                )
                .await?;
        }
        Ok(
            json!({"published":edition.id,"revision":edition.revision,"runtime":"rust","pdf":"typst"}),
        )
    }
}
