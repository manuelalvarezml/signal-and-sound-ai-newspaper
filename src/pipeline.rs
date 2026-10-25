use crate::{
    aws::Aws,
    model::{Edition, Source},
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
/// Balance the completed week's evidence with a small, dated context sample.
/// Never send an unbounded month of full source text to the model.
pub fn weekly_sources(
    records: Vec<Source>,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    limit: usize,
) -> Vec<Source> {
    let mut dated = records
        .into_iter()
        .filter_map(|s| {
            let date = s.published.as_ref()?;
            let date = DateTime::parse_from_rfc3339(date)
                .or_else(|_| DateTime::parse_from_rfc3339(&format!("{date}T00:00:00-05:00")))
                .ok()?;
            (date < end && date >= start - chrono::Duration::days(28)).then_some((date, s))
        })
        .collect::<Vec<_>>();
    dated.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    let mut result = vec![];
    let mut urls = std::collections::HashSet::new();
    for page in ["ai", "music"] {
        let quota = limit / 2;
        let context = quota / 5;
        let mut selected = vec![];
        for (old, count) in [(false, quota - context), (true, context)] {
            selected.extend(
                dated
                    .iter()
                    .filter(|(d, s)| s.page == page && (*d < start) == old)
                    .filter(|(_, s)| urls.insert(crate::collect::canonical(&s.url)))
                    .take(count)
                    .map(|(_, s)| s.clone()),
            );
        }
        if selected.len() < quota {
            selected.extend(
                dated
                    .iter()
                    .filter(|(_, s)| s.page == page)
                    .filter(|(_, s)| urls.insert(crate::collect::canonical(&s.url)))
                    .take(quota - selected.len())
                    .map(|(_, s)| s.clone()),
            );
        }
        result.extend(selected);
    }
    result
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
        let result = async {
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
                        // Keep archive context bounded as the publication grows. Original
                        // evidence, when cited, comes from the separate source records.
                        let stories = old["pages"].as_array().unwrap().iter().flat_map(|p| {
                            p["stories"].as_array().unwrap().iter().map(|s| json!({
                                "title":s["title"],"status":s["status"],"source_ids":s["source_ids"],
                                "summary":s["paragraphs"][0].as_str().unwrap_or("").chars().take(320).collect::<String>()
                            }))
                        }).collect::<Vec<_>>();
                        history.push(json!({"id":old["id"],"date":date,"stories":stories}));
                    }
                }
            }
            if kind == "weekly" {
                let (start, end, _) = crate::interests::coverage(
                    at,
                    kind,
                    self.settings["started_at"].as_str().unwrap(),
                )?;
                for s in collection["records"].as_array().unwrap() {
                    historical_sources.insert(s["id"].as_str().unwrap().into(), s.clone());
                }
                let sources = historical_sources
                    .into_values()
                    .map(serde_json::from_value)
                    .collect::<std::result::Result<Vec<Source>, _>>()?;
                collection["records"] = json!(weekly_sources(
                    sources,
                    start,
                    end,
                    self.settings["max_sources"].as_u64().unwrap_or(50) as usize
                ));
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
        }.await;
        if result.is_err() {
            self.state.release(&format!("analysis:{id}")).await?;
        }
        result
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
