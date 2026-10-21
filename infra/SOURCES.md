# Optional search connections

The newspaper works with configured feeds, direct sources and Hacker News without API keys. Search expands discovery beyond the feed list; it does not replace reading and checking the linked article.

## Brave Search

Start with the [Search plan](https://brave.com/search/api/), not Answers. As checked October 21, 2026, Search costs $5 per 1,000 requests and includes $5 in monthly credits. The [billing help](https://api-dashboard.search.brave.com/documentation/resources/help-feedback) explains how to select $0 prepaid credit. Keep paid replenishment disabled. The default application makes four daily queries and six for the Monday digest, with a 500-request monthly ceiling shared across daily and weekly jobs.

Create the key yourself and store it as an encrypted AWS Systems Manager SecureString parameter named `/signal-and-sound/brave-api-key`. Pass that parameter name as BraveKeyParameter when deploying. Do not paste the key into a chat, Git issue, command-line argument, or tracked file. The Lambda role can read parameters only under this application's namespace. Restart neither the website nor the publisher; the next analysis reads the configured parameter.

The adapter uses Brave's web search endpoint with daily or weekly freshness filters. An indexed page is not necessarily a newly published event: extraction and editorial date checks remain necessary. Public attribution belongs in the project's README/about information.

## X

Direct X access is optional. The [current rate card](https://docs.x.com/x-api/getting-started/pricing) lists $0.005 per post read. The default sample is ten posts daily, roughly $1.50–$1.55 per month, with a $1.60 application ceiling. Do not enable automatic credit replenishment for this default configuration. Store the bearer token in a SecureString parameter under `/signal-and-sound/` and pass its name as XTokenParameter.

Queries require external links and exclude replies and reposts. A 65/100 acceptance threshold combines topic relevance (35%), linked evidence (25%), novelty (15%), curated author credibility (10%), recency (10%), and engagement (5%). Fetch and verify linked primary evidence before writing. This bounded sample is not comprehensive monitoring of X.

## Evaluate before adding providers

Brave is a starting choice, not a proven universal optimum. Review the first two weeks for missed stories, primary-source yield, stale/duplicate results, music coverage and total cost. Only add or replace a provider when a small comparison on the same queries demonstrates a useful coverage gain. Tavily and Exa are plausible alternatives; neither is required by this implementation.
