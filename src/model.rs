use anyhow::{Result, ensure};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub title: String,
    pub publisher: String,
    pub url: String,
    #[serde(default)]
    pub published: Option<String>,
    pub retrieved_at: String,
    #[serde(default)]
    pub excerpt: String,
    #[serde(default = "ai")]
    pub page: String,
    #[serde(default = "authority")]
    pub authority: f64,
    #[serde(default)]
    pub event_id: String,
}
fn ai() -> String {
    "ai".into()
}
fn authority() -> f64 {
    0.8
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Concept {
    pub id: String,
    pub term: String,
    pub definition: String,
    pub why_it_matters: String,
    pub url: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Story {
    pub id: String,
    pub title: String,
    pub kicker: String,
    pub lane: String,
    pub status: String,
    pub paragraphs: Vec<String>,
    pub implication: String,
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub concepts: Vec<String>,
    #[serde(default)]
    pub evidence: BTreeMap<String, String>,
}
impl Story {
    pub fn words(&self) -> usize {
        self.paragraphs
            .iter()
            .map(|p| p.split_whitespace().count())
            .sum::<usize>()
            + self.implication.split_whitespace().count()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub lead: String,
    pub stories: Vec<Story>,
    pub takeaway: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edition {
    #[serde(default = "one")]
    pub schema_version: u32,
    pub id: String,
    #[serde(default = "one")]
    pub revision: u32,
    pub kind: String,
    pub date: String,
    pub cutoff: DateTime<FixedOffset>,
    pub coverage_start: DateTime<FixedOffset>,
    pub week_start: String,
    #[serde(default)]
    pub inaugural: bool,
    #[serde(default)]
    pub partial_week: bool,
    pub title: String,
    pub note: String,
    pub coverage_notes: Vec<String>,
    pub profile_version: String,
    pub editor: String,
    pub pages: Vec<Page>,
    pub sources: Vec<Source>,
    pub concepts: Vec<Concept>,
}
fn one() -> u32 {
    1
}
pub fn safe_url(value: &str) -> Result<url::Url> {
    let u = url::Url::parse(value)?;
    ensure!(
        matches!(u.scheme(), "http" | "https")
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none(),
        "Invalid public URL"
    );
    ensure!(
        u.port().is_none_or(|p| p == 80 || p == 443),
        "Invalid URL port"
    );
    Ok(u)
}
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
impl Edition {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.kind.as_str(), "daily" | "weekly")
                && self.revision > 0
                && self.schema_version == 1,
            "Invalid edition type"
        );
        chrono::NaiveDate::parse_from_str(&self.date, "%Y-%m-%d")?;
        ensure!(
            self.id == format!("{}-{}", self.kind, self.date),
            "Edition identity mismatch"
        );
        ensure!(
            self.coverage_start <= self.cutoff,
            "Invalid coverage window"
        );
        ensure!(
            self.pages.iter().map(|p| p.id.as_str()).collect::<Vec<_>>() == ["ai", "music"],
            "Exactly AI then music pages required"
        );
        let sources: BTreeMap<_, _> = self.sources.iter().map(|s| (s.id.as_str(), s)).collect();
        ensure!(sources.len() == self.sources.len(), "Duplicate sources");
        let concepts: HashSet<_> = self.concepts.iter().map(|c| c.id.as_str()).collect();
        ensure!(concepts.len() == self.concepts.len(), "Duplicate concepts");
        for c in &self.concepts {
            ensure!(
                valid_id(&c.id) && !c.why_it_matters.trim().is_empty(),
                "Invalid concept"
            );
            safe_url(&c.url)?;
        }
        for s in &self.sources {
            safe_url(&s.url)?;
            ensure!(
                s.authority.is_finite() && (0.0..=1.0).contains(&s.authority),
                "Invalid authority"
            );
            if let Some(date) = &s.published {
                ensure!(
                    date.get(..10)
                        .is_some_and(|d| d <= self.cutoff.date_naive().to_string().as_str()),
                    "Future source"
                );
                if date.contains('T') {
                    ensure!(
                        DateTime::parse_from_rfc3339(date)? <= self.cutoff,
                        "Source after cutoff"
                    );
                }
            }
        }
        let mut seen = HashSet::new();
        for p in &self.pages {
            ensure!(
                p.stories
                    .iter()
                    .flat_map(|s| &s.concepts)
                    .collect::<HashSet<_>>()
                    .len()
                    >= 2,
                "{}: at least two explained concepts required",
                p.id
            );
            ensure!(
                p.stories.len() == 6
                    && p.stories.iter().filter(|s| s.lane == "broad").count() == 4
                    && p.stories.iter().filter(|s| s.lane == "personal").count() == 2,
                "Six stories: four broad, two personal required"
            );
            let total: usize = p.stories.iter().map(Story::words).sum();
            let broad: usize = p
                .stories
                .iter()
                .filter(|s| s.lane == "broad")
                .map(Story::words)
                .sum();
            ensure!(
                total > 0 && total <= 680 && (0.55..=0.65).contains(&(broad as f64 / total as f64)),
                "{}: 60/40 word allocation or print budget violated ({broad}/{total})",
                p.id
            );
            for s in &p.stories {
                ensure!(
                    valid_id(&s.id) && seen.insert(&s.id),
                    "Duplicate or invalid story ID"
                );
                ensure!(
                    (1..=3).contains(&s.paragraphs.len()) && !s.source_ids.is_empty(),
                    "Missing story text/citations"
                );
                ensure!(
                    [
                        "New",
                        "Recent",
                        "Revisited",
                        "Learning",
                        "Early signal",
                        "Trend"
                    ]
                    .contains(&s.status.as_str()),
                    "Invalid status"
                );
                for cid in &s.concepts {
                    ensure!(concepts.contains(cid.as_str()), "Unknown concept");
                }
                for sid in &s.source_ids {
                    let source = sources
                        .get(sid.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Unknown citation"))?;
                    ensure!(
                        s.evidence.get(sid).is_some_and(|s| !s.trim().is_empty()),
                        "Missing claim evidence"
                    );
                    if s.status == "New" {
                        ensure!(
                            source
                                .published
                                .as_ref()
                                .is_some_and(|d| d
                                    .get(..10)
                                    .is_some_and(|d| d
                                        >= self.coverage_start.date_naive().to_string().as_str())),
                            "Old or undated source labeled New"
                        );
                    }
                }
                if s.status == "Trend" {
                    ensure!(
                        s.source_ids
                            .iter()
                            .filter_map(|id| sources.get(id.as_str()))
                            .filter(|s| s.published.is_some())
                            .map(|s| &s.url)
                            .collect::<HashSet<_>>()
                            .len()
                            >= 2,
                        "Trend requires distinct dated sources"
                    );
                }
            }
        }
        Ok(())
    }
    pub fn public(&self) -> Result<serde_json::Value> {
        let mut value = serde_json::to_value(self)?;
        for source in value["sources"].as_array_mut().unwrap() {
            source.as_object_mut().unwrap().remove("excerpt");
        }
        for page in value["pages"].as_array_mut().unwrap() {
            for story in page["stories"].as_array_mut().unwrap() {
                story.as_object_mut().unwrap().remove("evidence");
            }
        }
        Ok(value)
    }
    pub fn minutes(&self) -> usize {
        let words = self
            .pages
            .iter()
            .map(|p| {
                p.lead.split_whitespace().count()
                    + p.stories.iter().map(Story::words).sum::<usize>()
            })
            .sum::<usize>()
            + self
                .concepts
                .iter()
                .map(|c| {
                    c.definition.split_whitespace().count()
                        + c.why_it_matters.split_whitespace().count()
                })
                .sum::<usize>();
        words.div_ceil(180).max(1)
    }
}
