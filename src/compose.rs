use crate::{
    aws::Aws,
    model::{Edition, Source},
    state::{Budget, Store},
};
use anyhow::{Result, bail, ensure};
use chrono::{DateTime, FixedOffset};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
const SYSTEM: &str = include_str!("../prompts/system.txt");
const INSTRUCTION: &str = include_str!("../prompts/instruction.txt");

pub fn structured_schema(mut schema: Value) -> Value {
    fn visit(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for key in [
                    "default",
                    "minimum",
                    "maximum",
                    "minItems",
                    "maxItems",
                    "minLength",
                    "maxLength",
                    "pattern",
                ] {
                    map.remove(key);
                }
                if map.get("type").and_then(Value::as_str) == Some("object") {
                    map.insert("additionalProperties".into(), json!(false));
                    let fields = map
                        .get("properties")
                        .and_then(Value::as_object)
                        .map(|p| p.keys().cloned().collect::<Vec<_>>())
                        .unwrap_or_default();
                    map.entry("properties").or_insert(json!({}));
                    map.insert("required".into(), json!(fields));
                }
                for child in map.values_mut() {
                    visit(child);
                }
            }
            Value::Array(items) => {
                for child in items {
                    visit(child);
                }
            }
            _ => {}
        }
    }
    visit(&mut schema);
    schema
}

pub fn parse_model_json(content: &str) -> Result<Value> {
    let text = content.trim();
    let text = if text.starts_with("```json") || text.starts_with("```\n") {
        text.split_once('\n').map(|(_, s)| s).unwrap_or(text)
    } else {
        text
    };
    // Some models append prose after a valid JSON object. Decode just the first
    // object; downstream typed schemas still validate every consumed field.
    let data = serde_json::Deserializer::from_str(text)
        .into_iter::<Value>()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Empty model response"))??;
    ensure!(data.is_object(), "Model must return a JSON object");
    Ok(data)
}

pub struct Composer<'a> {
    pub aws: Arc<Aws>,
    pub settings: &'a Value,
    pub budget: &'a Budget,
    pub store: &'a Store,
    pub run_id: &'a str,
}
impl Composer<'_> {
    async fn call(&self, stage: &str, model: &str, prompt: &Value, maximum: u64) -> Result<Value> {
        let cache = format!("checkpoints/{}/{stage}.json", self.run_id);
        if let Some(c) = self.store.json(&cache).await? {
            return Ok(c);
        }
        let prompt = serde_json::to_string(prompt)?;
        let rates = &self.settings["prices_per_million"][model];
        let input = rates[0].as_f64().unwrap();
        let output = rates[1].as_f64().unwrap();
        let reservation =
            ((SYSTEM.len() + prompt.len() + 1024) as f64 * input + maximum as f64 * output) / 1e6;
        let key = format!("{}:bedrock:{stage}", self.run_id);
        let response_key = format!("checkpoints/{}/{stage}-response.json", self.run_id);
        let response = if let Some(r) = self.store.json(&response_key).await? {
            r
        } else {
            self.budget.reserve(&key, "bedrock", reservation).await?;
            let mut request = json!({"system":[{"text":SYSTEM}],"messages":[{"role":"user","content":[{"text":prompt}]}],"inferenceConfig":{"maxTokens":maximum,"temperature":0}});
            if model.starts_with("openai.") {
                let schema = if stage.starts_with("write-") {
                    structured_schema(serde_json::from_str(include_str!(
                        "../config/edition.schema.json"
                    ))?)
                } else {
                    json!({"type":"object","properties":{"supported":{"type":"boolean"},"issues":{"type":"array","items":{"type":"string"}}},"required":["supported","issues"],"additionalProperties":false})
                };
                request["outputConfig"] = json!({"textFormat":{"type":"json_schema","structure":{"jsonSchema":{"schema":serde_json::to_string(&schema)?,"name":"newspaper_response"}}}});
            }
            let r = self
                .aws
                .json(
                    "bedrock",
                    None,
                    &format!("/model/{model}/converse"),
                    &request,
                )
                .await?;
            self.store.put_json(&response_key, &r).await?;
            r
        };
        let usage = &response["usage"];
        let actual = (usage["inputTokens"].as_f64().unwrap_or(0.0) * input
            + usage["outputTokens"].as_f64().unwrap_or(0.0) * output)
            / 1e6;
        self.budget.settle(&key, actual).await?;
        ensure!(
            response["stopReason"] != "max_tokens",
            "Model output truncated; no publication"
        );
        let content = response["output"]["message"]["content"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Missing model output"))?
            .iter()
            .filter_map(|v| v["text"].as_str())
            .collect::<String>();
        let data = parse_model_json(&content)?;
        self.store.put_json(&cache, &data).await?;
        Ok(data)
    }
    pub async fn compose(
        &self,
        collection: &Value,
        profile: &Value,
        at: DateTime<FixedOffset>,
        kind: &str,
        history: Value,
    ) -> Result<Edition> {
        let records: Vec<Source> =
            serde_json::from_value::<Vec<Source>>(collection["records"].clone())?
                .into_iter()
                .filter(|s| s.excerpt.chars().count() >= 300)
                .collect();
        ensure!(
            records.len() >= 4,
            "Insufficient evidence; keep last good edition"
        );
        let inputs = records
            .iter()
            .map(|s| {
                let mut s = s.clone();
                s.excerpt = s.excerpt.chars().take(4000).collect();
                s
            })
            .collect::<Vec<_>>();
        let digest = match self.call("extract",self.settings["extract_model"].as_str().unwrap(),
            &json!({"instruction":"Extract supported facts from each source. Return JSON with items, each containing source_id, facts (one or two short strings), limitations (array), and topic. Maximum 40 words per item. Omit navigation-only or unusable sources. Do not emit empty facts.","sources":inputs}),8000).await {
            Ok(digest) => digest,
            Err(error) if error.to_string().contains("Model output truncated") || error.is::<serde_json::Error>() => {
                let compact=records.iter().map(|s|{let mut s=s.clone();s.excerpt=s.excerpt.chars().take(1800).collect();s}).collect::<Vec<_>>();
                self.call("extract-compact",self.settings["extract_model"].as_str().unwrap(),
                    &json!({"instruction":"Return a JSON object with items. At most one item per source, maximum 40 items. Each item has source_id, facts: an array of ONE short factual sentence, limitations: an empty array or ONE short caveat, topic: ai or music. Maximum 35 words per item. Omit irrelevant or unusable sources. Stop after the final closing brace. Never output blank or repeated list items.","sources":compact}),6000).await?
            },
            Err(error) => return Err(error),
        };
        ensure!(digest["items"].is_array(), "Invalid extraction schema");
        let at = collection["cutoff"]
            .as_str()
            .map(DateTime::parse_from_rfc3339)
            .transpose()?
            .unwrap_or(at);

        let (start, end, partial) =
            crate::interests::coverage(at, kind, self.settings["started_at"].as_str().unwrap())?;
        let metadata = json!({"id":format!("{kind}-{}",at.date_naive()),"revision":1,"kind":kind,"date":at.date_naive().to_string(),"cutoff":at,"coverage_start":start,"week_start":crate::interests::week_start(at),"inaugural":at.date_naive().to_string()==self.settings["started_at"].as_str().unwrap()[..10],"partial_week":partial,"profile_version":profile["version"],"editor":"AI-assisted; automated evidence review","coverage_notes":collection["coverage_notes"]});
        let mut instruction = INSTRUCTION.to_string();
        if kind == "weekly" {
            instruction.push_str(&format!(" Weekly: synthesize the completed week ending exclusively {end}. Compare with earlier 28-day archive. Trends require two distinct dated sources; distinguish early signals from established trends and name contrary evidence or uncertainty. Verify summary blogs against primary sources. Do not concatenate daily stories."));
        }
        let short = records
            .iter()
            .map(|s| {
                let mut s = s.clone();
                s.excerpt = s.excerpt.chars().take(6500).collect();
                s
            })
            .collect::<Vec<_>>();
        let topics = digest["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| json!({"source_id":i["source_id"],"topic":i["topic"]}))
            .collect::<Vec<_>>();
        let payload = json!({"instructions":instruction,"metadata":metadata,"interests":crate::interests::sanitized(profile),"candidate_topics":topics,"sources":short,"past_editions":history,"schema":serde_json::from_str::<Value>(include_str!("../config/edition.schema.json"))?});
        let mut prompt = payload.clone();
        for attempt in 0..3 {
            let stage = match attempt {
                0 => "write-rust-v5",
                1 => "write-rust-v5-repair",
                _ => "write-rust-v5-repair-final",
            };
            let raw = self
                .call(
                    stage,
                    self.settings["compose_model"].as_str().unwrap(),
                    &prompt,
                    self.settings["write_max_tokens"].as_u64().unwrap_or(16000),
                )
                .await?;
            let validated:Result<Edition>=async{
                let edition=normalize(raw.clone(),&metadata,&records)?;
                let tmp=tempfile::tempdir()?;crate::render::pdf(&edition,&tmp.path().join("validation.pdf"),self.settings["site_url"].as_str().unwrap_or(""))?;
                let public=edition.public()?;
                let mut issues=vec![];
                for (n,page) in edition.pages.iter().enumerate() {
                    let cited=page.stories.iter().flat_map(|s| &s.source_ids).collect::<std::collections::HashSet<_>>();
                    let sources=edition.sources.iter().filter(|s| cited.contains(&s.id)).collect::<Vec<_>>();
                    let concept_ids=page.stories.iter().flat_map(|s| &s.concepts).collect::<std::collections::HashSet<_>>();
                    let concepts=edition.concepts.iter().filter(|c|concept_ids.contains(&c.id)).collect::<Vec<_>>();
                    let stage=format!("verify-rust-v5-{}-{attempt}",page.id);
                    let verdict=self.call(&stage,self.settings["review_model"].as_str().unwrap_or(self.settings["compose_model"].as_str().unwrap()),&json!({"instruction":"Act as a skeptical fact checker. Review ONLY published_text and concepts against cited_original_evidence. Check every factual assertion, quantity, unit, date, attribution and limitation, including the implication sentences. Each story must be supported by its own source_ids; a source title alone is insufficient evidence for detailed claims. Detect contradictions, invented specifications, local/cloud confusion, latency versus relative speedup confusion, and overstated guarantees. Original evidence is the authority; the writer and its extracted digest are fallible. Return exactly one JSON object {supported:boolean,issues:[string]}. Give precise unsupported claims, story IDs, and corrections supported by the cited source. A vendor's reported result must remain attributed. Clearly conditional practical suggestions are allowed. Set supported false if any factual claim lacks support or contradicts its source. Do not evaluate assertions merely quoted within the original source excerpts as newspaper text.","published_text":public["pages"][n],"concepts":concepts,"cited_original_evidence":sources}),6000).await?;
                    if verdict["supported"]!=true || !verdict["issues"].as_array().is_some_and(Vec::is_empty) {
                        issues.push(json!({"page":page.id,"issues":verdict["issues"]}));
                    }
                }
                ensure!(issues.is_empty(),"Evidence review failed: {}",json!(issues));Ok(edition)
            }.await;
            match validated {
                Ok(e) => return Ok(e),
                Err(e) if attempt < 2 => {
                    prompt = payload.clone();
                    prompt["correction"] = json!(e.to_string());
                    prompt["previous_draft"] = raw;
                    prompt["previous_draft"]["sources"] = json!([]);
                    prompt["repair_instruction"] = json!(
                        "Correct the validation problem, preserve only supported claims, return full corrected edition. Word counts INCLUDE implication. Aim for 170-185 broad words and 115-125 personal words on each page. Use six stories. Shorten if PDF overflows; do not shrink fonts. Set sources to []."
                    );
                }
                Err(e) => return Err(e),
            }
        }
        bail!("Composition failed")
    }
}
pub fn normalize(mut raw: Value, metadata: &Value, records: &[Source]) -> Result<Edition> {
    for (k, v) in metadata.as_object().unwrap() {
        raw[k] = v.clone();
    }
    if raw.get("note").is_none() {
        raw["note"] = json!("Follow the linked sources for original reporting and research.");
    }
    // A legacy decorative field is not part of either public page layout.
    raw.as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Edition must be an object"))?
        .remove("subtitle");
    let mut concepts = BTreeMap::<String, Value>::new();
    for c in raw["concepts"].as_array().cloned().unwrap_or_default() {
        concepts.insert(c["id"].as_str().unwrap_or("").into(), c);
    }
    for p in raw["pages"]
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("Missing pages"))?
    {
        if let Some(Value::Array(local)) = p.as_object_mut().unwrap().remove("concepts") {
            for c in local {
                let id = c["id"].as_str().unwrap_or("").to_string();
                if let Some(old) = concepts.get(&id) {
                    ensure!(*old == c, "Conflicting concept definitions")
                }
                concepts.insert(id, c);
            }
        }
    }
    raw["concepts"] = json!(concepts.into_values().collect::<Vec<_>>());
    let source_map = records
        .iter()
        .map(|s| (s.id.as_str(), s))
        .collect::<BTreeMap<_, _>>();
    let mut wanted = BTreeMap::new();
    let coverage = raw["coverage_start"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(10)
        .collect::<String>();
    for p in raw["pages"].as_array_mut().unwrap() {
        for story in p["stories"]
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing stories"))?
        {
            if story["source_ids"].is_null()
                && let Some(evidence) = story["evidence"].as_object()
                && !evidence.is_empty()
            {
                story["source_ids"] = json!(evidence.keys().collect::<Vec<_>>());
            }
            let ids = story["source_ids"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing citations"))?
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect::<Vec<_>>();
            if !story["evidence"].is_object() {
                story["evidence"] = json!({});
            }
            for id in &ids {
                let s = source_map
                    .get(id.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Invented citation"))?;
                wanted.insert(id.clone(), *s);
                if story["evidence"][id].as_str().is_none_or(|e| e.is_empty()) {
                    story["evidence"][id] = json!(format!(
                        "Original source excerpt for verification: {}",
                        s.excerpt.chars().take(1200).collect::<String>()
                    ));
                }
            }
            if story["status"] == "New" {
                if ids
                    .iter()
                    .any(|id| source_map[id.as_str()].published.is_none())
                {
                    story["status"] = json!("Learning")
                } else if ids.iter().any(|id| {
                    source_map[id.as_str()]
                        .published
                        .as_ref()
                        .is_some_and(|d| d.get(..10).is_some_and(|d| d < coverage.as_str()))
                }) {
                    story["status"] = json!("Revisited")
                }
            }
        }
    }
    raw["sources"] = json!(wanted.into_values().collect::<Vec<_>>());
    let mut concept_sources = BTreeMap::new();
    for page in raw["pages"].as_array().unwrap() {
        for story in page["stories"].as_array().unwrap() {
            for concept in story["concepts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if let Some(source) = story["source_ids"][0]
                    .as_str()
                    .and_then(|id| source_map.get(id))
                {
                    concept_sources
                        .entry(concept.to_string())
                        .or_insert(source.url.clone());
                }
            }
        }
    }
    for concept in raw["concepts"].as_array_mut().unwrap() {
        if crate::model::safe_url(concept["url"].as_str().unwrap_or("")).is_err()
            && let Some(url) = concept["id"]
                .as_str()
                .and_then(|id| concept_sources.get(id))
        {
            concept["url"] = json!(url);
        }
    }
    let edition: Edition = serde_json::from_value(raw)?;
    edition.validate()?;
    Ok(edition)
}
