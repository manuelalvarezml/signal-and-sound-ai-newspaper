"""Explicit preferences are durable; work-derived beliefs are bounded and reversible."""
import hashlib
import json
from datetime import datetime, timedelta
from pathlib import Path
from zoneinfo import ZoneInfo

ZONE = ZoneInfo("America/Lima")


def week_start(at):
    local = at.astimezone(ZONE)
    return (local - timedelta(days=local.weekday())).date().isoformat()


def coverage(at, kind, started_at):
    local = at.astimezone(ZONE)
    # Weekly always means completed Monday-Sunday, independent of a delayed run.
    monday = datetime.fromisoformat(week_start(at)).replace(tzinfo=ZONE)
    if kind == "weekly":
        end = monday
        start = max(datetime.fromisoformat(started_at), monday - timedelta(days=7))
        if start >= end:
            raise ValueError("No completed publication week exists yet")
        return start, end, start > monday - timedelta(days=7)
    start = max(datetime.fromisoformat(started_at), local - timedelta(days=1))
    return start, local, False


def apply_observations(profile, observations, at):
    updated = json.loads(json.dumps(profile))
    known = set(updated.setdefault("applied_evidence", []))
    log = []
    for o in observations:
        required = {"topic", "source", "date", "kind", "reason"}
        if not required <= o.keys() or o["kind"] not in ("explicit", "work"):
            raise ValueError("Observation requires provenance and explicit/work classification")
        if o.get("automated", False):
            continue  # heartbeat frequency is not evidence of a human interest
        identity = hashlib.sha256(json.dumps(o, sort_keys=True).encode()).hexdigest()
        if identity in known:
            continue
        topic = next((t for t in updated["topics"] if t["id"] == o["topic"]), None)
        if not topic:
            raise ValueError("New topics require an explicit curated topic definition")
        before = topic["weight"]
        if o["kind"] == "explicit":
            if "weight" not in o or not 0 <= o["weight"] <= 1:
                raise ValueError("Explicit updates require a weight between zero and one")
            topic.update(weight=o["weight"], explicit=True, confidence=1.0)
        elif not topic.get("explicit"):
            week = week_start(at)
            if topic.get("adjusted_week") != week:
                topic["weight"] = min(1.0, round(topic["weight"] + .05, 3))
                topic["confidence"] = min(.9, round(topic.get("confidence", .5) + .05, 3))
                topic["adjusted_week"] = week
        topic["last_evidence"] = o["date"]
        known.add(identity)
        log.append({**o, "before": before, "after": topic["weight"], "evidence_id": identity})
    updated["applied_evidence"] = sorted(known)
    updated["last_reviewed"] = at.isoformat()
    updated["version"] = at.strftime("%Y-%m-%d") + "-" + hashlib.sha256(json.dumps(updated["topics"], sort_keys=True).encode()).hexdigest()[:8]
    return updated, log


def public_interest_config(profile):
    return {"version": profile["version"], "last_reviewed": profile["last_reviewed"],
            "topics": [{k: t[k] for k in ("id", "label", "keywords", "weight", "page")} for t in profile["topics"]],
            "reading_level": "practitioner; explain mechanisms briefly; do not assume mastery"}
