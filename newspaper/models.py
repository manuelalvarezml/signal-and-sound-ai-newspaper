from __future__ import annotations
import re
from datetime import datetime
from typing import Literal
from urllib.parse import urlsplit
from pydantic import BaseModel, ConfigDict, Field, model_validator, field_validator


class Strict(BaseModel):
    model_config = ConfigDict(extra="forbid")


class Source(Strict):
    id: str
    title: str
    publisher: str
    url: str
    published: str | None = None
    retrieved_at: str
    excerpt: str = ""
    page: Literal["ai", "music"] = "ai"
    authority: float = Field(default=0.8, ge=0, le=1)
    event_id: str = ""

    @field_validator("url")
    @classmethod
    def safe_url(cls, value):
        u = urlsplit(value)
        if u.scheme not in ("https", "http") or not u.hostname or u.username or u.password:
            raise ValueError("Only absolute public HTTP(S) source URLs are allowed")
        return value


class Concept(Strict):
    id: str = Field(pattern=r"^[a-z0-9-]+$")
    term: str
    definition: str
    why_it_matters: str = ""
    url: str

    _safe_url = field_validator("url")(Source.safe_url.__func__)


class Story(Strict):
    id: str = Field(pattern=r"^[a-z0-9-]+$")
    title: str
    kicker: str
    lane: Literal["broad", "personal"]
    status: Literal["New", "Recent", "Revisited", "Learning", "Early signal", "Trend"]
    paragraphs: list[str] = Field(min_length=1, max_length=3)
    implication: str
    source_ids: list[str] = Field(min_length=1)
    concepts: list[str] = []
    evidence: dict[str, str] = Field(default_factory=dict)

    @property
    def words(self):
        return len(re.findall(r"\S+", " ".join(self.paragraphs + [self.implication])))


class Page(Strict):
    id: Literal["ai", "music"]
    title: str
    subtitle: str
    lead: str
    stories: list[Story] = Field(min_length=2, max_length=6)
    takeaway: str


class Edition(Strict):
    schema_version: int = 1
    id: str = Field(pattern=r"^(daily|weekly)-\d{4}-\d{2}-\d{2}$")
    revision: int = Field(default=1, ge=1)
    kind: Literal["daily", "weekly"]
    date: str
    cutoff: datetime
    coverage_start: datetime
    week_start: str
    inaugural: bool = False
    partial_week: bool = False
    title: str
    note: str
    coverage_notes: list[str]
    profile_version: str
    editor: str = "AI-assisted, source-checked"
    pages: list[Page]
    sources: list[Source]
    concepts: list[Concept]

    @model_validator(mode="after")
    def validate_edition(self):
        if [p.id for p in self.pages] != ["ai", "music"]:
            raise ValueError("Exactly two pages, AI then music, are required")
        if self.cutoff.tzinfo is None or self.coverage_start.tzinfo is None:
            raise ValueError("Coverage timestamps require a time zone")
        if self.coverage_start > self.cutoff:
            raise ValueError("Coverage begins after cutoff")
        if self.id != f"{self.kind}-{self.date}":
            raise ValueError("Edition identity must match its kind and date")
        ids = {s.id for s in self.sources}
        if len(ids) != len(self.sources):
            raise ValueError("Duplicate source IDs")
        concepts = {c.id for c in self.concepts}
        seen = set()
        for page in self.pages:
            broad = sum(s.words for s in page.stories if s.lane == "broad")
            total = sum(s.words for s in page.stories)
            if not .55 <= broad / max(1, total) <= .65:
                raise ValueError(f"{page.id}: 60/40 word allocation violated ({broad}/{total})")
            if total > 680:
                raise ValueError(f"{page.id}: print word budget exceeded")
            for story in page.stories:
                if story.id in seen:
                    raise ValueError("Duplicate story IDs")
                seen.add(story.id)
                if not set(story.source_ids) <= ids or not set(story.concepts) <= concepts:
                    raise ValueError("Unknown citation or concept")
                for sid in story.source_ids:
                    if not story.evidence.get(sid):
                        raise ValueError(f"Missing claim evidence for {story.id}/{sid}")
                if story.status == "Trend" and len({s.url for s in self.sources if s.id in story.source_ids and s.published}) < 2:
                    raise ValueError("A trend requires two distinct dated sources")
                if story.status == "New":
                    for source in self.sources:
                        if source.id in story.source_ids and source.published:
                            if source.published[:10] < self.coverage_start.date().isoformat():
                                raise ValueError("Old source mislabeled as new")
        for source in self.sources:
            if source.published and source.published[:10] > self.cutoff.date().isoformat():
                raise ValueError("Future source")
            if source.published and "T" in source.published:
                stamp = datetime.fromisoformat(source.published.replace("Z", "+00:00"))
                if stamp.tzinfo and stamp > self.cutoff:
                    raise ValueError("Source published after the analysis cutoff")
        return self
