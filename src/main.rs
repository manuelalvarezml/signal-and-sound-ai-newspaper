use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use signal_and_sound::{
    aws::Aws,
    config,
    model::Edition,
    pipeline::Runtime,
    state::{Budget, State, Store},
};
use std::{path::PathBuf, sync::Arc};

#[derive(Parser)]
#[command(version, about = "Signal & Sound: a Rust + Typst morning newspaper")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    CheckCloud {
        #[arg(long)]
        pipeline: bool,
        #[arg(long, default_value = "rust-migration-20261022")]
        run_id: String,
    },
    Render {
        #[arg(long)]
        edition: PathBuf,
        #[arg(long, default_value = "dist")]
        dist: PathBuf,
        #[arg(long, default_value = "http://localhost:8787")]
        site_url: String,
    },
    Run {
        #[arg(long, default_value = "daily")]
        kind: String,
        #[arg(long)]
        cutoff: Option<String>,
    },
    Collect {
        #[arg(long)]
        cutoff: Option<String>,
    },
    Serve {
        #[arg(long, default_value_t = 8787)]
        port: u16,
        #[arg(long, default_value = "dist")]
        dist: PathBuf,
    },
    Interests {
        observations: PathBuf,
    },
    SyncInterests,
    Status,
    PrepareDeploy {
        #[arg(long)]
        image_uri: String,
        #[arg(long)]
        output: PathBuf,
    },
}
fn at(value: Option<String>) -> Result<chrono::DateTime<chrono::FixedOffset>> {
    Ok(value
        .map(|s| chrono::DateTime::parse_from_rfc3339(&s))
        .transpose()?
        .unwrap_or_else(|| {
            chrono::Utc::now()
                .with_timezone(&chrono_tz::America::Lima)
                .fixed_offset()
        }))
}
#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("AWS_LAMBDA_RUNTIME_API").is_ok() {
        let runtime = Arc::new(Runtime::cloud().await?);
        let role = std::env::var("APP_ROLE").unwrap_or_else(|_| "collector".into());
        lambda_runtime::run(lambda_runtime::service_fn(
            move |event: lambda_runtime::LambdaEvent<Value>| {
                let runtime = runtime.clone();
                let role = role.clone();
                async move {
                    eprintln!("{}", json!({"event":"run_started","role":role,"request_id":event.context.request_id}));
                    let result = if role == "publisher" {
                        runtime.publisher(event.payload).await
                    } else {
                        runtime
                            .collector(event.payload, &event.context.request_id)
                            .await
                    };
                    match &result {
                        Ok(value) => eprintln!("{}", json!({"event":"run_succeeded","role":role,"request_id":event.context.request_id,"result":value})),
                        Err(error) => {
                            let failure = json!({"event":"run_failed","role":role,"request_id":event.context.request_id,"at":chrono::Utc::now(),"error":format!("{error:#}")});
                            eprintln!("{failure}");
                            if let Err(save_error) = runtime.evidence.put_json(
                                &format!("failures/{role}/{}.json",event.context.request_id), &failure
                            ).await {
                                eprintln!("Could not save failure record: {save_error}");
                            }
                        }
                    }
                    result.map_err(|e| -> lambda_runtime::Error { e.into() })
                }
            },
        ))
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        return Ok(());
    }
    match Cli::parse().command {
        Command::CheckCloud { pipeline, run_id } => {
            let runtime = Runtime::cloud().await?;
            println!(
                "{}",
                runtime
                    .collector(
                        json!({"action":if pipeline {"check_pipeline"}else{"check_search"}}),
                        &run_id
                    )
                    .await?
            );
        }
        Command::Render {
            edition,
            dist,
            site_url,
        } => {
            let edition: Edition = serde_json::from_slice(&std::fs::read(edition)?)?;
            println!(
                "{}",
                signal_and_sound::render::render(&edition, &dist, &site_url)?.display()
            );
        }
        Command::Serve { port, dist } => {
            use axum::{
                extract::Request,
                http::HeaderValue,
                middleware::{self, Next},
                response::Response,
            };
            async fn headers(request: Request, next: Next) -> Response {
                let path = request.uri().path().to_string();
                let mut response = next.run(request).await;
                if path.ends_with(".pdf") {
                    let mode = if path.ends_with("/print.pdf") {
                        "inline"
                    } else {
                        "attachment"
                    };
                    response.headers_mut().insert(
                        "content-disposition",
                        HeaderValue::from_str(&format!(
                            "{mode}; filename=\"signal-and-sound.pdf\""
                        ))
                        .unwrap(),
                    );
                }
                response
            }
            let app = axum::Router::new()
                .fallback_service(tower_http::services::ServeDir::new(dist))
                .layer(middleware::from_fn(headers));
            let listener =
                tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
            println!("http://localhost:{port}");
            axum::serve(listener, app).await?;
        }
        Command::Interests { observations } => {
            let path = PathBuf::from("private/interests.json");
            let profile: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            let observations: Vec<Value> = serde_json::from_slice(&std::fs::read(observations)?)?;
            let (updated, log) =
                signal_and_sound::interests::apply(&profile, &observations, at(None)?)?;
            let history = PathBuf::from("private/interest-history");
            std::fs::create_dir_all(&history)?;
            std::fs::write(
                history.join(format!("{}.json", profile["version"].as_str().unwrap())),
                serde_json::to_vec_pretty(&profile)?,
            )?;
            std::fs::write(path, serde_json::to_vec_pretty(&updated)?)?;
            std::fs::write(
                "config/interests.json",
                serde_json::to_vec_pretty(&signal_and_sound::interests::sanitized(&updated))?,
            )?;
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("private/interest-updates.jsonl")?;
            for row in &log {
                writeln!(file, "{}", serde_json::to_string(row)?)?
            }
            println!(
                "{}",
                json!({"version":updated["version"],"changes":log.len()})
            );
        }
        Command::SyncInterests => {
            let aws = Arc::new(Aws::new().await?);
            let bucket = std::env::var("EVIDENCE_BUCKET")?;
            let profile = config("interests")?;
            Store::S3 { aws, bucket }
                .put_json(
                    "config/interests.json",
                    &signal_and_sound::interests::sanitized(&profile),
                )
                .await?;
            println!("Sanitized interest topics synced");
        }
        Command::PrepareDeploy { image_uri, output } => {
            fn convert(v: serde_yaml::Value) -> Result<Value> {
                Ok(match v {
                    serde_yaml::Value::Tagged(t) => {
                        let name = t.tag.to_string().trim_start_matches('!').to_string();
                        let mut value = convert(t.value)?;
                        if name == "GetAtt"
                            && let Some(s) = value.as_str()
                        {
                            value = json!(s.splitn(2, '.').collect::<Vec<_>>())
                        }
                        json!({if name=="Ref"{name}else{format!("Fn::{name}")}:value})
                    }
                    serde_yaml::Value::Mapping(m) => Value::Object(
                        m.into_iter()
                            .map(|(k, v)| {
                                Ok((
                                    k.as_str()
                                        .ok_or_else(|| anyhow::anyhow!("Invalid YAML key"))?
                                        .to_string(),
                                    convert(v)?,
                                ))
                            })
                            .collect::<Result<_>>()?,
                    ),
                    serde_yaml::Value::Sequence(s) => {
                        Value::Array(s.into_iter().map(convert).collect::<Result<_>>()?)
                    }
                    other => serde_json::to_value(other)?,
                })
            }
            let mut template = convert(serde_yaml::from_slice(&std::fs::read(
                "infra/template.yaml",
            )?)?)?;
            for r in template["Resources"].as_object_mut().unwrap().values_mut() {
                if r["Type"] == "AWS::Serverless::Function" {
                    r.as_object_mut().unwrap().remove("Metadata");
                    r["Properties"]["ImageUri"] = json!(image_uri);
                } else if r["Type"] == "AWS::PricingPlanManager::Subscription" {
                    r.as_object_mut().unwrap().remove("Metadata");
                }
            }
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?
            }
            std::fs::write(output, serde_json::to_vec_pretty(&template)?)?;
        }
        Command::Status => {
            let settings = config("settings")?;
            let budget = Budget::new(State::local("state".into())?, settings.clone());
            println!(
                "{}",
                json!({"settings":settings,"budget":budget.state.read(&budget.key).await?})
            );
        }
        command => {
            let aws = Arc::new(Aws::new().await?);
            let runtime = Runtime {
                aws,
                settings: config("settings")?,
                state: State::local("state".into())?,
                evidence: Store::Local("private/store".into()),
                public: Store::Local("dist".into()),
            };
            let exa = runtime.secret("EXA_API_KEY").await?;
            let x = runtime.secret("X_BEARER_TOKEN").await?;
            match command {
                Command::Run { kind, cutoff } => {
                    ensure!(
                        matches!(kind.as_str(), "daily" | "weekly"),
                        "Invalid edition kind"
                    );
                    let edition = runtime
                        .analyze(at(cutoff)?, &kind, exa.as_deref(), x.as_deref())
                        .await?;
                    println!(
                        "{}",
                        signal_and_sound::render::render(
                            &edition,
                            &PathBuf::from("dist"),
                            runtime.settings["site_url"].as_str().unwrap()
                        )?
                        .display()
                    );
                }
                Command::Collect { cutoff } => {
                    let at = at(cutoff)?;
                    let budget = Budget::new(runtime.state.clone(), runtime.settings.clone());
                    let result = signal_and_sound::collect::collect(
                        &config("sources")?,
                        &runtime.settings,
                        &config("interests")?,
                        at,
                        &budget,
                        &runtime.evidence,
                        &format!("discovery-{}", at.format("%Y-%m-%dT%H%M")),
                        false,
                        exa.as_deref(),
                        x.as_deref(),
                    )
                    .await?;
                    println!(
                        "{}",
                        json!({"records":result["records"].as_array().map(Vec::len),"candidates":result["metadata_candidates"],"coverage":result["coverage_notes"],"search":result["search_provenance"]})
                    );
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(())
}
