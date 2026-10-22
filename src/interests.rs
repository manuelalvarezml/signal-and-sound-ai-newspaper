use anyhow::{Result, ensure};
use chrono::{DateTime, Datelike, Duration, FixedOffset, TimeZone};
use chrono_tz::America::Lima;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub fn week_start(at: DateTime<FixedOffset>) -> String {
    let at = at.with_timezone(&Lima);
    (at.date_naive() - Duration::days(at.weekday().num_days_from_monday() as i64)).to_string()
}
pub fn coverage(
    at: DateTime<FixedOffset>,
    kind: &str,
    started: &str,
) -> Result<(DateTime<FixedOffset>, DateTime<FixedOffset>, bool)> {
    let started = DateTime::parse_from_rfc3339(started)?;
    if kind == "weekly" {
        let day = chrono::NaiveDate::parse_from_str(&week_start(at), "%Y-%m-%d")?;
        let end = Lima
            .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
            .single()
            .unwrap()
            .fixed_offset();
        let normal = end - Duration::days(7);
        let start = started.max(normal);
        ensure!(start < end, "No completed publication week exists yet");
        Ok((start, end, start > normal))
    } else {
        Ok((started.max(at - Duration::days(1)), at, false))
    }
}
pub fn sanitized(profile: &Value) -> Value {
    json!({"version":profile["version"],"last_reviewed":profile["last_reviewed"],"reading_level":"practitioner; explain mechanisms briefly; do not assume mastery", "topics":profile["topics"].as_array().unwrap().iter().map(|t|json!({"id":t["id"],"label":t["label"],"keywords":t["keywords"],"weight":t["weight"],"page":t["page"]})).collect::<Vec<_>>()})
}
pub fn apply(
    profile: &Value,
    observations: &[Value],
    at: DateTime<FixedOffset>,
) -> Result<(Value, Vec<Value>)> {
    let mut updated = profile.clone();
    let mut known: BTreeSet<String> = updated["applied_evidence"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let mut log = vec![];
    for o in observations {
        ensure!(
            ["topic", "source", "date", "kind", "reason"]
                .iter()
                .all(|k| o[*k].is_string())
                && matches!(o["kind"].as_str(), Some("explicit" | "work")),
            "Observation requires provenance"
        );
        if o["automated"] == true {
            continue;
        }
        let identity = crate::hash(&serde_json::to_vec(o)?);
        if known.contains(&identity) || known.contains(&crate::hash(legacy_json(o).as_bytes())) {
            continue;
        }
        let topic = updated["topics"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|t| t["id"] == o["topic"])
            .ok_or_else(|| anyhow::anyhow!("New topics require curated definitions"))?;
        let before = topic["weight"].as_f64().unwrap_or(0.5);
        if o["kind"] == "explicit" {
            let weight = o["weight"]
                .as_f64()
                .ok_or_else(|| anyhow::anyhow!("Explicit weight required"))?;
            ensure!((0.0..=1.0).contains(&weight), "Invalid weight");
            topic["weight"] = json!(weight);
            topic["explicit"] = json!(true);
            topic["confidence"] = json!(1.0);
        } else if topic["explicit"] != true && topic["adjusted_week"] != week_start(at) {
            topic["weight"] = json!(((before + 0.05).min(1.0) * 1000.0).round() / 1000.0);
            topic["confidence"] =
                json!((topic["confidence"].as_f64().unwrap_or(0.5) + 0.05).min(0.9));
            topic["adjusted_week"] = json!(week_start(at));
        }
        topic["last_evidence"] = o["date"].clone();
        let mut row = o.clone();
        row["before"] = json!(before);
        row["after"] = topic["weight"].clone();
        row["evidence_id"] = json!(identity);
        known.insert(identity);
        log.push(row);
    }
    updated["applied_evidence"] = json!(known);
    updated["last_reviewed"] = json!(at.to_rfc3339());
    updated["version"] = json!(format!(
        "{}-{}",
        at.date_naive(),
        &crate::hash(&serde_json::to_vec(&updated["topics"])?)[..8]
    ));
    Ok((updated, log))
}

// Preserve observation IDs written by the previous JSON serializer during migration.
pub fn legacy_json(value: &Value) -> String {
    match value {
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(legacy_json).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", legacy_json(&json!(k)), legacy_json(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => serde_json::to_string(value)
            .unwrap()
            .chars()
            .map(|c| {
                if c.is_ascii() {
                    c.to_string()
                } else {
                    c.encode_utf16(&mut [0; 2])
                        .iter()
                        .map(|v| format!("\\u{v:04x}"))
                        .collect::<String>()
                }
            })
            .collect(),
    }
}
