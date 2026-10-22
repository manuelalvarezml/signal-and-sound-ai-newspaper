# Optional search connections

The newspaper works with configured feeds, direct sources and Hacker News without API keys. Search expands discovery beyond the feed list; it does not replace reading and checking the linked article.

## Exa (selected provider)

Store the key as an AWS Systems Manager SecureString at `/signal-and-sound/exa-api-key` in us-east-1. Deploy with `ExaKeyParameter=/signal-and-sound/exa-api-key`. Lambda retrieves it with decryption into EXA_API_KEY in process memory; it is never embedded in CloudFormation, the image, Git, or public files. Local checks should retrieve the same parameter in-process through the authorized personal AWS profile, not save it in shell history or an environment file.

Integration follows Exa's [build-with-exa skill](https://github.com/exa-labs/agent-skills/tree/main/skills/build-with-exa) and [Search reference](https://exa.ai/docs/reference/search): POST /search, type auto, contents.highlights true. Four daily queries cover general AI and music AI; weekly runs add two roundup queries. Daily searches cover the preceding 24 hours to the analysis cutoff. Weekly searches cover the completed Monday–Sunday period (October 21–25 for the inaugural week). Date filters can omit undated pages; feeds remain a complementary source. No domain/category restrictions or Exa-generated summaries are requested. AWS already schedules runs, so Exa Monitors is unnecessary.

The collector reads and verifies discovered articles with the existing bounded fetcher. Exa highlights help rank candidates; they are not treated as automatically verified claims. Private search checkpoints retain the request, response and cost for recovery, with the existing 45-day expiration. Public editions expose source links and editorial summaries, not raw API payloads. The [Exa terms](https://exa.ai/assets/Exa_Labs_Terms_of_Service.pdf) reviewed October 22, 2026 do not contain the blanket response-retention prohibition found in Brave's FAQ; third-party source rights still apply.

Budgeting reserves $0.02 per request and settles Exa's costDollars.total when present. This conservatively covers Auto search plus ten highlight extractions at the checked rates. Missing cost metadata keeps the full reservation. Limits are $3 metered search usage and 500 requests/month, plus the shared application ceiling; free credits are not presumed by the budget. Roughly 150 queries/month should fit the advertised $10 monthly credit, but account credit availability is not inferred from possession of a key. Keep automatic top-ups disabled if you want credits-only use.

A Lambda invocation with `{"action":"check_search"}` runs the four queries, reports counts/costs and checks real SSM permissions without invoking the writer or publisher. It consumes normal budget and stores private checkpoints. Credentials are never returned. Failed/ambiguous requests retain their reservation and are not automatically reissued.

Brave has been removed as the active adapter. X remains separate and disabled until its token is supplied.

## X

Direct X access is optional. The [current rate card](https://docs.x.com/x-api/getting-started/pricing) lists $0.005 per post read. The default sample is ten posts daily, roughly $1.50–$1.55 per month, with a $1.60 application ceiling. Do not enable automatic credit replenishment for this default configuration. Store the bearer token in a SecureString parameter under `/signal-and-sound/` and pass its name as XTokenParameter.

Queries require external links and exclude replies and reposts. A 65/100 acceptance threshold combines topic relevance (35%), linked evidence (25%), novelty (15%), curated author credibility (10%), recency (10%), and engagement (5%). Fetch and verify linked primary evidence before writing. This bounded sample is not comprehensive monitoring of X.

## Coverage review

Review missed stories, primary-source yield, stale/duplicate results, music coverage and total cost. Exa is the owner's selected provider; another provider is not required.
