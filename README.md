# Signal & Sound

A quiet, two-page morning newspaper: general AI and AI for Music, with cited news, practical implications, explained concepts, and a printable A4 PDF.

**Built in Rust, with Typst for print.** The Rust application owns collection, Exa discovery, X relevance scoring, Bedrock analysis and verification, interest updates, HTML generation, PDF validation, archives, budgets and AWS Lambda handlers. HTML/CSS and a small browser script remain static assets. There is no Python application dependency.

[Read the newspaper](https://d27kukj7weob2m.cloudfront.net/) · [Archive](https://d27kukj7weob2m.cloudfront.net/archive.html)

## Run locally

Install Rust 1.89 or later and [Typst 0.15.1](https://github.com/typst/typst/releases/tag/v0.15.1). The lockfile pins dependencies; bundled Bitstream Vera fonts and Typst's embedded serif fonts make print output reproducible. The font license is included under `print/fonts/`.

```sh
cargo build --locked
cargo run --locked -- render --edition tests/fixtures/edition.json --dist dist
cargo run --locked -- serve --port 8787
```

Open http://localhost:8787. The fixture is historical demonstration content, dated October 21, 2026. It is not a fresh analysis. Set TYPST_BIN if Typst is outside PATH.

For fresh evidence and model analysis, use an authorized AWS profile with Bedrock access:

```sh
export AWS_PROFILE=YOUR_PERSONAL_PROFILE
export AWS_REGION=us-east-1
export EXA_API_KEY_PARAMETER=/signal-and-sound/exa-api-key
cargo run --locked -- collect
cargo run --locked -- run --kind daily
cargo run --locked -- run --kind weekly
cargo run --locked -- status
```

The weekly command requires a completed publication week. Direct feeds work without search credentials. Secrets are decrypted from SSM into memory, never embedded in Git, images, templates or public artifacts. Exa is the selected search provider; X remains optional. [Source setup](infra/SOURCES.md).

## Architecture

```text
EventBridge Scheduler — daily + Monday digest, 06:00 America/Lima
  → Rust collector Lambda
      → RSS / source websites / public APIs + Exa
      → private evidence checkpoints in S3
      → Bedrock extraction, writing and evidence verification
      → Typst preflight, edition schema and citation validation
  → Rust publisher Lambda, invoked only after valid analysis
      → static HTML + exact two-page Typst PDF
      → private publication S3 → CloudFront
```

DynamoDB provides atomic cost reservations and leases. API responses are checkpointed before parsing/validation so retries reuse paid results. Writing and review use [Bedrock native JSON-schema output](https://docs.aws.amazon.com/bedrock/latest/userguide/structured-output.html), with separate Rust validation of factual citations and publishing constraints. Failed or ambiguous requests retain their reservations. The last valid edition stays readable if a later run fails. Publication writes the latest pointer last; old revisions remain immutable. PDF attachment and inline print/share routes have explicit Content-Disposition headers.

Daily editions have six stories per page: four broad and two tailored to sanitized technical/artistic interests. Broad stories occupy roughly 60% of story words. Source publication dates, evidence maps, status labels and concept definitions are validated. A Trend needs two distinct dated source URLs. Retrieval text is untrusted evidence, never instructions; HTML is escaped and Typst reads strings from JSON rather than executing generated markup.

Monday also creates a digest of the completed Monday–Sunday week, comparing earlier editions. Archive summaries and original evidence are bounded: up to 50 sources balanced across both pages, with a small sample from the previous 28 days for context. The inaugural week is October 21–25, 2026. The daily run starts at 06:00 Lima, and rendering follows successful analysis.

## Private interests

Copy config/interests.example.json to ignored config/interests.json. Keep your richer private profile in ignored private/interests.json. Each observation has topic, source, date, kind (explicit/work), and reason; explicit observations also specify weight.

```sh
cargo run --locked -- interests private/observations.json
EVIDENCE_BUCKET=YOUR_PRIVATE_BUCKET cargo run --locked -- sync-interests
```

Explicit preferences are durable. Work-derived increases are capped at 0.05 per topic per week, recorded with provenance, and reversible through private version history. Automation frequency and silence do not establish interests. Only sanitized topic labels, keywords and weights reach the cloud writer. The local Codex weekly review can inspect authorized work; AWS cannot access local chats.

## Validation

```sh
cargo fmt --check
cargo test --locked
sam validate --lint --template-file infra/template.yaml --region us-east-1
```

Tests cover historical JSON compatibility, citations and dates, first-week boundaries, Exa request windows and paid checkpoint reuse, budgets and leases, interest provenance, SSRF address blocking, X relevance, privacy redaction, HTML/Typst injection, two-page A4 PDFs, overflow, and immutable revisions. Visually inspect both PDF pages and mobile layouts before publication.

## Deploy

Use your own authorized AWS account. See [deployment and rollback](infra/DEPLOYMENT.md). The image contains only the Rust executable, Typst, fonts, templates and sanitized example configuration. Source data and personal configuration are excluded from the build context.

```sh
docker buildx build --platform linux/arm64 --provenance=false --load -t signal-and-sound:release .
cargo run --locked -- prepare-deploy --image-uri YOUR_ECR_URI:RELEASE --output private/deploy-template.json
```

Infrastructure is disabled by default in the reusable template. The existing deployment retains its CloudFront URL, archive format, S3 buckets and DynamoDB records. New Typst editions use new revisions; previous PDFs are not rewritten.

Default limits target a small personal workload: $8 estimated application stop, $1 infrastructure reserve, $2 Bedrock cap, $3 search cap, 500 search calls and $1.60 X cap monthly. Exa credits may cover search usage, but budgeting counts metered cost anyway. These controls are estimates, not an invoice guarantee. X integration is inactive until a token is supplied.

## Rust code map

- `model`: typed editions and publishing invariants.
- `collect` / `search`: bounded public retrieval, Exa and X discovery.
- `compose` / `pipeline`: evidence-grounded model stages and orchestration.
- `state` / `aws`: local SQLite or DynamoDB, S3, credentials and official AWS SigV4 signing.
- `render` / `print/newspaper.typ`: static website and print output.
- `interests`: bounded, auditable personalization.
- `main`: local CLI/server and official AWS Lambda runtime.

MIT license. Bundled fonts retain their own license.
