import hashlib
import ipaddress
import json
import math
import os
import re
import socket
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone, timedelta
from urllib.parse import urlsplit, urlunsplit, parse_qsl, urlencode, urljoin
from urllib.robotparser import RobotFileParser
import feedparser
import httpx
import trafilatura
from lxml import html
from .models import Source

UA = "SignalAndSound/0.1 (personal research newspaper; bounded daily collection)"


def canonical(url):
    u = urlsplit(url)
    query = urlencode([(k, v) for k, v in parse_qsl(u.query) if not k.startswith("utm_") and k not in ("ref", "fbclid")])
    return urlunsplit((u.scheme.lower(), u.netloc.lower(), u.path.rstrip("/") or "/", query, ""))


def source_id(url):
    return hashlib.sha256(canonical(url).encode()).hexdigest()[:16]


def assert_public(url):
    u = urlsplit(url)
    if u.scheme not in ("http", "https") or not u.hostname or u.username or u.password or u.port not in (None, 80, 443):
        raise ValueError("Unsafe URL")
    for item in socket.getaddrinfo(u.hostname, u.port or 443, type=socket.SOCK_STREAM):
        if not ipaddress.ip_address(item[4][0]).is_global:
            raise ValueError("Non-public destination blocked")


class Fetcher:
    def __init__(self, deadline=None):
        self.robots = {}
        self.deadline = deadline
        self.client = httpx.Client(timeout=httpx.Timeout(15, connect=5), headers={"User-Agent": UA}, follow_redirects=False)

    def get(self, url, headers=None, robots=True):
        for _ in range(5):
            if self.deadline and time.monotonic() >= self.deadline:
                raise TimeoutError("Collection time budget exhausted")
            assert_public(url)
            u = urlsplit(url)
            origin = f"{u.scheme}://{u.netloc}"
            if robots:
                if origin not in self.robots:
                    try:
                        response = self.get(origin + "/robots.txt", robots=False)
                        parser = RobotFileParser()
                        parser.parse(response.splitlines())
                        self.robots[origin] = parser
                    except httpx.HTTPStatusError as e:
                        if e.response.status_code not in (404, 410):
                            raise
                        self.robots[origin] = None
                rule = self.robots[origin]
                if rule and not rule.can_fetch(UA, url):
                    raise ValueError("Source disallows collection")
            with self.client.stream("GET", url, headers=headers) as response:
                if response.is_redirect:
                    target = urljoin(url, response.headers["location"])
                    # Never forward API credentials across hosts.
                    if urlsplit(target).netloc != u.netloc:
                        headers = None
                    url = target
                    continue
                response.raise_for_status()
                parts, size = [], 0
                for part in response.iter_bytes():
                    size += len(part)
                    if size > 2_000_000:
                        raise ValueError("Source exceeds download limit")
                    parts.append(part)
                return b"".join(parts).decode(response.encoding or "utf-8", errors="replace")
        raise ValueError("Too many redirects")


def relevance(text, topics, page):
    text = text.lower()
    matches = [t["weight"] for t in topics if t["page"] in (page, "both") and any(k.lower() in text for k in t["keywords"])]
    return min(1.0, max(matches, default=.15) + .05 * max(0, len(matches) - 1))


def x_score(relevance_score, evidence, novelty, credibility, age_hours, metrics):
    weighted = metrics.get("like_count", 0) + 2 * metrics.get("retweet_count", 0) + 2 * metrics.get("quote_count", 0) + metrics.get("reply_count", 0)
    engagement = min(1, math.log1p(weighted / max(age_hours, 1)) / math.log1p(50))
    freshness = 2 ** (-max(age_hours, 0) / 48)
    return round(100 * (.35 * relevance_score + .25 * evidence + .15 * novelty + .10 * credibility + .10 * freshness + .05 * engagement), 2)


def parse_source(entry, fetcher, cutoff):
    text = fetcher.get(entry["url"], robots=entry["kind"] not in ("hn",))
    now = cutoff.isoformat()
    records = []
    if entry["kind"] == "rss":
        feed = feedparser.parse(text)
        for item in feed.entries[:30]:
            url = item.get("link")
            if not url:
                continue
            stamp = item.get("published_parsed") or item.get("updated_parsed")
            published = datetime(*stamp[:6], tzinfo=timezone.utc).isoformat() if stamp else None
            records.append(Source(id=source_id(url), title=item.get("title", "Untitled"), publisher=entry["name"], url=url,
                                  published=published, retrieved_at=now, excerpt=re.sub("<[^>]+>", " ", item.get("summary", ""))[:3000],
                                  page=entry["page"], authority=entry["authority"]))
    elif entry["kind"] == "html":
        tree = html.fromstring(text)
        seen = set()
        for a in tree.xpath("//a[@href]"):
            url = urljoin(entry["url"], a.get("href"))
            title = " ".join(a.text_content().split())
            if entry.get("path_filter", "/") not in urlsplit(url).path or len(title) < 24 or url in seen or urlsplit(url).hostname != urlsplit(entry["url"]).hostname:
                continue
            seen.add(url)
            records.append(Source(id=source_id(url), title=title[:240], publisher=entry["name"], url=url,
                                  retrieved_at=now, page=entry["page"], authority=entry["authority"]))
            if len(records) >= 20:
                break
    elif entry["kind"] == "hn":
        for item_id in json.loads(text)[:20]:
            item = json.loads(fetcher.get(f"https://hacker-news.firebaseio.com/v0/item/{item_id}.json", robots=False))
            if item and item.get("url"):
                records.append(Source(id=source_id(item["url"]), title=item["title"], publisher="Hacker News discovery", url=item["url"],
                                      published=datetime.fromtimestamp(item["time"], timezone.utc).isoformat(), retrieved_at=now,
                                      excerpt=f"Discussion: https://news.ycombinator.com/item?id={item_id}", authority=.5))
    return [r for r in records if not r.published or r.published[:10] <= cutoff.date().isoformat()]


def collect(registry, settings, profile, cutoff, budget, store, run_id, weekly=False):
    checkpoint = store.json(f"checkpoints/{run_id}/collection.json")
    if checkpoint:
        return checkpoint
    sources, errors = {}, []
    deadline = time.monotonic() + settings.get("collection_seconds", 180)
    known_urls = set()
    for key in sorted(store.keys("history/"))[-35:]:
        previous = store.json(key) or {}
        known_urls.update(canonical(r["url"]) for r in previous.get("records", []))
    primary_hosts = {urlsplit(s["url"]).hostname for s in registry if s["authority"] >= .8}
    def fetch_entry(entry):
        return parse_source(entry, Fetcher(deadline), cutoff)
    selected = [s for s in registry if weekly or not s.get("weekly_only")][:settings["max_sources"]]
    with ThreadPoolExecutor(max_workers=6) as pool:
        jobs = {pool.submit(fetch_entry, s): s for s in selected}
        for job in as_completed(jobs):
            entry = jobs[job]
            try:
                for record in job.result():
                    sources[record.id] = record
            except Exception as exc:
                errors.append(f"{entry['name']}: {type(exc).__name__}")
    fetcher = Fetcher(deadline)
    queries = [("ai", "AI research models agents releases"), ("ai", "LLM evaluation voice agents memory infrastructure"),
               ("music", "music AI generation recommendation research releases"), ("music", "music information retrieval DJ controllable music generation")]
    if weekly:
        queries += [("ai", "AI weekly roundup research"), ("music", "music AI weekly news research")]
    brave = os.environ.get("BRAVE_API_KEY")
    if brave:
        for n, (page, query) in enumerate(queries):
            key = f"{run_id}:search:{n}"
            cachekey = f"checkpoints/{run_id}/search-{n}.json"
            result = store.json(cachekey)
            try:
                if result is None:
                    budget.reserve(key, "search", 0)  # only an account with free credits/no reload
                    url = "https://api.search.brave.com/res/v1/web/search?" + urlencode({"q": query, "freshness": "pw" if weekly else "pd", "count": 10})
                    result = json.loads(fetcher.get(url, headers={"X-Subscription-Token": brave}, robots=False))
                    store.json(cachekey, result)
                    budget.settle(key, 0)
                for item in result.get("web", {}).get("results", []):
                    r = Source(id=source_id(item["url"]), title=item["title"], url=item["url"], publisher=urlsplit(item["url"]).hostname,
                               retrieved_at=cutoff.isoformat(), excerpt=item.get("description", ""), page=page, authority=.6)
                    sources.setdefault(r.id, r)
            except Exception as exc:
                errors.append(f"Web search: {type(exc).__name__}")
    else:
        errors.append("Brave Search not connected; feeds and direct sources only")
    token = os.environ.get("X_BEARER_TOKEN")
    if token and not weekly:
        try:
            key, cachekey = f"{run_id}:x", f"checkpoints/{run_id}/x.json"
            data = store.json(cachekey)
            if data is None:
                budget.reserve(key, "x", settings["x_daily_posts"] * .005)
                query = '("AI research" OR "music AI" OR "music generation" OR "LLM evaluation") has:links -is:retweet -is:reply lang:en'
                url = "https://api.x.com/2/tweets/search/recent?" + urlencode({"query": query, "max_results": 10,
                    "start_time": (cutoff - timedelta(hours=48)).astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
                    "end_time": cutoff.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
                    "sort_order": "relevancy", "tweet.fields": "created_at,public_metrics,entities,author_id"})
                data = json.loads(fetcher.get(url, {"Authorization": "Bearer " + token}, robots=False))
                store.json(cachekey, data)
                budget.settle(key, len(data.get("data", [])) * .005)
            for post in data.get("data", []):
                text = post["text"]
                page = "music" if any(t in text.lower() for t in ("music", "audio", "song", "mir ")) else "ai"
                age = max(0, (cutoff - datetime.fromisoformat(post["created_at"].replace("Z", "+00:00"))).total_seconds() / 3600)
                links = post.get("entities", {}).get("urls", [])
                external = [l.get("expanded_url", "") for l in links if urlsplit(l.get("expanded_url", "")).hostname not in ("x.com", "twitter.com", "t.co", None)]
                evidence = 1.0 if any(urlsplit(u).hostname in primary_hosts for u in external) else .5 if external else 0
                novelty = 1.0 if any(canonical(u) not in known_urls for u in external) else 0
                credibility = .9 if post.get("author_id") in settings.get("trusted_x_author_ids", []) else .3
                score = x_score(relevance(text, profile["topics"], page), evidence, novelty, credibility, age, post.get("public_metrics", {}))
                if score >= 65:
                    for link in post.get("entities", {}).get("urls", []):
                        url = link.get("expanded_url", "")
                        if urlsplit(url).hostname in ("x.com", "twitter.com", "t.co"):
                            continue
                        r = Source(id=source_id(url), title=text[:200], url=url, publisher="X discovery; verify linked source",
                                   retrieved_at=cutoff.isoformat(), page=page, authority=.5,
                                   excerpt=f"Discovery score {score}; https://x.com/i/status/{post['id']}")
                        sources.setdefault(r.id, r)
        except Exception as exc:
            errors.append(f"X: {type(exc).__name__}")
    elif not token:
        errors.append("X not connected; no direct X posts were collected")
    ranked = sorted(sources.values(), key=lambda r: (relevance(r.title + r.excerpt, profile["topics"], r.page), r.published or ""), reverse=True)[:settings["max_candidates"]]
    # Reserve half the extraction capacity for each editorial page.
    selected = [s for page in ("ai", "music") for s in [r for r in ranked if r.page == page][:settings["max_articles"] // 2]]
    def extract(r):
        raw = Fetcher(deadline).get(r.url)
        data = trafilatura.bare_extraction(raw, url=r.url, with_metadata=True)
        if data and data.text:
            r.excerpt = data.text[:9000]
            r.title = data.title or r.title
            r.published = data.date or r.published
            if data.sitename:
                r.publisher = data.sitename
        return r
    enriched = []
    with ThreadPoolExecutor(max_workers=6) as pool:
        jobs = {pool.submit(extract, s): s for s in selected}
        for job in as_completed(jobs):
            try:
                r = job.result()
                if r.excerpt and (not r.published or r.published[:10] <= cutoff.date().isoformat()):
                    enriched.append(r.model_dump())
            except Exception as exc:
                errors.append(f"Article {jobs[job].id}: {type(exc).__name__}")
    result = {"run_id": run_id, "cutoff": cutoff.isoformat(), "records": enriched, "coverage_notes": errors, "metadata_candidates": len(ranked)}
    store.json(f"checkpoints/{run_id}/collection.json", result)
    store.json(f"history/{cutoff.date()}/{run_id}.json", result)
    return result
