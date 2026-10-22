# AWS deployment

Use a personal AWS account and explicit deployment authorization. The stack has two Lambdas, a daily EventBridge schedule, two private S3 buckets, a DynamoDB budget/state table, CloudFront origin access control, the AWS-managed CachingOptimized policy, and a Free pricing subscription with a WAF association. Deployment creates IAM roles scoped to these services. No public S3 bucket or paid domain is required.

## Build and release

Requirements: Docker, AWS CLI, an authenticated AWS profile, and optionally SAM CLI for validation. Run tests and inspect the two PDF pages first.

```sh
sam validate --lint --template-file infra/template.yaml --region us-east-1
docker buildx build --platform linux/arm64 --provenance=false --load -t signal-and-sound:release .
```

Create a private ECR repository for this project, log Docker in using `aws ecr get-login-password` piped to `docker login --password-stdin`, tag the release with your repository URI, and push it. Keep credentials out of command arguments and logs. Retain only a few images using an ECR lifecycle policy.

Prepare the deployment template with the pushed image URI:

```sh
cargo run --locked -- prepare-deploy --image-uri YOUR_ECR_URI:RELEASE --output private/deploy-template.json
aws cloudformation deploy --template-file private/deploy-template.json \
  --stack-name signal-and-sound --region us-east-1 \
  --capabilities CAPABILITY_IAM CAPABILITY_AUTO_EXPAND \
  --parameter-overrides ProductionEnabled=false DeliveryEnabled=false
```

Both functions share a Rust binary and Typst in one Debian container. APP_ROLE selects collector or publisher; the official Rust Lambda runtime handles invocation. The application has no Python runtime or package dependency. Publication uses a DynamoDB lease rather than reserved concurrency, so accounts with the default low Lambda quota are supported. The standard SAM build path is also supported; the direct Docker path avoids legacy Docker-builder issues on Apple Silicon.

## Enable the service

1. Keep the managed cache policy: custom caching rules require a higher plan tier. Mutable pointers use no-cache headers; the managed policy imposes a one-second minimum cache lifetime. Confirm CloudFront Free-plan enrollment and the completed stack before enabling delivery. The template's subscription resource specifies FREE, not a paid plan. Its resource-scoped lint exemption accommodates older cfn-lint catalogs; CloudFormation itself must validate the resource. Official reference: https://docs.aws.amazon.com/AWSCloudFormation/latest/TemplateReference/aws-resource-pricingplanmanager-subscription.html . If the account is ineligible, resolve that before public launch.
2. Confirm access to the configured Bedrock models and current provider prices. Defaults use Nova Micro for extraction and GPT-OSS 120B for writing and independent per-page evidence review, through Bedrock in us-east-1. Optional Exa/X credentials belong in encrypted SSM parameters under `/signal-and-sound/`; pass their parameter names to the stack. Paid search replenishment must remain disabled. No source credential is mandatory for the feed-only mode.
3. Upload only sanitized config/interests.json to the EvidenceBucket output at config/interests.json. Never upload biography or raw Codex chats. Use EVIDENCE_BUCKET=YOUR_BUCKET AWS_PROFILE=YOUR_PROFILE cargo run --locked -- sync-interests after deployment authorization.
4. Enable ProductionEnabled and smoke-test analysis and publishing. A manually reviewed edition can be saved privately under editions/ID.json and published by invoking PublisherFunction with an edition_key. The publisher creates attachment and inline PDF variants with explicit HTTP Content-Disposition headers.
5. Enable DeliveryEnabled after validation. Check both pages and anonymous PDF retrieval over HTTPS, including a phone-width browser test. Actual printing uses the device's native PDF share/print interface; the app cannot verify a physical printer.
6. Verify the 06:00 America/Lima schedule. Monday daily and weekly runs execute sequentially. The first weekly edition covers October 21–25, 2026; change started_at for another installation.

## Interest review integration

AWS cannot read local Codex chats. A local Monday 05:30 Codex automation may inspect human-authorized work and update private beliefs, then upload only sanitized topic weights to this project's evidence bucket. Keep the full biography and observation log local. Explicit preferences override inference; automated reminders do not count as evidence. Version history supports reversal.

## Failure handling and rollback

- Failed analysis: the last good public issue remains live; inspect bounded logs and private checkpoints. Model responses are saved privately before validation. One bounded compact extraction can recover a truncated/invalid extraction response. Writing and review use native Bedrock JSON-schema output; Rust still enforces citations, word allocation, dates and PDF size. Up to two correction passes can repair an invalid draft; every paid stage remains budgeted. Source dates determine New/Revisited labels, and original source excerpts back citation provenance.
- Failed rendering: latest.json is not changed. Shorten copy if the print layout overflows.
- Ambiguous paid request: keep its reservation and investigate before reissuing it. Do not generate a fresh identity to evade a budget refusal.
- Scheduled runs are idempotent by edition date. Editorial corrections increment the revision.
- Set ProductionEnabled=false to stop new analysis. Existing static editions can remain readable. DeliveryEnabled controls public delivery separately.
- Restore an earlier latest.json version or release pointer to roll back an edition; immutable edition URLs remain available.

## Costs and limits

Defaults target $5/month, with an $8 estimated application stop, a $1 infrastructure reserve, a $2 Bedrock cap and a $1.60 X cap. Search stops at 500 calls/month or $3 of metered usage, whichever comes first; Exa account credits may cover that usage. These are application estimates, not an AWS invoice guarantee. The Free CloudFront plan covers eligible delivery under its terms; S3, Lambda, image storage and logs have separate usage. Verify current rates and use account budget alerts with a user-selected notification destination. Error alarms have no inferred email/SMS recipients.

## Rust migration validation

The collector action `check_search` verifies Exa, SSM, DynamoDB and S3 without writing an edition. `check_pipeline` runs collection, extraction, writing, evidence review and Typst validation; it saves a private draft under checks/ and does not publish. Both consume the normal application budget. A completed draft can be promoted only under a new edition revision, then passed to the publisher. Local `check-cloud --pipeline --run-id ID` uses the same implementation and requires the deployed environment values; reuse its run ID to recover paid checkpoints.
