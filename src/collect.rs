use crate::{
    model::Source,
    state::{Budget, Store},
};
use anyhow::{Result, bail, ensure};
use chrono::{DateTime, FixedOffset, Utc};
use futures::{StreamExt, stream};
use scraper::{Html, Selector};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    net::IpAddr,
    time::Duration,
};
const UA: &str = "SignalAndSound/1.0 (personal research newspaper; bounded daily collection)";
pub fn canonical(url: &str) -> String {
    let Ok(mut u) = url::Url::parse(url) else {
        return url.into();
    };
    u.set_fragment(None);
    let pairs = u
        .query_pairs()
        .filter(|(k, _)| !k.starts_with("utm_") && !matches!(k.as_ref(), "ref" | "fbclid"))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<Vec<_>>();
    u.set_query(None);
    if !pairs.is_empty() {
        u.query_pairs_mut().extend_pairs(pairs);
    }
    let path = u.path().trim_end_matches('/').to_string();
    u.set_path(if path.is_empty() { "/" } else { &path });
    u.to_string()
}
pub fn source_id(url: &str) -> String {
    crate::hash(canonical(url).as_bytes())[..16].into()
}
pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let a = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_unspecified()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && a[0] != 0
                && a[0] < 224
                && !(a[0] == 100 && (64..=127).contains(&a[1]))
                && !(a[0] == 198 && (a[1] == 18 || a[1] == 19))
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(v4));
            }
            let a = ip.segments();
            (a[0] & 0xe000) == 0x2000 && !(a[0] == 0x2001 && a[1] == 0xdb8)
        }
    }
}
async fn raw_get(url: &str) -> Result<String> {
    let mut u = crate::model::safe_url(url)?;
    for _ in 0..5 {
        let host = u.host_str().unwrap().to_string();
        let addresses =
            tokio::net::lookup_host((host.as_str(), u.port_or_known_default().unwrap()))
                .await?
                .collect::<Vec<_>>();
        ensure!(
            !addresses.is_empty() && addresses.iter().all(|a| public_ip(a.ip())),
            "Non-public destination blocked"
        );
        // Pin validated addresses to prevent DNS rebinding between validation and fetch.
        let client = reqwest::Client::builder()
            .resolve_to_addrs(&host, &addresses)
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .user_agent(UA)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let r = client.get(u.clone()).send().await?;
        if r.status().is_redirection() {
            let target = r
                .headers()
                .get("location")
                .ok_or_else(|| anyhow::anyhow!("Missing redirect"))?
                .to_str()?;
            u = crate::model::safe_url(u.join(target)?.as_str())?;
            continue;
        }
        ensure!(r.status().is_success(), "Source HTTP {}", r.status());
        let mut bytes = vec![];
        let mut stream = r.bytes_stream();
        while let Some(chunk) = stream.next().await {
            bytes.extend_from_slice(&chunk?);
            ensure!(bytes.len() <= 2_000_000, "Download exceeds limit")
        }
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    bail!("Too many redirects")
}
pub async fn fetch(url: &str, robots: bool) -> Result<String> {
    let u = crate::model::safe_url(url)?;
    if robots {
        let origin = format!(
            "{}://{}{}",
            u.scheme(),
            u.host_str().unwrap(),
            u.port().map(|p| format!(":{p}")).unwrap_or_default()
        );
        match raw_get(&format!("{origin}/robots.txt")).await {
            Ok(r) => ensure!(
                robotstxt::DefaultMatcher::default().one_agent_allowed_by_robots(&r, UA, url),
                "Source disallows collection"
            ),
            Err(e) if e.to_string().contains("404") || e.to_string().contains("410") => {}
            Err(e) => return Err(e),
        }
    }
    raw_get(url).await
}
pub fn relevance(text: &str, profile: &Value, page: &str) -> f64 {
    let text = text.to_lowercase();
    let mut matches = vec![];
    if let Some(topics) = profile["topics"].as_array() {
        for t in topics {
            if (t["page"] == page || t["page"] == "both")
                && t["keywords"].as_array().is_some_and(|ks| {
                    ks.iter()
                        .filter_map(Value::as_str)
                        .any(|k| text.contains(&k.to_lowercase()))
                })
            {
                matches.push(t["weight"].as_f64().unwrap_or(0.15))
            }
        }
    }
    (matches.iter().copied().reduce(f64::max).unwrap_or(0.15)
        + 0.05 * matches.len().saturating_sub(1) as f64)
        .min(1.0)
}
pub fn x_score(
    relevance: f64,
    evidence: f64,
    novelty: f64,
    credibility: f64,
    age: f64,
    metrics: &Value,
) -> f64 {
    let count = |k: &str| metrics[k].as_f64().unwrap_or(0.0);
    let weighted = count("like_count")
        + 2.0 * count("retweet_count")
        + 2.0 * count("quote_count")
        + count("reply_count");
    let engagement = (weighted / age.max(1.0)).ln_1p() / 50_f64.ln_1p();
    100.0
        * (0.35 * relevance
            + 0.25 * evidence
            + 0.15 * novelty
            + 0.1 * credibility
            + 0.1 * 2_f64.powf(-age.max(0.0) / 48.0)
            + 0.05 * engagement.min(1.0))
}
fn base(entry: &Value, url: &str, title: &str, at: DateTime<FixedOffset>) -> Source {
    Source {
        id: source_id(url),
        title: title.into(),
        publisher: entry["name"].as_str().unwrap_or("Source").into(),
        url: url.into(),
        published: None,
        retrieved_at: at.to_rfc3339(),
        excerpt: String::new(),
        page: entry["page"].as_str().unwrap_or("ai").into(),
        authority: entry["authority"].as_f64().unwrap_or(0.5),
        event_id: String::new(),
    }
}
async fn parse_source(entry: Value, at: DateTime<FixedOffset>) -> Result<Vec<Source>> {
    let url = entry["url"].as_str().unwrap();
    let kind = entry["kind"].as_str().unwrap();
    let text = fetch(url, kind != "hn").await?;
    let mut records = vec![];
    match kind {
        "rss" => {
            let feed = feed_rs::parser::parse(text.as_bytes())?;
            for item in feed.entries.into_iter().take(30) {
                let Some(link) = item.links.first() else {
                    continue;
                };
                let mut s = base(
                    &entry,
                    &link.href,
                    item.title
                        .as_ref()
                        .map(|t| t.content.as_str())
                        .unwrap_or("Untitled"),
                    at,
                );
                s.published = item.published.or(item.updated).map(|d| d.to_rfc3339());
                s.excerpt = item
                    .summary
                    .map(|s| {
                        Html::parse_fragment(&s.content)
                            .root_element()
                            .text()
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                records.push(s)
            }
        }
        "html" => {
            let doc = Html::parse_document(&text);
            let baseurl = url::Url::parse(url)?;
            for a in doc.select(&Selector::parse("a[href]").unwrap()) {
                let Ok(u) = baseurl.join(a.value().attr("href").unwrap()) else {
                    continue;
                };
                let title = a
                    .text()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if u.host_str() != baseurl.host_str()
                    || !u
                        .path()
                        .contains(entry["path_filter"].as_str().unwrap_or("/"))
                    || title.len() < 24
                {
                    continue;
                }
                records.push(base(&entry, u.as_str(), &title, at));
                if records.len() >= 20 {
                    break;
                }
            }
        }
        "hn" => {
            let ids: Vec<u64> = serde_json::from_str(&text)?;
            for id in ids.into_iter().take(20) {
                let data: Value = serde_json::from_str(
                    &fetch(
                        &format!("https://hacker-news.firebaseio.com/v0/item/{id}.json"),
                        false,
                    )
                    .await?,
                )?;
                if let Some(url) = data["url"].as_str() {
                    let mut s = base(
                        &entry,
                        url,
                        data["title"].as_str().unwrap_or("Discussion"),
                        at,
                    );
                    s.published = data["time"]
                        .as_i64()
                        .and_then(|t| DateTime::<Utc>::from_timestamp(t, 0))
                        .map(|d| d.to_rfc3339());
                    s.excerpt = format!("Discussion: https://news.ycombinator.com/item?id={id}");
                    records.push(s)
                }
            }
        }
        _ => bail!("Unknown source kind"),
    }
    records.retain(|r| crate::model::source_available_at(r.published.as_deref(), at));
    Ok(records)
}
fn extract_html(raw: &str, s: &mut Source) {
    let doc = Html::parse_document(raw);
    let selector = |s: &str| Selector::parse(s).unwrap();
    for name in ["meta[property='og:title']", "meta[name='title']"] {
        if let Some(t) = doc
            .select(&selector(name))
            .next()
            .and_then(|e| e.value().attr("content"))
        {
            s.title = t.into();
            break;
        }
    }
    for name in [
        "meta[property='article:published_time']",
        "meta[name='date']",
        "meta[name='citation_publication_date']",
        "time[datetime]",
    ] {
        if let Some(t) = doc
            .select(&selector(name))
            .next()
            .and_then(|e| e.value().attr("content").or(e.value().attr("datetime")))
        {
            let normalized = t.replace('/', "-");
            if DateTime::parse_from_rfc3339(&normalized).is_ok()
                || chrono::NaiveDate::parse_from_str(&normalized, "%Y-%m-%d").is_ok()
            {
                s.published = Some(normalized);
                break;
            }
        }
    }
    if let Some(name) = doc
        .select(&selector("meta[property='og:site_name']"))
        .next()
        .and_then(|e| e.value().attr("content"))
    {
        s.publisher = name.into()
    }
    for root in ["article", "main", "body"] {
        if let Some(element) = doc.select(&selector(root)).next() {
            let paragraphs = element
                .select(&selector("h1,h2,h3,p,li,blockquote"))
                .filter(|e| {
                    !e.ancestors()
                        .filter_map(scraper::ElementRef::wrap)
                        .any(|e| {
                            matches!(
                                e.value().name(),
                                "nav" | "header" | "footer" | "script" | "style"
                            )
                        })
                })
                .map(|e| {
                    e.text()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .filter(|s| s.len() > 25)
                .collect::<Vec<_>>()
                .join("\n");
            if paragraphs.len() > 100 {
                s.excerpt = paragraphs.chars().take(9000).collect();
                break;
            }
        }
    }
}
// This orchestration boundary keeps settings, evidence and credentials explicit.
#[allow(clippy::too_many_arguments)]
pub async fn collect(
    registry: &Value,
    settings: &Value,
    profile: &Value,
    at: DateTime<FixedOffset>,
    budget: &Budget,
    store: &Store,
    run_id: &str,
    weekly: bool,
    exa: Option<&str>,
    x: Option<&str>,
) -> Result<Value> {
    let cache = format!("checkpoints/{run_id}/collection.json");
    if let Some(c) = store.json(&cache).await? {
        return Ok(c);
    }
    let mut sources = BTreeMap::new();
    let mut errors = vec![];
    let selected = registry
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| weekly || e["weekly_only"] != true)
        .take(settings["max_sources"].as_u64().unwrap_or(50) as usize)
        .cloned()
        .collect::<Vec<_>>();
    let mut jobs = stream::iter(selected.into_iter().map(|e| async move {
        let name = e["name"].as_str().unwrap_or("Source").to_string();
        (
            name,
            tokio::time::timeout(Duration::from_secs(60), parse_source(e, at)).await,
        )
    }))
    .buffer_unordered(6);
    while let Some((name, result)) = jobs.next().await {
        match result {
            Ok(Ok(records)) => {
                for r in records {
                    sources.insert(r.id.clone(), r);
                }
            }
            _ => errors.push(format!("{name}: collection unavailable")),
        }
    }
    let (found, notes, provenance) =
        crate::search::discover(settings, at, budget, store, run_id, weekly, exa).await?;
    errors.extend(notes);
    for r in found {
        sources.entry(r.id.clone()).or_insert(r);
    }
    if let Some(token) = x.filter(|_| !weekly) {
        match collect_x(
            token, settings, registry, profile, at, budget, store, run_id,
        )
        .await
        {
            Ok(records) => {
                for r in records {
                    sources.entry(r.id.clone()).or_insert(r);
                }
            }
            Err(_) => errors.push("X collection unavailable".into()),
        }
    } else if x.is_none() {
        errors.push("X not connected; no direct X posts were collected".into())
    }
    let mut ranked = sources.into_values().collect::<Vec<_>>();
    ranked.sort_by(|a, b| {
        relevance(&format!("{}{}", b.title, b.excerpt), profile, &b.page)
            .total_cmp(&relevance(
                &format!("{}{}", a.title, a.excerpt),
                profile,
                &a.page,
            ))
            .then(b.published.cmp(&a.published))
    });
    ranked.truncate(settings["max_candidates"].as_u64().unwrap_or(300) as usize);
    let count = ranked.len();
    let selected = ["ai", "music"]
        .into_iter()
        .flat_map(|page| {
            ranked
                .iter()
                .filter(move |r| r.page == page)
                .take(settings["max_articles"].as_u64().unwrap_or(40) as usize / 2)
                .cloned()
        })
        .collect::<Vec<_>>();
    let mut records = vec![];
    let mut jobs = stream::iter(selected.into_iter().map(|mut s| async move {
        let result = tokio::time::timeout(Duration::from_secs(35), fetch(&s.url, true)).await;
        if let Ok(Ok(raw)) = result {
            extract_html(&raw, &mut s);
            Some(s)
        } else {
            None
        }
    }))
    .buffer_unordered(6);
    while let Some(r) = jobs.next().await {
        if let Some(r) = r {
            if !r.excerpt.is_empty()
                && crate::model::source_available_at(r.published.as_deref(), at)
            {
                records.push(r)
            }
        } else {
            errors.push("Article unavailable; excluded from evidence".into())
        }
    }
    let result = json!({"run_id":run_id,"cutoff":at,"records":records,"coverage_notes":errors,"metadata_candidates":count,"search_provenance":provenance});
    store.put_json(&cache, &result).await?;
    store
        .put_json(
            &format!("history/{}/{run_id}.json", at.date_naive()),
            &result,
        )
        .await?;
    Ok(result)
}
#[allow(clippy::too_many_arguments)]
async fn collect_x(
    token: &str,
    settings: &Value,
    registry: &Value,
    profile: &Value,
    at: DateTime<FixedOffset>,
    budget: &Budget,
    store: &Store,
    id: &str,
) -> Result<Vec<Source>> {
    let cache = format!("checkpoints/{id}/x.json");
    let key = format!("{id}:x");
    let data = if let Some(d) = store.json(&cache).await? {
        d
    } else {
        budget.reserve(&key, "x", 0.05).await?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()?;
        let r=client.get("https://api.x.com/2/tweets/search/recent").bearer_auth(token).query(&[("query", "(\"AI research\" OR \"music AI\" OR \"music generation\" OR \"LLM evaluation\") has:links -is:retweet -is:reply lang:en".into()),("max_results","10".into()),("start_time",(at-chrono::Duration::hours(48)).with_timezone(&Utc).to_rfc3339_opts(chrono::SecondsFormat::Secs,true)),("end_time",at.with_timezone(&Utc).to_rfc3339_opts(chrono::SecondsFormat::Secs,true)),("sort_order","relevancy".into()),("tweet.fields","created_at,public_metrics,entities,author_id".into())]).send().await?;
        ensure!(r.status().is_success(), "X HTTP error");
        let d: Value = r.json().await?;
        store.put_json(&cache, &d).await?;
        d
    };
    let posts = data["data"].as_array().cloned().unwrap_or_default();
    budget.settle(&key, posts.len() as f64 * 0.005).await?;
    let mut known = HashSet::new();
    for key in store.keys("history/").await?.into_iter().rev().take(35) {
        if let Some(h) = store.json(&key).await?
            && let Some(records) = h["records"].as_array()
        {
            for r in records {
                if let Some(u) = r["url"].as_str() {
                    known.insert(canonical(u));
                }
            }
        }
    }
    let primary = registry
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["authority"].as_f64().unwrap_or(0.0) >= 0.8)
        .filter_map(|s| {
            url::Url::parse(s["url"].as_str()?)
                .ok()?
                .host_str()
                .map(String::from)
        })
        .collect::<HashSet<_>>();
    let mut records = vec![];
    for post in posts {
        let text = post["text"].as_str().unwrap_or("");
        let lower = text.to_lowercase();
        let page = if ["music", "audio", "song"].iter().any(|w| lower.contains(w)) {
            "music"
        } else {
            "ai"
        };
        let links = post["entities"]["urls"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|l| l["expanded_url"].as_str().map(String::from))
            .filter(|u| {
                crate::model::safe_url(u)
                    .is_ok_and(|u| !matches!(u.host_str(), Some("x.com" | "twitter.com" | "t.co")))
            })
            .collect::<Vec<_>>();
        let evidence = if links
            .iter()
            .any(|u| url::Url::parse(u).is_ok_and(|u| primary.contains(u.host_str().unwrap_or(""))))
        {
            1.0
        } else if links.is_empty() {
            0.0
        } else {
            0.5
        };
        let novelty = if links.iter().any(|u| !known.contains(&canonical(u))) {
            1.0
        } else {
            0.0
        };
        let age = (at - DateTime::parse_from_rfc3339(post["created_at"].as_str().unwrap_or(""))?)
            .num_seconds() as f64
            / 3600.0;
        let credibility = if settings["trusted_x_author_ids"]
            .as_array()
            .is_some_and(|ids| ids.contains(&post["author_id"]))
        {
            0.9
        } else {
            0.3
        };
        if x_score(
            relevance(text, profile, page),
            evidence,
            novelty,
            credibility,
            age,
            &post["public_metrics"],
        ) >= 65.0
        {
            for url in links {
                let mut s = base(
                    &json!({"name":"X discovery; verify linked source","page":page,"authority":0.5}),
                    &url,
                    text,
                    at,
                );
                s.excerpt = format!(
                    "Discovery: https://x.com/i/status/{}",
                    post["id"].as_str().unwrap_or("")
                );
                records.push(s)
            }
        }
    }
    Ok(records)
}
