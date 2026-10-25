use serde_json::json;
use signal_and_sound::{
    collect, compose, config, interests,
    model::Edition,
    state::{Budget, State, Store},
};
fn edition() -> Edition {
    serde_json::from_str(include_str!("fixtures/edition.json")).unwrap()
}
#[test]
fn compatible_edition_and_private_redaction() {
    let e = edition();
    e.validate().unwrap();
    let p = e.public().unwrap();
    assert!(
        p["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s.get("excerpt").is_none())
    );
    assert!(p["pages"].as_array().unwrap().iter().all(|p| {
        p["stories"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s.get("evidence").is_none())
    }));
}
#[test]
fn model_json_preserves_rejection_despite_trailing_prose() {
    let verdict=compose::parse_model_json("```json\n{\"supported\":false,\"issues\":[\"unsupported latency\"]}\n```\nAdditional explanation").unwrap();
    assert_eq!(verdict["supported"], false);
    assert_eq!(verdict["issues"][0], "unsupported latency");
    assert!(compose::parse_model_json("{\"supported\":true").is_err());
    assert!(compose::parse_model_json("I think it is supported").is_err());
    assert!(compose::parse_model_json("[]").is_err());
    assert_eq!(
        compose::parse_model_json(
            "{\n \"{\n \"supported\":false,\"issues\":[\"unsupported claim\"]}"
        )
        .unwrap()["supported"],
        false
    );
    assert!(compose::parse_model_json("{\"supported\":true,\"{\"supported\":false}").is_err());
    assert_eq!(
        compose::parse_model_json("{\n {\"supported\":false,\"issues\":[\"unsupported claim\"]}")
            .unwrap()["supported"],
        false
    );
}
#[test]
fn cutoff_filter_rejects_same_day_future_and_invalid_timestamps() {
    use signal_and_sound::model::source_available_at;
    let cutoff = chrono::DateTime::parse_from_rfc3339("2026-10-25T06:00:00-05:00").unwrap();
    assert!(!source_available_at(Some("2026-10-25T12:00:00Z"), cutoff));
    assert!(source_available_at(Some("2026-10-25T11:00:00Z"), cutoff));
    assert!(source_available_at(
        Some("2026-10-25T05:59:59-05:00"),
        cutoff
    ));
    assert!(!source_available_at(Some("2026-10-25Tbad"), cutoff));
    assert!(!source_available_at(Some("2026-10-26"), cutoff));
    assert!(source_available_at(Some("2026-10-25"), cutoff));
    assert!(source_available_at(None, cutoff));
    // A source's local date may be tomorrow while its instant is before cutoff.
    let cutoff = chrono::DateTime::parse_from_rfc3339("2026-10-25T23:00:00-05:00").unwrap();
    assert!(source_available_at(Some("2026-10-26T03:00:00Z"), cutoff));
}
#[test]
fn native_schema_requires_citations_and_leaves_evidence_attachment_to_rust() {
    let schema = compose::structured_schema(
        serde_json::from_str(include_str!("../config/edition.schema.json")).unwrap(),
    );
    assert_eq!(schema["additionalProperties"], false);
    assert!(
        schema["$defs"]["Story"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("source_ids"))
    );
    assert_eq!(
        schema["$defs"]["Story"]["properties"]["evidence"]["additionalProperties"],
        false
    );
    assert_eq!(
        schema["$defs"]["Story"]["properties"]["evidence"]["properties"],
        json!({})
    );
    assert!(
        schema["$defs"]["Page"]["properties"]["stories"]
            .get("minItems")
            .is_none()
    );
}
#[test]
fn editorial_repairs_cannot_rewrite_unflagged_stories() {
    let draft = serde_json::to_value(edition()).unwrap();
    let mut story = draft["pages"][0]["stories"][0].clone();
    story["title"] = json!("Corrected supported title");
    let allowed = vec![story["id"].as_str().unwrap().to_string()];
    let patch = json!({"stories":[story],"concepts":[],"leads":[]});
    let result = compose::apply_editorial_patch(draft.clone(), &patch, &allowed).unwrap();
    assert_eq!(
        result["pages"][0]["stories"][0]["title"],
        "Corrected supported title"
    );
    assert_eq!(
        result["pages"][0]["stories"][1],
        draft["pages"][0]["stories"][1]
    );
    assert_eq!(result["pages"][1], draft["pages"][1]);
    assert_eq!(result["cutoff"], draft["cutoff"]);
    assert!(compose::apply_editorial_patch(draft.clone(), &patch, &[]).is_err());
    let bad_lead =
        json!({"stories":[],"concepts":[],"leads":[{"id":allowed[0],"lead":"A corrected note"}]});
    assert!(
        compose::apply_editorial_patch(draft.clone(), &bad_lead, &allowed)
            .unwrap_err()
            .to_string()
            .contains("Unknown patched page")
    );
    let good_lead =
        json!({"stories":[],"concepts":[],"leads":[{"id":"ai","lead":"A corrected note"}]});
    let result = compose::apply_editorial_patch(draft.clone(), &good_lead, &allowed).unwrap();
    assert_eq!(result["pages"][0]["lead"], "A corrected note");
    assert_eq!(result["pages"][0]["stories"], draft["pages"][0]["stories"]);
}
#[test]
fn invalid_citations_and_future_dates_fail() {
    let mut e = edition();
    e.pages[0].stories[0].source_ids = vec!["invented".into()];
    assert!(e.validate().is_err());
    let mut e = edition();
    e.sources[0].published = Some("2099-01-01".into());
    assert!(e.validate().is_err());
}
#[test]
fn six_stories_required() {
    let mut e = edition();
    e.pages[0].stories.pop();
    assert!(e.validate().is_err());
}
#[test]
fn first_and_normal_week() {
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-26T06:00:00-05:00").unwrap();
    let (s, e, partial) = interests::coverage(at, "weekly", "2026-10-21T00:00:00-05:00").unwrap();
    assert_eq!(s.to_rfc3339(), "2026-10-21T00:00:00-05:00");
    assert_eq!(e.to_rfc3339(), "2026-10-26T00:00:00-05:00");
    assert!(partial);
    assert!(
        interests::coverage(
            at - chrono::Duration::days(5),
            "weekly",
            "2026-10-21T00:00:00-05:00"
        )
        .is_err()
    );
    let (s, _, partial) = interests::coverage(
        at + chrono::Duration::days(7),
        "weekly",
        "2026-10-21T00:00:00-05:00",
    )
    .unwrap();
    assert_eq!(s.date_naive().to_string(), "2026-10-26");
    assert!(!partial);
}
#[test]
fn weekly_evidence_is_bounded_balanced_and_excludes_the_new_week() {
    let start = chrono::DateTime::parse_from_rfc3339("2026-10-26T00:00:00-05:00").unwrap();
    let end = start + chrono::Duration::days(7);
    let mut records = vec![];
    for page in ["ai", "music"] {
        for n in 0..70 {
            let mut s = edition().sources[0].clone();
            s.id = format!("{page}-{n}");
            s.url = format!("https://example.org/{page}/{n}");
            s.page = page.into();
            s.published = Some((end - chrono::Duration::hours(n * 6)).to_rfc3339());
            records.push(s);
        }
    }
    let selected = signal_and_sound::pipeline::weekly_sources(records, start, end, 50);
    assert_eq!(selected.len(), 50);
    for page in ["ai", "music"] {
        let sources = selected
            .iter()
            .filter(|s| s.page == page)
            .collect::<Vec<_>>();
        assert_eq!(sources.len(), 25);
        assert_eq!(
            sources
                .iter()
                .filter(
                    |s| chrono::DateTime::parse_from_rfc3339(s.published.as_ref().unwrap())
                        .unwrap()
                        < start
                )
                .count(),
            5
        );
        assert!(sources.iter().all(|s| {
            chrono::DateTime::parse_from_rfc3339(s.published.as_ref().unwrap()).unwrap() < end
        }));
    }
}
#[test]
fn exa_payload_has_bounded_windows() {
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-22T06:00:00-05:00").unwrap();
    let q = signal_and_sound::search::queries(&config("settings").unwrap(), at, false).unwrap();
    assert_eq!(q.len(), 4);
    assert_eq!(q[0].1["contents"], json!({"highlights":true}));
    assert_eq!(q[0].1["startPublishedDate"], "2026-10-21T11:00:00Z");
    assert_eq!(q[0].1["endPublishedDate"], "2026-10-22T11:00:00Z");
    assert!(q[0].1.get("category").is_none());
}
#[test]
fn interests_explicit_durable_work_bounded() {
    let p = json!({"version":"old","topics":[{"id":"voice","weight":0.5,"explicit":false},{"id":"music","weight":0.9,"explicit":true}]});
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-22T06:00:00-05:00").unwrap();
    let obs = vec![
        json!({"topic":"voice","source":"a","date":"2026-10-22","kind":"work","reason":"human request"}),
        json!({"topic":"voice","source":"b","date":"2026-10-22","kind":"work","reason":"human request"}),
        json!({"topic":"music","source":"c","date":"2026-10-22","kind":"work","reason":"human request"}),
    ];
    let (p, log) = interests::apply(&p, &obs, at).unwrap();
    assert_eq!(p["topics"][0]["weight"], 0.55);
    assert_eq!(p["topics"][1]["weight"], 0.9);
    assert_eq!(log.len(), 3);
    assert!(interests::apply(&p, &obs, at).unwrap().1.is_empty());
    let mut auto = obs[0].clone();
    auto["automated"] = json!(true);
    auto["source"] = json!("heartbeat");
    assert!(interests::apply(&p, &[auto], at).unwrap().1.is_empty());
}
#[test]
fn url_safety_and_relevance() {
    assert_eq!(
        collect::canonical("https://EXAMPLE.com/a/?utm_source=x&id=2#top"),
        "https://example.com/a?id=2"
    );
    for ip in [
        "127.0.0.1",
        "169.254.169.254",
        "10.0.0.1",
        "::1",
        "::ffff:127.0.0.1",
        "100.64.0.1",
    ] {
        assert!(!collect::public_ip(ip.parse().unwrap()));
    }
    assert!(collect::public_ip("8.8.8.8".parse().unwrap()));
    assert!(collect::x_score(0.0, 0.0, 1.0, 0.3, 1.0, &json!({"like_count":1000000})) < 65.0);
    assert!(collect::x_score(1.0, 1.0, 1.0, 0.8, 5.0, &json!({})) > 65.0);
}
#[tokio::test]
async fn budgets_replay_limits_and_leases() {
    let tmp = tempfile::tempdir().unwrap();
    let state = State::local(tmp.path().into()).unwrap();
    let mut settings = config("settings").unwrap();
    settings["search_monthly_limit"] = json!(1);
    let budget = Budget::new(state.clone(), settings);
    budget.reserve("one", "search", 0.02).await.unwrap();
    assert!(budget.reserve("one", "search", 0.02).await.is_err());
    assert!(budget.reserve("two", "search", 0.02).await.is_err());
    budget.settle("one", 0.017).await.unwrap();
    assert_eq!(
        state.read(&budget.key).await.unwrap()["requests"]["one"]["micros"],
        17000
    );
    assert!(state.claim("run", 900).await.unwrap());
    assert!(!state.claim("run", 900).await.unwrap());
    state.finish("run").await.unwrap();
    assert!(!state.claim("run", 900).await.unwrap());
}
#[tokio::test]
async fn private_storage_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Store::Local(tmp.path().into());
    assert!(s.put_json("../outside", &json!({})).await.is_err());
    s.put_json("nested/data.json", &json!({"ok":true}))
        .await
        .unwrap();
    assert_eq!(
        s.json("nested/data.json").await.unwrap().unwrap()["ok"],
        true
    );
}
#[tokio::test]
async fn exa_checkpoint_avoids_paid_repeat() {
    let tmp = tempfile::tempdir().unwrap();
    let state = State::local(tmp.path().join("state")).unwrap();
    let store = Store::Local(tmp.path().join("evidence"));
    let mut settings = config("settings").unwrap();
    settings["search_daily_queries"] = json!(1);
    let budget = Budget::new(state, settings.clone());
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-22T06:00:00-05:00").unwrap();
    let payload = signal_and_sound::search::queries(&settings, at, false)
        .unwrap()
        .remove(0)
        .1;
    budget
        .reserve("cached:exa:0", "search", 0.02)
        .await
        .unwrap();
    store.put_json("checkpoints/cached/exa-0.json",&json!({"request":payload,"response":{"results":[{"title":"New paper","url":"https://example.org/paper","publishedDate":"2026-10-22T10:00:00Z","highlights":["Evidence"]}],"costDollars":{"total":0.017}}})).await.unwrap();
    for _ in 0..2 {
        let (r, e, _) = signal_and_sound::search::discover(
            &settings,
            at,
            &budget,
            &store,
            "cached",
            false,
            Some("unused-test-key"),
        )
        .await
        .unwrap();
        assert_eq!(r.len(), 1);
        assert!(e.is_empty());
    }
    assert_eq!(
        budget.state.read(&budget.key).await.unwrap()["requests"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn normalize_grounds_original_evidence() {
    let e = edition();
    let mut raw = serde_json::to_value(&e).unwrap();
    raw["pages"][0]["stories"][0]["evidence"] = json!({});
    let e = compose::normalize(raw, &json!({}), &e.sources).unwrap();
    assert!(!e.pages[0].stories[0].evidence.is_empty());
}
#[test]
fn missing_concept_link_uses_a_cited_original_source() {
    let e = edition();
    let mut raw = serde_json::to_value(&e).unwrap();
    let concept = e.concepts[0].id.clone();
    let story = e
        .pages
        .iter()
        .flat_map(|p| &p.stories)
        .find(|s| s.concepts.contains(&concept))
        .unwrap();
    let expected = e
        .sources
        .iter()
        .find(|s| s.id == story.source_ids[0])
        .unwrap()
        .url
        .clone();
    raw["concepts"][0]["url"] = json!("");
    raw["subtitle"] = json!("Discarded legacy decoration");
    raw.as_object_mut().unwrap().remove("note");
    let normalized = compose::normalize(raw, &json!({}), &e.sources).unwrap();
    assert_eq!(
        normalized
            .concepts
            .iter()
            .find(|c| c.id == concept)
            .unwrap()
            .url,
        expected
    );
    assert_eq!(
        normalized.pages[0].stories[0].paragraphs,
        e.pages[0].stories[0].paragraphs
    );
}
#[test]
fn typst_pdf_web_and_immutable_archive() {
    let tmp = tempfile::tempdir().unwrap();
    let e = edition();
    let path = signal_and_sound::render::render(&e, tmp.path(), "https://example.org").unwrap();
    let bytes = std::fs::read(path.join("signal-and-sound.pdf")).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    assert_eq!(pdf.get_pages().len(), 2);
    assert_eq!(bytes, std::fs::read(path.join("print.pdf")).unwrap());
    let html = std::fs::read_to_string(path.join("ai.html")).unwrap();
    assert!(
        html.contains("download=\"signal-and-sound-")
            && html.contains("data-concept=")
            && html.contains("Exa Search API")
    );
    assert!(!html.contains("INDEPENDENT CURIOSITY"));
    let before = std::fs::read(tmp.path().join("latest.json")).unwrap();
    let mut changed = e.clone();
    changed.pages[0].stories[0].title = "Changed".into();
    assert!(signal_and_sound::render::render(&changed, tmp.path(), "https://example.org").is_err());
    assert_eq!(
        before,
        std::fs::read(tmp.path().join("latest.json")).unwrap()
    );
}
#[test]
fn overflow_retains_good_pointer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut e = edition();
    signal_and_sound::render::render(&e, tmp.path(), "https://example.org").unwrap();
    let before = std::fs::read(tmp.path().join("latest.json")).unwrap();
    e.revision += 1;
    e.pages[0].stories[0].title = "Overflow ".repeat(1500);
    assert!(signal_and_sound::render::render(&e, tmp.path(), "https://example.org").is_err());
    assert_eq!(
        before,
        std::fs::read(tmp.path().join("latest.json")).unwrap()
    );
}
#[test]
fn source_and_html_text_are_untrusted() {
    let mut e = edition();
    e.pages[0].stories[0].title = "<script>alert(1)</script> #read(\"/etc/passwd\")".into();
    let tmp = tempfile::tempdir().unwrap();
    let path = signal_and_sound::render::render(&e, tmp.path(), "https://example.org").unwrap();
    let html = std::fs::read_to_string(path.join("ai.html")).unwrap();
    assert!(!html.contains("<script>alert(1)</script>"));
    assert!(html.contains("&lt;script&gt;"));
}

#[tokio::test]
async fn publisher_recovers_partial_upload_without_replacing_old_pdf() {
    use signal_and_sound::{aws::Aws, pipeline::Runtime};
    use std::sync::Arc;
    let tmp = tempfile::tempdir().unwrap();
    let e = edition();
    let evidence = Store::Local(tmp.path().join("evidence"));
    let public = Store::Local(tmp.path().join("public"));
    let key = format!("editions/{}.json", e.id);
    evidence
        .put_json(&key, &serde_json::to_value(&e).unwrap())
        .await
        .unwrap();
    let release = format!("editions/{}/r{}", e.id, e.revision);
    public
        .put_json(&format!("{release}/edition.json"), &e.public().unwrap())
        .await
        .unwrap();
    public
        .put(
            &format!("{release}/signal-and-sound.pdf"),
            b"existing-immutable-pdf".to_vec(),
            "application/pdf",
            true,
            None,
        )
        .await
        .unwrap();
    let runtime = Runtime {
        aws: Arc::new(Aws::new().await.unwrap()),
        settings: config("settings").unwrap(),
        state: State::local(tmp.path().join("state")).unwrap(),
        evidence,
        public: public.clone(),
    };
    runtime.publisher(json!({"edition_key":key})).await.unwrap();
    assert_eq!(
        public
            .get(&format!("{release}/signal-and-sound.pdf"))
            .await
            .unwrap()
            .unwrap(),
        b"existing-immutable-pdf"
    );
    assert!(
        public
            .get(&format!("{release}/music.html"))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        public.json("latest.json").await.unwrap().unwrap()["daily"]["id"],
        e.id
    );
}

#[test]
fn legacy_observation_ids_are_not_reapplied() {
    let o = json!({"date":"2026-10-21","kind":"work","reason":"human request","source":"a","topic":"voice"});
    let expected = r#"{"date": "2026-10-21", "kind": "work", "reason": "human request", "source": "a", "topic": "voice"}"#;
    assert_eq!(interests::legacy_json(&o), expected);
    let p = json!({"version":"old","applied_evidence":[signal_and_sound::hash(expected.as_bytes())],"topics":[{"id":"voice","weight":0.55,"explicit":false}]});
    let at = chrono::DateTime::parse_from_rfc3339("2026-11-02T06:00:00-05:00").unwrap();
    let (updated, log) = interests::apply(&p, &[o], at).unwrap();
    assert!(log.is_empty());
    assert_eq!(updated["topics"][0]["weight"], 0.55);
}

#[tokio::test]
async fn failed_analysis_releases_only_its_own_lease() {
    use signal_and_sound::{aws::Aws, pipeline::Runtime};
    use std::sync::Arc;
    let tmp = tempfile::tempdir().unwrap();
    let state = State::local(tmp.path().join("state")).unwrap();
    let evidence = Store::Local(tmp.path().join("evidence"));
    let at = edition().cutoff;
    let id = format!("daily-{}", at.date_naive());
    evidence
        .put_json(
            &format!("checkpoints/{id}/collection.json"),
            &json!({"records":[],"cutoff":at,"coverage_notes":[]}),
        )
        .await
        .unwrap();
    let runtime = Runtime {
        aws: Arc::new(Aws::new().await.unwrap()),
        settings: config("settings").unwrap(),
        state: state.clone(),
        evidence,
        public: Store::Local(tmp.path().join("public")),
    };
    let error = runtime.analyze(at, "daily", None, None).await.unwrap_err();
    assert!(error.to_string().contains("Insufficient evidence"));
    let lease = format!("analysis:{id}");
    assert!(state.claim(&lease, 900).await.unwrap());
    assert!(
        runtime
            .analyze(at, "daily", None, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("Run active")
    );
    assert!(
        !state.claim(&lease, 900).await.unwrap(),
        "A rejected concurrent run must not release the owner's lease"
    );
}
