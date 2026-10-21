# Deployment handoff — not executed

Manuel must approve public AWS launch after reviewing the local style and content. Keep ProductionEnabled=false and DeliveryEnabled=false until then. This repository contains no live credentials.

## Build and verify after approval

1. Run local tests and inspect both PDF pages. Install/use AWS SAM and Docker; `sam build --template-file infra/template.yaml --use-container` builds the two image functions from the locked requirements. CloudFormation/SAM service validation and a Docker build remain launch checks; they were not available during this local preview.
2. Confirm region/model access, current Bedrock prices, X per-post charges and the Brave free-credit account settings. Disable paid search replenishment. Connect optional tokens as encrypted SSM parameters under `/signal-and-sound/`. Do not put credentials in template parameters, source control or logs. Default SSM-managed encryption is assumed; a customer KMS key needs a scoped decrypt grant.
3. Deploy the stack in us-east-1 with both enable parameters false. No public delivery or analysis should run at this stage. Review resource changes and account budget permissions.
4. Verify/enroll the CloudFront distribution in the Free flat-rate plan before enabling delivery. The template intentionally does not claim PriceClass_100 is a free plan: it is not. Free-plan eligibility and enrollment must be checked in AWS. If the account is ineligible, pause launch for a cost decision. Official docs: https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/flat-rate-pricing-plan.html and https://docs.aws.amazon.com/PricingPlanManager/latest/UserGuide/plans.html .
5. Upload only `config/interests.json` to the private evidence bucket at `config/interests.json`. Never upload private/profile.md or raw Codex chats. An approved sanitized profile can be synchronized later using scripts/sync_interests.py. Update the Codex weekly heartbeat's upload authorization only after launch approval.
6. Enable ProductionEnabled for a controlled smoke run, with DeliveryEnabled still false. Invoke Collector with an explicit kind/cutoff. Verify source coverage, actual Bedrock cost, evidence validation, PDF page count and publication objects. Confirm failure alarms and retain-last-good behavior. Verify endpoint 403 for direct S3 access and readable CloudFront output when delivery is enabled.
7. Enable DeliveryEnabled only once Free flat-rate enrollment and public-content review are confirmed. Confirm the 06:00 America/Lima schedule. Monday daily and weekly analyses run sequentially; each completion invokes the publisher. Publisher concurrency is one, so pointer updates serialize.
8. Configure an account AWS Budget alert at $5/$8/$10 using the user's chosen notification destination after it is supplied. CloudWatch error alarms exist without notification destinations. The application budget cannot enforce a hard account invoice ceiling. No email or SMS destination has been inferred.

## Failure handling

- Failed analysis: prior issue stays live; inspect bounded logs and the private checkpoint.
- Failed rendering: publisher does not move latest.json; shorten editorial copy if print overflows.
- Ambiguous paid request: reservation is retained; investigate before clearing it. Do not retry with a new identity simply to bypass a budget refusal.
- Date identity makes scheduled analysis idempotent. Manual same-day revisions require an explicitly revised editorial JSON, not another paid automatic run.
- Source outages are recorded. Fewer than four usable sources blocks automated composition. More sources are needed for a useful ten-brief issue; the writer may use labeled learning context but cannot invent news.
- Turn ProductionEnabled=false to stop analysis. Delivery can continue serving existing static editions. DeliveryEnabled=false disables public delivery separately.
- No VPC/NAT, database server, user authentication service or paid domain is required. Public output is intentionally anonymous editorial copy.

## Cost assumptions to verify

The configured $8 application stop reserves $1 for infrastructure and caps Bedrock at $2 and X at $1.60/month. Reservations use conservative text token bounds, then reconcile reported usage. Search calls stop at 500/month and assume free credits with no recharge. CloudFront Free plan covers eligible delivery within its terms; other AWS services, image storage and logs still have costs. Keep one small image per function and apply ECR retention after SAM image repositories exist. Recheck current prices before enabling anything.
