use crate::aws::Aws;
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub enum Store {
    Local(PathBuf),
    S3 { aws: Arc<Aws>, bucket: String },
}
impl Store {
    fn local_path(root: &Path, key: &str) -> Result<PathBuf> {
        ensure!(
            !key.starts_with('/')
                && Path::new(key)
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
            "Unsafe storage key"
        );
        Ok(root.join(key))
    }
    fn s3_url(aws: &Aws, bucket: &str, key: &str) -> String {
        let mut u = url::Url::parse(&format!(
            "https://{bucket}.s3.{}.amazonaws.com/",
            aws.region
        ))
        .unwrap();
        u.set_path(key);
        u.to_string()
    }
    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        match self {
            Self::Local(root) => {
                let path = Self::local_path(root, key)?;
                if path.exists() {
                    Ok(Some(std::fs::read(path)?))
                } else {
                    Ok(None)
                }
            }
            Self::S3 { aws, bucket } => {
                let r = aws
                    .request("s3", "GET", &Self::s3_url(aws, bucket, key), &[], vec![])
                    .await?;
                if r.status() == 404 {
                    return Ok(None);
                }
                ensure!(r.status().is_success(), "S3 read failed {}", r.status());
                Ok(Some(r.bytes().await?.to_vec()))
            }
        }
    }
    pub async fn json(&self, key: &str) -> Result<Option<Value>> {
        self.get(key)
            .await?
            .map(|b| serde_json::from_slice(&b).map_err(Into::into))
            .transpose()
    }
    pub async fn put_json(&self, key: &str, value: &Value) -> Result<()> {
        self.put(
            key,
            serde_json::to_vec_pretty(value)?,
            "application/json",
            false,
            None,
        )
        .await
    }
    pub async fn put(
        &self,
        key: &str,
        body: Vec<u8>,
        mime: &str,
        immutable: bool,
        disposition: Option<String>,
    ) -> Result<()> {
        match self {
            Self::Local(root) => {
                let path = Self::local_path(root, key)?;
                std::fs::create_dir_all(path.parent().unwrap())?;
                let mut tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
                use std::io::Write;
                tmp.write_all(&body)?;
                tmp.persist(path)?;
            }
            Self::S3 { aws, bucket } => {
                let mut headers = vec![
                    ("content-type", mime.into()),
                    (
                        "cache-control",
                        if immutable {
                            "public,max-age=31536000,immutable"
                        } else {
                            "no-cache"
                        }
                        .into(),
                    ),
                ];
                if let Some(d) = disposition {
                    headers.push(("content-disposition", d));
                }
                let r = aws
                    .request("s3", "PUT", &Self::s3_url(aws, bucket, key), &headers, body)
                    .await?;
                ensure!(r.status().is_success(), "S3 write failed {}", r.status());
            }
        }
        Ok(())
    }
    pub async fn keys(&self, prefix: &str) -> Result<Vec<String>> {
        let mut keys = vec![];
        match self {
            Self::Local(root) => {
                for p in walkdir::WalkDir::new(root)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_file())
                {
                    let key = p.path().strip_prefix(root)?.to_string_lossy().to_string();
                    if key.starts_with(prefix) {
                        keys.push(key)
                    }
                }
            }
            Self::S3 { aws, bucket } => {
                #[derive(serde::Deserialize)]
                struct Item {
                    #[serde(rename = "Key")]
                    key: String,
                }
                #[derive(serde::Deserialize)]
                struct Listing {
                    #[serde(rename = "Contents", default)]
                    contents: Vec<Item>,
                    #[serde(rename = "NextContinuationToken")]
                    next: Option<String>,
                }
                let mut token: Option<String> = None;
                loop {
                    let mut url = url::Url::parse(&Self::s3_url(aws, bucket, ""))?;
                    url.query_pairs_mut()
                        .append_pair("list-type", "2")
                        .append_pair("prefix", prefix);
                    if let Some(t) = &token {
                        url.query_pairs_mut().append_pair("continuation-token", t);
                    }
                    let response = aws.request("s3", "GET", url.as_str(), &[], vec![]).await?;
                    ensure!(response.status().is_success(), "S3 listing failed");
                    let listing: Listing = quick_xml::de::from_str(&response.text().await?)?;
                    keys.extend(listing.contents.into_iter().map(|i| i.key));
                    token = listing.next;
                    if token.is_none() {
                        break;
                    }
                }
            }
        }
        keys.sort();
        Ok(keys)
    }
}
#[derive(Clone)]
pub enum State {
    Local(PathBuf),
    Dynamo { aws: Arc<Aws>, table: String },
}
impl State {
    pub fn local(path: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&path)?;
        let path = path.join("state.sqlite3");
        let c = rusqlite::Connection::open(&path)?;
        c.execute(
            "CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )?;
        Ok(Self::Local(path))
    }
    async fn item(&self, key: &str) -> Result<Value> {
        match self {
            Self::Dynamo { aws, table } => Ok(aws
                .json(
                    "dynamodb",
                    Some("DynamoDB_20120810.GetItem"),
                    "/",
                    &json!({"TableName":table,"Key":{"pk":{"S":key}},"ConsistentRead":true}),
                )
                .await?["Item"]
                .clone()),
            _ => bail!("Not DynamoDB"),
        }
    }
    pub async fn read(&self, key: &str) -> Result<Value> {
        match self {
            Self::Local(path) => {
                let c = rusqlite::Connection::open(path)?;
                use rusqlite::OptionalExtension;
                let s: Option<String> = c
                    .query_row("SELECT value FROM state WHERE key=?", [key], |r| r.get(0))
                    .optional()?;
                Ok(s.map(|s| serde_json::from_str(&s))
                    .transpose()?
                    .unwrap_or(json!({})))
            }
            Self::Dynamo { .. } => {
                let item = self.item(key).await?;
                Ok(serde_json::from_str(
                    item["payload"]["S"].as_str().unwrap_or("{}"),
                )?)
            }
        }
    }
    pub async fn mutate<F, T>(&self, key: &str, f: F) -> Result<T>
    where
        F: Fn(Value) -> Result<(Value, T)>,
    {
        match self {
            Self::Local(path) => {
                let mut c = rusqlite::Connection::open(path)?;
                c.busy_timeout(std::time::Duration::from_secs(30))?;
                let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                use rusqlite::OptionalExtension;
                let s: Option<String> = tx
                    .query_row("SELECT value FROM state WHERE key=?", [key], |r| r.get(0))
                    .optional()?;
                let (v, out) = f(s
                    .map(|s| serde_json::from_str(&s))
                    .transpose()?
                    .unwrap_or(json!({})))?;
                tx.execute(
                    "INSERT OR REPLACE INTO state VALUES (?,?)",
                    [key, &serde_json::to_string(&v)?],
                )?;
                tx.commit()?;
                Ok(out)
            }
            Self::Dynamo { aws, table } => {
                for _ in 0..8 {
                    let old = self.item(key).await?;
                    let version = old["version"]["N"].as_str().unwrap_or("0").parse::<u64>()?;
                    let (v, out) = f(serde_json::from_str(
                        old["payload"]["S"].as_str().unwrap_or("{}"),
                    )?)?;
                    let mut request = json!({"TableName":table,"Item":{"pk":{"S":key},"version":{"N":(version+1).to_string()},"payload":{"S":serde_json::to_string(&v)?}},"ConditionExpression":"attribute_not_exists(pk)"});
                    if !old.is_null() {
                        request["ConditionExpression"] = json!("#v = :v");
                        request["ExpressionAttributeNames"] = json!({"#v":"version"});
                        request["ExpressionAttributeValues"] =
                            json!({":v":{"N":version.to_string()}})
                    }
                    match aws
                        .json("dynamodb", Some("DynamoDB_20120810.PutItem"), "/", &request)
                        .await
                    {
                        Ok(_) => return Ok(out),
                        Err(e) if e.to_string().contains("ConditionalCheckFailedException") => {
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
                bail!("State contention")
            }
        }
    }
    pub async fn claim(&self, id: &str, seconds: i64) -> Result<bool> {
        let now = chrono::Utc::now().timestamp();
        self.mutate(&format!("run:{id}"), |mut v| {
            if v["status"] == "done" || v["lease_until"].as_f64().unwrap_or(0.0) > now as f64 {
                return Ok((v, false));
            }
            v["status"] = json!("running");
            v["lease_until"] = json!(now + seconds);
            Ok((v, true))
        })
        .await
    }
    pub async fn finish(&self, id: &str) -> Result<()> {
        self.mutate(&format!("run:{id}"), |mut v| {
            v["status"] = json!("done");
            v["lease_until"] = json!(0);
            Ok((v, ()))
        })
        .await
    }
    pub async fn release(&self, id: &str) -> Result<()> {
        self.mutate(&format!("run:{id}"), |mut v| {
            v["status"] = json!("idle");
            v["lease_until"] = json!(0);
            Ok((v, ()))
        })
        .await
    }
}
#[derive(Clone)]
pub struct Budget {
    pub state: State,
    pub settings: Value,
    pub key: String,
}
impl Budget {
    pub fn new(state: State, settings: Value) -> Self {
        Self {
            state,
            settings,
            key: format!("budget:{}", chrono::Utc::now().format("%Y-%m")),
        }
    }
    pub async fn reserve(&self, id: &str, provider: &str, dollars: f64) -> Result<()> {
        ensure!(dollars.is_finite() && dollars >= 0.0, "Invalid reservation");
        let micros = (dollars * 1e6).round() as i64;
        self.state
            .mutate(&self.key, |mut v| {
                if !v["requests"].is_object() {
                    v["requests"] = json!({})
                }
                let requests = v["requests"].as_object_mut().unwrap();
                ensure!(
                    !requests.contains_key(id),
                    "Request already reserved; reuse checkpoint or investigate"
                );
                let total: i64 = requests
                    .values()
                    .map(|r| r["micros"].as_i64().unwrap_or(0))
                    .sum();
                let service: i64 = requests
                    .values()
                    .filter(|r| r["provider"] == provider)
                    .map(|r| r["micros"].as_i64().unwrap_or(0))
                    .sum();
                let ceiling = (self.settings["stop_usd"].as_f64().unwrap()
                    - self.settings["infrastructure_reserve_usd"]
                        .as_f64()
                        .unwrap())
                    * 1e6;
                let cap = self.settings[format!("{provider}_monthly_usd")]
                    .as_f64()
                    .unwrap_or(self.settings["stop_usd"].as_f64().unwrap())
                    * 1e6;
                ensure!(
                    (total + micros) as f64 <= ceiling && (service + micros) as f64 <= cap,
                    "Monthly cost limit reached"
                );
                if provider == "search" {
                    ensure!(
                        requests
                            .values()
                            .filter(|r| r["provider"] == "search")
                            .count()
                            < self.settings["search_monthly_limit"].as_u64().unwrap() as usize,
                        "Monthly search limit reached"
                    )
                }
                requests.insert(
                    id.into(),
                    json!({"provider":provider,"micros":micros,"status":"reserved"}),
                );
                Ok((v, ()))
            })
            .await
    }
    pub async fn settle(&self, id: &str, dollars: f64) -> Result<()> {
        ensure!(dollars.is_finite() && dollars >= 0.0, "Invalid charge");
        self.state
            .mutate(&self.key, |mut v| {
                let r = &mut v["requests"][id];
                let actual = (dollars * 1e6).round() as i64;
                ensure!(
                    r["micros"].as_i64().is_some_and(|n| actual <= n),
                    "Actual cost exceeded reservation"
                );
                r["micros"] = json!(actual);
                r["status"] = json!("settled");
                Ok((v, ()))
            })
            .await
    }
}
