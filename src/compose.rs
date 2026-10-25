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
    let mut text = if text.starts_with("```json") || text.starts_with("```\n") {
        text.split_once('\n').map(|(_, s)| s).unwrap_or(text)
    } else {
        text
    };
    // Bedrock's constrained open-weight response can duplicate its opening
    // brace. Remove only a keyless duplicate prefix, never any JSON field.
    while let Some(rest) = text.trim_start().strip_prefix('{') {
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('"').unwrap_or(rest).trim_start();
        if !rest.starts_with('{') {
            break;
        }
        text = rest;
    }
    // Some models append prose after a valid JSON object. Decode just the first
    // object; downstream typed schemas still validate every consumed field.
    let data = serde_json::Deserializer::from_str(text)
        .into_iter::<Value>()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Empty model response"))??;
    ensure!(data.is_object(), "Model must return a JSON object");
    Ok(data)
}

pub fn apply_editorial_patch(mut draft: Value, patch: &Value, allowed: &[String]) -> Result<Value> {
    for changed in patch["stories"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Missing story patches"))?
    {
        let id = changed["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing patch ID"))?;
        ensure!(
            allowed.iter().any(|s| s == id),
            "Repair attempted to change an unflagged story"
        );
        let story = draft["pages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .flat_map(|p| p["stories"].as_array_mut().unwrap())
            .find(|s| s["id"] == id)
            .ok_or_else(|| anyhow::anyhow!("Unknown patched story"))?;
        *story = changed.clone();
    }
    for changed in patch["concepts"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Missing concept patches"))?
    {
        let concept = draft["concepts"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|c| c["id"] == changed["id"])
            .ok_or_else(|| anyhow::anyhow!("Unknown patched concept"))?;
        *concept = changed.clone();
    }
    for changed in patch["leads"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Missing lead patches"))?
    {
        let page = draft["pages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|p| p["id"] == changed["id"])
            .ok_or_else(|| anyhow::anyhow!("Unknown patched page"))?;
        page["lead"] = changed["lead"].clone();
    }
    Ok(draft)
}

pub struct Composer<'a> {
    pub aws: Arc<Aws>,
    pub settings: &'a Value,
    pub budget: &'a Budget,
    pub store: &'a Store,
    pub run_id: &'a str,
}
fn stage_key(stage: &str, model: &str, prompt: &Value, maximum: u64) -> Result<String> {
    Ok(format!(
        "{stage}-v8-{}",
        &crate::hash(&serde_json::to_vec(
            &json!({"model":model,"prompt":prompt,"system":SYSTEM,"maximum":maximum})
        )?)[..16]
    ))
}
impl Composer<'_> {
    async fn call(&self, stage: &str, model: &str, prompt: &Value, maximum: u64) -> Result<Value> {
        match self.call_once(stage, model, prompt, maximum).await {
            Err(error)
                if error.is::<serde_json::Error>()
                    || error.to_string() == "Model must return a JSON object" =>
            {
                // Regenerate malformed output once using the original evidence.
                // Do not mistake a JSON formatting failure for a factual error
                // and rewrite unrelated stories. Both responses remain cached.
                let mut retry = prompt.clone();
                retry["output_recovery"] = json!(
                    "Your prior response could not be parsed as JSON. Answer the original request using one valid JSON object matching the required schema. No duplicated opening braces, commentary or markdown. Preserve all evidence requirements; do not assume approval."
                );
                self.call_once(&format!("{stage}-format-retry"), model, &retry, maximum)
                    .await
            }
            result => result,
        }
    }
    async fn call_once(
        &self,
        stage: &str,
        model: &str,
        prompt: &Value,
        maximum: u64,
    ) -> Result<Value> {
        // Every stage is keyed by its exact inputs. A corrected source set or
        // prompt must never reuse an incompatible writer/reviewer checkpoint.
        let stage = stage_key(stage, model, prompt, maximum)?;
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
                } else if stage.starts_with("patch-") {
                    let mut base: Value =
                        serde_json::from_str(include_str!("../config/edition.schema.json"))?;
                    let patch_input: Value = serde_json::from_str(&prompt)?;
                    if patch_input["allowed_story_ids"]
                        .as_array()
                        .is_some_and(|ids| !ids.is_empty())
                    {
                        base["$defs"]["Story"]["properties"]["id"]["enum"] =
                            patch_input["allowed_story_ids"].clone();
                    }
                    structured_schema(json!({"type":"object","properties":{
                        "stories":{"type":"array","items":{"$ref":"#/$defs/Story"}},
                        "concepts":{"type":"array","items":{"$ref":"#/$defs/Concept"}},
                        "leads":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string","enum":["ai","music"]},"lead":{"type":"string"}}}}
                    },"$defs":base["$defs"]}))
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
        let at = collection["cutoff"]
            .as_str()
            .map(DateTime::parse_from_rfc3339)
            .transpose()?
            .unwrap_or(at);
        let records: Vec<Source> =
            serde_json::from_value::<Vec<Source>>(collection["records"].clone())?
                .into_iter()
                .filter(|s| {
                    s.excerpt.chars().count() >= 300
                        && crate::model::source_available_at(s.published.as_deref(), at)
                })
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
        let mut raw = self
            .call(
                "write-rust-v5",
                self.settings["compose_model"].as_str().unwrap(),
                &payload,
                self.settings["write_max_tokens"].as_u64().unwrap_or(16000),
            )
            .await?;
        for attempt in 0..5 {
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
                    let stage=format!("verify-rust-{}-{}-{attempt}",if attempt==0{"v5"}else{"v7"},page.id);
                    let verdict=self.call(&stage,self.settings["review_model"].as_str().unwrap_or(self.settings["compose_model"].as_str().unwrap()),&json!({"instruction":"Act as a skeptical fact checker. Review ONLY published_text and concepts against cited_original_evidence. Check every factual assertion, quantity, unit, date, attribution and limitation, including the implication sentences. Each story must be supported by its own source_ids; a source title alone is insufficient evidence for detailed claims. Detect contradictions, invented specifications, local/cloud confusion, latency versus relative speedup confusion, and overstated guarantees. Original evidence is the authority; the writer and its extracted digest are fallible. Return exactly one JSON object {supported:boolean,issues:[string]}. Give precise unsupported claims, story IDs, and corrections supported by the cited source. A vendor's reported result must remain attributed. Clearly conditional practical suggestions are allowed. Set supported false if any factual claim lacks support or contradicts its source. Do not evaluate assertions merely quoted within the original source excerpts as newspaper text.","published_text":public["pages"][n],"concepts":concepts,"cited_original_evidence":sources}),6000).await?;
                    if verdict["supported"]!=true || !verdict["issues"].as_array().is_some_and(Vec::is_empty) {
                        issues.push(json!({"page":page.id,"issues":verdict["issues"]}));
                    }
                }
                ensure!(issues.is_empty(),"Evidence review failed: {}",json!(issues));Ok(edition)
            }.await;
            match validated {
                Ok(e) => return Ok(e),
                Err(e) if attempt < 4 => {
                    let error = e
                        .to_string()
                        .replace(['\u{2011}', '\u{2010}', '\u{2013}'], "-");
                    eprintln!(
                        "Editorial validation attempt {} failed: {}",
                        attempt + 1,
                        error
                    );
                    let allowed = raw["pages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .flat_map(|p| p["stories"].as_array().unwrap())
                        .filter(|s| {
                            !error.starts_with("Evidence review failed")
                                || error.contains(s["id"].as_str().unwrap())
                                || s["source_ids"].as_array().is_some_and(|ids| {
                                    ids.iter()
                                        .filter_map(Value::as_str)
                                        .any(|id| error.contains(id))
                                })
                        })
                        .map(|s| s["id"].as_str().unwrap().to_string())
                        .collect::<Vec<_>>();
                    let flagged = raw["pages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .flat_map(|p| p["stories"].as_array().unwrap())
                        .filter(|s| allowed.iter().any(|id| s["id"] == *id))
                        .cloned()
                        .collect::<Vec<_>>();
                    let cited = flagged
                        .iter()
                        .flat_map(|s| s["source_ids"].as_array().into_iter().flatten())
                        .filter_map(Value::as_str)
                        .collect::<std::collections::HashSet<_>>();
                    let evidence = records
                        .iter()
                        .filter(|s| cited.is_empty() || cited.contains(s.id.as_str()))
                        .collect::<Vec<_>>();
                    let mut patch_prompt = json!({"instruction":"Repair only the flagged claims. Return {stories:[complete replacement Story objects for allowed_story_ids only],concepts:[only changed Concept objects],leads:[only changed {id,lead} page notes]}. Lead IDs can ONLY be ai or music, never a story ID. Use empty arrays when no change is needed. Do not rewrite verified stories. Correct headlines and implications as well as paragraphs. Preserve IDs, source_ids, lane and concept references unless the validation issue explicitly concerns them. Narrow claims to what the original evidence actually demonstrates. Attribute results and state trust assumptions. Remove unsupported specifics rather than substituting new ones. If evidence does not mention a claim, omit it entirely rather than asserting that the study did not test or conclude it. Do not invent guarantees, affiliations, hardware, latency or deployments. Keep each replacement within about five words of its prior word count to preserve the page's 60/40 allocation, unless correcting a word-allocation error. Keep at least two explained concepts per page. Set evidence to {}. Return only the patch JSON.","allowed_story_ids":allowed,"issues":error,"flagged_stories":flagged,"concepts":raw["concepts"],"original_sources":evidence});
                    let patch = self
                        .call(
                            &format!("patch-rust-v7-{attempt}"),
                            self.settings["compose_model"].as_str().unwrap(),
                            &patch_prompt,
                            16000,
                        )
                        .await?;
                    raw = match apply_editorial_patch(raw.clone(), &patch, &allowed) {
                        Ok(updated) => updated,
                        Err(error) => {
                            patch_prompt["patch_validation_error"] = json!(error.to_string());
                            let patch = self
                                .call(
                                    &format!("patch-rust-v9-{attempt}-shape-retry"),
                                    self.settings["compose_model"].as_str().unwrap(),
                                    &patch_prompt,
                                    16000,
                                )
                                .await?;
                            apply_editorial_patch(raw, &patch, &allowed)?
                        }
                    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;

    #[test]
    fn cache_invalidates_changed_evidence_and_verdict_inputs() {
        let original = json!({"sources":[{"id":"a","excerpt":"original"}]});
        let corrected = json!({"sources":[{"id":"a","excerpt":"corrected"}]});
        for stage in [
            "extract",
            "write-rust-v5",
            "verify-rust-v5-ai-0",
            "patch-rust-v7-0",
        ] {
            assert_eq!(
                stage_key(stage, "model", &original, 8000).unwrap(),
                stage_key(stage, "model", &original, 8000).unwrap()
            );
            assert_ne!(
                stage_key(stage, "model", &original, 8000).unwrap(),
                stage_key(stage, "model", &corrected, 8000).unwrap()
            );
            assert_ne!(
                stage_key(stage, "model", &original, 8000).unwrap(),
                stage_key(stage, "model", &original, 16000).unwrap()
            );
        }
    }

    #[tokio::test]
    async fn cached_truncation_and_compact_recovery_do_not_repeat_paid_requests() {
        let tmp = tempfile::tempdir().unwrap();
        let state = State::local(tmp.path().join("state")).unwrap();
        let store = Store::Local(tmp.path().join("evidence"));
        let settings = crate::config("settings").unwrap();
        let budget = Budget::new(state, settings.clone());
        let model = settings["extract_model"].as_str().unwrap();
        let prompt = json!({"sources":["test evidence"]});
        let stage = stage_key("extract", model, &prompt, 8000).unwrap();
        let key = format!("recovery:bedrock:{stage}");
        budget.reserve(&key, "bedrock", 0.1).await.unwrap();
        store.put_json(&format!("checkpoints/recovery/{stage}-response.json"),&json!({"usage":{"inputTokens":100,"outputTokens":100},"stopReason":"max_tokens","output":{"message":{"content":[{"text":"incomplete"}]}}})).await.unwrap();
        let compact = stage_key("extract-compact", model, &prompt, 6000).unwrap();
        store
            .put_json(
                &format!("checkpoints/recovery/{compact}.json"),
                &json!({"items":[]}),
            )
            .await
            .unwrap();
        let composer = Composer {
            aws: Arc::new(Aws::new().await.unwrap()),
            settings: &settings,
            budget: &budget,
            store: &store,
            run_id: "recovery",
        };
        for _ in 0..2 {
            assert!(
                composer
                    .call("extract", model, &prompt, 8000)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("truncated")
            );
            assert_eq!(
                composer
                    .call("extract-compact", model, &prompt, 6000)
                    .await
                    .unwrap(),
                json!({"items":[]})
            );
        }
        assert_eq!(
            budget.state.read(&budget.key).await.unwrap()["requests"]
                .as_object()
                .unwrap()
                .len(),
            1
        );
    }
}
