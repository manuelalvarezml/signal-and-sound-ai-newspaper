use crate::model::Edition;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub fn root() -> PathBuf {
    PathBuf::from(std::env::var("APP_ROOT").unwrap_or_else(|_| ".".into()))
}
pub fn pdf(edition: &Edition, path: &Path, site_url: &str) -> Result<()> {
    edition.validate()?;
    let dir = tempfile::tempdir()?;
    std::fs::write(
        dir.path().join("edition.json"),
        serde_json::to_vec(
            &json!({"edition":edition,"minutes":edition.minutes(),"site_url":site_url,"cutoff":edition.cutoff.with_timezone(&chrono_tz::America::Lima).format("%H:%M").to_string()}),
        )?,
    )?;
    std::fs::copy(
        root().join("print/newspaper.typ"),
        dir.path().join("newspaper.typ"),
    )?;
    let output =
        std::process::Command::new(std::env::var("TYPST_BIN").unwrap_or_else(|_| "typst".into()))
            .arg("compile")
            .arg("--ignore-system-fonts")
            .arg("--font-path")
            .arg(root().join("print/fonts"))
            .arg("--root")
            .arg(dir.path())
            .arg(dir.path().join("newspaper.typ"))
            .arg(path)
            .output()?;
    ensure!(
        output.status.success(),
        "Typst failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document = lopdf::Document::load(path)?;
    ensure!(
        document.get_pages().len() == 2,
        "PDF overflow: exactly two A4 pages required; shorten copy"
    );
    for (_, id) in document.get_pages() {
        let page = document.get_object(id)?.as_dict()?;
        let bounds = page.get(b"MediaBox")?.as_array()?;
        let number = |i: usize| {
            bounds[i]
                .as_float()
                .map(|n| n as f64)
                .or_else(|_| bounds[i].as_i64().map(|n| n as f64))
        };
        ensure!(
            (number(2)? - 595.276).abs() < 1.0 && (number(3)? - 841.89).abs() < 1.0,
            "PDF must be A4"
        );
    }
    Ok(())
}
pub fn render(edition: &Edition, dist: &Path, site_url: &str) -> Result<PathBuf> {
    edition.validate()?;
    std::fs::create_dir_all(dist)?;
    let release = format!("editions/{}/r{}", edition.id, edition.revision);
    let final_target = dist.join(&release);
    let existing = final_target.join("edition.json");
    let public = edition.public()?;
    if existing.exists() {
        ensure!(
            serde_json::from_slice::<Value>(&std::fs::read(&existing)?)? == public,
            "Published revisions are immutable; increment revision"
        );
    }
    let staging = tempfile::tempdir_in(dist)?;
    let target = staging.path();
    let mut asset_files = walkdir::WalkDir::new(root().join("web/assets"))
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect::<Vec<_>>();
    asset_files.sort();
    let asset_bytes = asset_files
        .iter()
        .map(std::fs::read)
        .collect::<std::io::Result<Vec<_>>>()?
        .concat();
    let asset_base = format!("/assets/{}", &crate::hash(&asset_bytes)[..12]);
    let asset_dest = dist.join(asset_base.trim_start_matches('/'));
    std::fs::create_dir_all(&asset_dest)?;
    for file in &asset_files {
        std::fs::copy(file, asset_dest.join(file.file_name().unwrap()))?;
    }
    let mut env = minijinja::Environment::new();
    env.set_loader(minijinja::path_loader(root().join("web")));
    let sources = edition
        .sources
        .iter()
        .map(|s| (&s.id, s))
        .collect::<BTreeMap<_, _>>();
    let concepts = edition
        .concepts
        .iter()
        .map(|s| (&s.id, s))
        .collect::<BTreeMap<_, _>>();
    let links =
        json!({"ai":format!("/{release}/ai.html"),"music":format!("/{release}/music.html")});
    pdf(edition, &target.join("signal-and-sound.pdf"), site_url)?;
    std::fs::copy(
        target.join("signal-and-sound.pdf"),
        target.join("print.pdf"),
    )?;
    for (i, page) in edition.pages.iter().enumerate() {
        let context = json!({"edition":edition,"page":page,"page_number":i+1,"page_number_label":format!("{:02}",i+1),"asset_base":asset_base,"print_pdf":format!("/{release}/print.pdf"),"sources":sources,"concepts":concepts,"links":links,"pdf":format!("/{release}/signal-and-sound.pdf"),"cutoff":edition.cutoff.with_timezone(&chrono_tz::America::Lima).format("%-I:%M %p").to_string(),"minutes":edition.minutes(),"date_label":edition.cutoff.with_timezone(&chrono_tz::America::Lima).format("%A, %d %B %Y").to_string()});
        std::fs::write(
            target.join(format!("{}.html", page.id)),
            env.get_template("edition.html")?.render(context)?,
        )?;
    }
    std::fs::write(
        target.join("edition.json"),
        serde_json::to_vec_pretty(&public)?,
    )?;
    if !existing.exists() {
        ensure!(
            !final_target.exists(),
            "Incomplete release exists; use new revision"
        );
        std::fs::create_dir_all(final_target.parent().unwrap())?;
        std::fs::rename(target, &final_target)?;
    }
    let mut pointer = if dist.join("latest.json").exists() {
        serde_json::from_slice::<Value>(&std::fs::read(dist.join("latest.json"))?)?
    } else {
        json!({})
    };
    let new = json!({"id":edition.id,"date":edition.date,"revision":edition.revision,"cutoff":edition.cutoff,"pages":links,"pdf":format!("/{release}/signal-and-sound.pdf")});
    let old = &pointer[&edition.kind];
    if old.is_null()
        || (edition.date.as_str(), edition.revision as u64)
            >= (
                old["date"].as_str().unwrap_or(""),
                old["revision"].as_u64().unwrap_or(0),
            )
    {
        pointer[&edition.kind] = new;
    }
    for (route, section, kind) in [
        ("index.html", "ai", "daily"),
        ("ai.html", "ai", "daily"),
        ("music.html", "music", "daily"),
        ("weekly.html", "ai", "weekly"),
    ] {
        std::fs::write(
            dist.join(route),
            env.get_template("latest.html")?
                .render(json!({"asset_base":asset_base,"section":section,"kind":kind}))?,
        )?;
    }
    let mut entries = if dist.join("archive.json").exists() {
        serde_json::from_slice::<Vec<Value>>(&std::fs::read(dist.join("archive.json"))?)?
    } else {
        vec![]
    };
    let entry = json!({"id":edition.id,"date":edition.date,"kind":edition.kind,"revision":edition.revision,"url":format!("/{release}/ai.html"),"pdf":format!("/{release}/signal-and-sound.pdf")});
    if let Some(old) = entries.iter_mut().find(|e| e["id"] == edition.id) {
        if old["revision"].as_u64().unwrap_or(0) <= edition.revision as u64 {
            *old = entry
        }
    } else {
        entries.push(entry)
    }
    entries.sort_by(|a, b| {
        b["date"]
            .as_str()
            .cmp(&a["date"].as_str())
            .then(b["kind"].as_str().cmp(&a["kind"].as_str()))
    });
    std::fs::write(
        dist.join("archive.html"),
        env.get_template("archive.html")?
            .render(json!({"entries":entries,"asset_base":asset_base}))?,
    )?;
    std::fs::write(
        dist.join("archive.json"),
        serde_json::to_vec_pretty(&entries)?,
    )?;
    std::fs::write(
        dist.join("latest.tmp"),
        serde_json::to_vec_pretty(&pointer)?,
    )?;
    std::fs::rename(dist.join("latest.tmp"), dist.join("latest.json"))?;
    Ok(final_target)
}
