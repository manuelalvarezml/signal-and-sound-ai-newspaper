import json
import re
import shutil
import hashlib
import tempfile
import math
import unicodedata
from pathlib import Path
from html import escape
from jinja2 import Environment, FileSystemLoader, select_autoescape
from reportlab.pdfgen import canvas
from reportlab.lib.pagesizes import A4
from reportlab.lib.colors import HexColor
from reportlab.lib.styles import ParagraphStyle
from reportlab.platypus import Paragraph
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from pypdf import PdfReader
from .models import Edition

ROOT = Path(__file__).resolve().parent.parent


def public_edition(edition):
    data = edition.model_dump(mode="json")
    # Evidence excerpts and private drafting annotations stay in the private store.
    for source in data["sources"]:
        source.pop("excerpt", None)
    for page in data["pages"]:
        for story in page["stories"]:
            story.pop("evidence", None)
    return data


def typography(text):
    text = unicodedata.normalize("NFKC", text)
    text = re.sub(r"[\u2010-\u2015\u2212]", "-", text)
    return text.replace("—", "-").replace("–", "-").replace("’", "'").replace("“", '"').replace("”", '"')


def reading_minutes(edition):
    words = sum(story.words for page in edition.pages for story in page.stories)
    words += sum(len(page.lead.split()) for page in edition.pages)
    words += sum(len((c.definition + " " + c.why_it_matters).split()) for c in edition.concepts)
    return max(1, math.ceil(words / 180))


def render_pdf(edition, path, site_url):
    import reportlab
    fonts = Path(reportlab.__file__).parent / "fonts"
    for name, filename in [("Vera", "Vera.ttf"), ("VeraBold", "VeraBd.ttf"), ("VeraItalic", "VeraIt.ttf")]:
        pdfmetrics.registerFont(TTFont(name, str(fonts / filename)))
    pdfmetrics.registerFontFamily("Vera", normal="Vera", bold="VeraBold", italic="VeraItalic")
    width, height = A4
    c = canvas.Canvas(str(path), pagesize=A4)
    c.setTitle(f"Signal & Sound | {edition.date} | {edition.kind}")
    c.setAuthor("Signal & Sound")
    ink, accent = HexColor("#232821"), HexColor("#ad482f")
    source_map = {s.id: s for s in edition.sources}
    concept_map = {x.id: x for x in edition.concepts}
    def para(text, x, y, w, size=10.5, leading=13, font="Vera", color=ink):
        p = Paragraph(text, ParagraphStyle("p", fontName=font, fontSize=size, leading=leading, textColor=color, spaceAfter=0))
        _, h = p.wrap(w, height)
        p.drawOn(c, x, y - h)
        return y - h
    for number, page in enumerate(edition.pages, 1):
        margin, gap = 34, 20
        col = (width - margin * 2 - gap) / 2
        c.setFillColor(ink)
        c.setFont("Times-Bold", 30)
        c.drawString(margin, height - 47, "Signal & Sound")
        label = "General AI" if page.id == "ai" else "AI for Music"
        c.setFont("VeraBold", 9)
        c.drawRightString(width - margin - 90, height - 33, label)
        c.setFont("Vera", 8)
        c.drawRightString(width - margin, height - 33, edition.date)
        total_minutes = reading_minutes(edition)
        c.drawRightString(width - margin, height - 48, f"{edition.kind.upper()} / {number:02d}  |  {total_minutes} MIN READ - BOTH PAGES")
        c.setStrokeColor(ink)
        c.setLineWidth(1.5)
        c.line(margin, height - 60, width - margin, height - 60)
        y = para("<b>EDITOR'S NOTE</b>  " + escape(typography(page.lead)), margin, height - 72,
                 width - 2 * margin, 9, 12) - 15
        broad = [s for s in page.stories if s.lane == "broad"]
        personal = [s for s in page.stories if s.lane == "personal"]
        # Three briefs per column. The fourth broad brief shares the right column
        # with the two personalized stories; each brief retains its lane label.
        columns = [broad[:3], broad[3:] + personal]
        bottoms = []
        for stories, x in zip(columns, [margin, margin + col + gap]):
            cy = y
            for story in stories:
                date = source_map[story.source_ids[0]].published or "Undated"
                lane = "BROAD VIEW" if story.lane == "broad" else "YOUR INTERESTS"
                cy = para(escape(f"{lane} / {story.status.upper()} / {date[:10]}"), x, cy, col, 6.8, 9, "Vera", accent)
                cy = para(escape(typography(story.title)), x, cy - 4, col, 13, 15, "Times-Bold") - 4
                for text in story.paragraphs:
                    cy = para(escape(typography(text)), x, cy, col) - 3
                cy = para("<b>Why it matters.</b> " + escape(typography(story.implication)), x, cy, col, 9, 11)
                links = " / ".join(f'<link href="{escape(source_map[sid].url, quote=True)}" color="#ad482f">{escape(source_map[sid].publisher)}</link>' for sid in story.source_ids)
                cy = para(links, x, cy - 4, col, 7.5, 9) - 8
            bottoms.append(cy)
        concepts_y = min(bottoms) - 3
        c.setStrokeColor(HexColor("#babeb3"))
        c.setLineWidth(.5)
        c.line(margin, concepts_y + 4, width - margin, concepts_y + 4)
        concepts_y = para("CONCEPTS TO KEEP", margin, concepts_y - 5, width - 2 * margin, 7.6, 10, "VeraBold", accent) - 7
        terms = list(dict.fromkeys(cid for story in page.stories for cid in story.concepts))[:2]
        for cid, x in zip(terms, [margin, margin + col + gap]):
            concept = concept_map[cid]
            cy = para(f"<b>{escape(concept.term)}.</b> {escape(typography(concept.definition))}", x, concepts_y, col, 9, 11) - 4
            if concept.why_it_matters:
                cy = para("<b>Why it matters.</b> " + escape(typography(concept.why_it_matters)), x, cy, col, 9, 11)
            bottoms.append(cy)
        if min(bottoms) < 63:
            raise ValueError(f"PDF overflow on {page.id}: {min(bottoms):.1f}; shorten copy, never shrink text")
        c.setStrokeColor(HexColor("#babeb3"))
        c.setLineWidth(.5)
        c.line(margin, 49, width - margin, 49)
        c.setFillColor(ink)
        c.setFont("Vera", 7)
        cutoff = edition.cutoff.astimezone(__import__('zoneinfo').ZoneInfo("America/Lima")).strftime("%H:%M")
        c.drawString(margin, 35, f"Sources checked through {cutoff} Lima | Older items labeled Revisited | {edition.id}")
        c.drawString(margin, 24, site_url + "/archive.html  |  AI-assisted edition. Source links are clickable.")
        c.drawRightString(width - margin, 24, f"{number} / 2")
        c.showPage()
    c.save()
    reader = PdfReader(path)
    if len(reader.pages) != 2:
        raise ValueError("PDF must contain exactly two pages")
    for p, page in zip(reader.pages, edition.pages):
        text = p.extract_text()
        for story in page.stories:
            if typography(story.title) not in text.replace("\n", " "):
                raise ValueError("PDF story missing")


def render(edition: Edition, dist: Path, site_url="http://localhost:8787"):
    dist.mkdir(parents=True, exist_ok=True)
    release = Path("editions") / edition.id / f"r{edition.revision}"
    final_target = dist / release
    public_json = json.dumps(public_edition(edition), ensure_ascii=False, indent=2)
    existing = final_target / "edition.json"
    if existing.exists() and json.loads(existing.read_text()) != json.loads(public_json):
        raise ValueError("Published revisions are immutable; increment revision")
    staging = tempfile.TemporaryDirectory(dir=dist, prefix=".build-")
    target = Path(staging.name)
    assets = ROOT / "web" / "assets"
    asset_hash = hashlib.sha256(b"".join(p.read_bytes() for p in sorted(assets.iterdir()) if p.is_file())).hexdigest()[:12]
    asset_base = "/assets/" + asset_hash
    shutil.copytree(assets, dist / "assets" / asset_hash, dirs_exist_ok=True)
    env = Environment(loader=FileSystemLoader(ROOT / "web"), autoescape=select_autoescape())
    env.filters["pretty_date"] = lambda v: str(v)[:10]
    sources = {s.id: s for s in edition.sources}
    concepts = {c.id: c for c in edition.concepts}
    pdf_name = "signal-and-sound.pdf"
    render_pdf(edition, target / pdf_name, site_url)
    shutil.copyfile(target / pdf_name, target / "print.pdf")
    links = {p.id: "/" + str(release / (p.id + ".html")) for p in edition.pages}
    for i, page in enumerate(edition.pages):
        content = env.get_template("edition.html").render(edition=edition, page=page, page_number=i + 1,
            asset_base=asset_base, print_pdf="/" + str(release / "print.pdf"),
            sources=sources, concepts=concepts, links=links, pdf="/" + str(release / pdf_name),
            cutoff=edition.cutoff.astimezone(__import__('zoneinfo').ZoneInfo("America/Lima")).strftime("%I:%M %p").lstrip("0"),
            words=reading_minutes(edition) * 180,
            date_label=edition.cutoff.astimezone(__import__('zoneinfo').ZoneInfo("America/Lima")).strftime("%A, %d %B %Y"))
        (target / f"{page.id}.html").write_text(content)
    (target / "edition.json").write_text(public_json)
    final_target.parent.mkdir(parents=True, exist_ok=True)
    if not existing.exists():
        if final_target.exists():
            raise ValueError("Incomplete release exists; use a new revision")
        target.rename(final_target)
    staging.cleanup()
    target = final_target
    # All artifacts exist and passed validation before the one mutable pointer changes.
    pointer_path = dist / "latest.json"
    pointer = json.loads(pointer_path.read_text()) if pointer_path.exists() else {}
    old = pointer.get(edition.kind)
    new = {"id": edition.id, "date": edition.date, "revision": edition.revision, "cutoff": edition.cutoff.isoformat(), "pages": links, "pdf": "/" + str(release / pdf_name)}
    if not old or (edition.date, edition.revision) >= (old['date'], old['revision']):
        pointer[edition.kind] = new
    tmp = dist / "latest.tmp"
    tmp.write_text(json.dumps(pointer, indent=2))
    tmp.replace(pointer_path)
    for route, section in [("index.html", "ai"), ("ai.html", "ai"), ("music.html", "music"), ("weekly.html", "ai")]:
        (dist / route).write_text(env.get_template("latest.html").render(asset_base=asset_base, section=section, kind="weekly" if route == "weekly.html" else "daily"))
    entries = []
    for p in dist.glob("editions/*/r*/edition.json"):
        data = json.loads(p.read_text())
        entries.append({"id": data["id"], "date": data["date"], "kind": data["kind"], "revision": data["revision"], "url": "/" + str(p.parent.relative_to(dist) / "ai.html"), "pdf": "/" + str(p.parent.relative_to(dist) / pdf_name)})
    entries.sort(key=lambda e: (e["date"], e["kind"], e["revision"]), reverse=True)
    unique = list({e["id"]: e for e in reversed(entries)}.values())[::-1]
    (dist / "archive.html").write_text(env.get_template("archive.html").render(entries=unique, asset_base=asset_base))
    (dist / "archive.json").write_text(json.dumps(unique))
    return target
