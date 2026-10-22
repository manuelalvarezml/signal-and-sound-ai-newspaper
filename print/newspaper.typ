#let data = json("edition.json")
#let edition = data.edition
#let ink = rgb("232821")
#let accent = rgb("ad482f")
#set document(title: "Signal & Sound | " + edition.date, author: "Signal & Sound")
#set page(paper: "a4", margin: (x: 12mm, top: 10mm, bottom: 20mm), footer: context [
  #line(length: 100%, stroke: 0.4pt + rgb("babeb3"))
  #v(3pt)
  #text(size: 7pt)[Sources checked through #data.cutoff Lima | Older items labeled Revisited | #edition.id]
  #v(2pt)
  #grid(columns: (1fr, auto), text(size: 7pt)[#link(data.site_url + "/archive.html")[Archive and clickable source links] | AI-assisted edition.], text(size: 7pt)[#counter(page).display() / 2])
])
#set text(font: "Bitstream Vera Sans", size: 10.5pt, fill: ink, lang: "en")
#set par(justify: false, leading: 3pt, spacing: 4pt)
#let sources = edition.sources.map(s => (s.id, s)).to-dict()
#let concepts = edition.concepts.map(c => (c.id, c)).to-dict()
#let story(s) = block(breakable: false, below: 9pt)[
  #let source = sources.at(s.source_ids.first())
  #let date = if source.published == none { "Undated" } else { source.published.slice(0, 10) }
  #text(size: 7pt, fill: accent)[#(if s.lane == "broad" {"BROAD VIEW"} else {"YOUR INTERESTS"}) / #upper(s.status) / #date]
  #v(3pt)
  #text(font: "Libertinus Serif", size: 14pt, weight: "bold", s.title)
  #v(3pt)
  #for p in s.paragraphs [#p #parbreak()]
  #text(size: 9.5pt)[*Why it matters.* #s.implication]
  #v(3pt)
  #text(size: 8pt, fill: accent)[#s.source_ids.map(id => link(sources.at(id).url, sources.at(id).publisher)).join(" / ")]
]
#for (i, page) in edition.pages.enumerate() {
  if i > 0 { pagebreak() }
  grid(columns: (1fr, auto), align: (left, right),
    text(font: "Libertinus Serif", size: 29pt, weight: "bold")[Signal & Sound],
    [#text(size: 9pt, weight: "bold")[#(if page.id == "ai" {"General AI"} else {"AI for Music"})] #h(6pt) #text(size: 8pt, edition.date)
     #v(5pt)
     #text(size: 7pt)[#upper(edition.kind) / #(i + 1) | #data.minutes MIN READ - BOTH PAGES]]
  )
  v(5pt)
  line(length: 100%, stroke: 1.3pt + ink)
  v(7pt)
  text(size: 9.5pt)[*EDITOR'S NOTE* #h(4pt) *Today* #h(4pt) #page.lead.replace(regex("^Today['’]s "), "")]
  v(10pt)
  let broad = page.stories.filter(s => s.lane == "broad")
  let personal = page.stories.filter(s => s.lane == "personal")
  grid(columns: (1fr, 1fr), column-gutter: 7mm,
    block(breakable: false)[#for s in broad.slice(0, 3) { story(s) }],
    block(breakable: false)[#for s in broad.slice(3) + personal { story(s) }]
  )
  v(2pt)
  line(length: 100%, stroke: 0.4pt + rgb("babeb3"))
  v(5pt)
  text(size: 8pt, fill: accent, weight: "bold")[CONCEPTS TO KEEP]
  v(6pt)
  let all-ids = page.stories.map(s => s.concepts).flatten().dedup()
  let ids = all-ids.slice(0, calc.min(2, all-ids.len()))
  grid(columns: (1fr, 1fr), column-gutter: 7mm, ..ids.map(id => {
    let c = concepts.at(id)
    block(breakable: false)[
      #text(size: 9.5pt)[#strong(c.term + ".") #c.definition]
      #v(3pt)
      #text(size: 9.5pt)[*Why it matters.* #c.why_it_matters]
    ]
  }))
}
