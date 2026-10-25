use crate::{
    model::Source,
    state::{Budget, Store},
};
use anyhow::{Result, ensure};
use chrono::{DateTime, FixedOffset};
use serde_json::{Value, json};

pub fn queries(
    settings: &Value,
    at: DateTime<FixedOffset>,
    weekly: bool,
) -> Result<Vec<(String, Value)>> {
    let mut queries = vec![
        (
            "ai",
            "Latest AI model releases and research breakthroughs from research labs",
        ),
        (
            "ai",
            "Latest LLM evaluation voice agents memory and infrastructure research",
        ),
        (
            "music",
            "Latest music AI generation recommendation research and releases",
        ),
        (
            "music",
            "Latest music information retrieval DJ tools and controllable music generation research",
        ),
    ];
    queries.truncate(settings["search_daily_queries"].as_u64().unwrap_or(4) as usize);
    if weekly {
        queries.extend([
            ("ai", "AI weekly research roundup and emerging trends"),
            (
                "music",
                "Music AI weekly research roundup and emerging trends",
            ),
        ]);
    }
    let (start, end, _) = crate::interests::coverage(
        at,
        if weekly { "weekly" } else { "daily" },
        settings["started_at"].as_str().unwrap(),
    )?;
    Ok(queries.into_iter().map(|(page,query)|(page.into(),json!({"query":query,"type":"auto","contents":{"highlights":true},"startPublishedDate":start.with_timezone(&chrono::Utc).to_rfc3339_opts(chrono::SecondsFormat::Secs,true),"endPublishedDate":end.with_timezone(&chrono::Utc).to_rfc3339_opts(chrono::SecondsFormat::Secs,true)}))).collect())
}
pub async fn discover(
    settings: &Value,
    at: DateTime<FixedOffset>,
    budget: &Budget,
    store: &Store,
    run_id: &str,
    weekly: bool,
    key: Option<&str>,
) -> Result<(Vec<Source>, Vec<String>, Vec<Value>)> {
    let Some(key) = key else {
        return Ok((
            vec![],
            vec!["Exa not connected; feeds and direct sources only".into()],
            vec![],
        ));
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut records = vec![];
    let mut errors = vec![];
    let mut provenance = vec![];
    for (n, (page, payload)) in queries(settings, at, weekly)?.into_iter().enumerate() {
        let id = format!("{run_id}:exa:{n}");
        let cache = format!("checkpoints/{run_id}/exa-{n}.json");
        let result: Result<Value> = async {
            let cached = if let Some(cached) = store.json(&cache).await? {
                cached
            } else {
                budget
                    .reserve(
                        &id,
                        "search",
                        settings["exa_request_reserve_usd"].as_f64().unwrap_or(0.02),
                    )
                    .await?;
                let response = client
                    .post("https://api.exa.ai/search")
                    .header("x-api-key", key)
                    .json(&payload)
                    .send()
                    .await?;
                ensure!(
                    response.status().is_success(),
                    "Exa HTTP {}",
                    response.status()
                );
                let data: Value = response.json().await?;
                ensure!(data["results"].is_array(), "Invalid search response");
                let cached = json!({"request":payload,"response":data});
                store.put_json(&cache, &cached).await?;
                cached
            };
            ensure!(cached["request"] == payload, "Search checkpoint mismatch");
            let data = &cached["response"];
            let cost = data["costDollars"]["total"]
                .as_f64()
                .unwrap_or(settings["exa_request_reserve_usd"].as_f64().unwrap_or(0.02));
            budget.settle(&id, cost).await?;
            Ok(data.clone())
        }
        .await;
        match result {
            Ok(data) => {
                provenance.push(json!({"provider":"exa","page":page,"query":payload["query"],"request_id":data["requestId"],"results":data["results"].as_array().map(Vec::len),"cost_usd":data["costDollars"]["total"],"start":payload["startPublishedDate"],"end":payload["endPublishedDate"]}));
                for item in data["results"].as_array().unwrap() {
                    let Some(url) = item["url"].as_str() else {
                        continue;
                    };
                    let Ok(parsed) = crate::model::safe_url(url) else {
                        continue;
                    };
                    let published = item["publishedDate"].as_str().map(String::from);
                    if !crate::model::source_available_at(published.as_deref(), at) {
                        continue;
                    }
                    let highlights = item["highlights"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default();
                    records.push(Source {
                        id: crate::collect::source_id(url),
                        url: url.into(),
                        title: item["title"].as_str().unwrap_or(url).into(),
                        publisher: parsed.host_str().unwrap_or("Source").into(),
                        published,
                        retrieved_at: at.to_rfc3339(),
                        excerpt: highlights.chars().take(9000).collect(),
                        page: page.clone(),
                        authority: 0.6,
                        event_id: String::new(),
                    });
                }
            }
            Err(e) => {
                let message = if e.is::<reqwest::Error>() {
                    "Network error".into()
                } else {
                    e.to_string()
                };
                errors.push(format!("Exa query {}: {message}", n + 1));
            }
        }
    }
    Ok((records, errors, provenance))
}
