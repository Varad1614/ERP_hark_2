# Part 2 · Stage 4.1 — Payment gateway (Razorpay) · worker S4-pay

Branch `part2/s4-pay` from `erp_merge` @ `c31cc487`. Tracker rows: [`../tracker/p2-s4-pay.md`](../tracker/p2-s4-pay.md).

**Status: sandbox-ready, live validation BLOCKED_EXTERNAL.** No Razorpay account / keys exist, so
every Razorpay call is exercised against a local HTTP stub that reproduces Razorpay's documented
request, response, error and signature formats (doc URLs below). Nothing here was run against
`api.razorpay.com`; no credential was invented.

## Part 1 changes confirmed in code (before building)

| Item | Where | Present |
|------|-------|---------|
| SEC-9D-003: production default `SUTRA_PAYMENT_GATEWAY=disabled` (`DisabledPaymentGateway`, 503 before signature handling) | `apps/api/src/config_b9.rs`, `modules/finance/src/payments/gateway.rs`, `payments/http.rs::gateway_webhook_signed` | yes |
| DEF-F-002 / SEC-P1-CONFIG-002: `mock` refused in production at load, start-up gate and Helm render | `config_b9.rs` (Forbidden), `config_http.rs::validate_production`, `deploy/helm/sutraerp/templates/core/configmap.yaml:36` | yes (kept; `razorpay` added as the only other production mode) |
| Webhook idempotency table `finance.gateway_webhook_event (gateway, event_id)` unique, append-only | `migrations/20261030000010_finance_payments.sql:116` | yes (reused; no new dedupe table needed) |
| DEF-X-003: webhook looked up in system context, applied in the payment's tenant | `payments/http.rs::gateway_webhook` | yes |
| DEF-F-004 (webhook amount mismatch leaves no trace) | — | was **open** → fixed here |
| DEF-F-006 (late capture after failed / expired / cancelled refused) | `payments/domain.rs::can_transition` already allows `failed|expired|cancelled → completed` | late capture of a **failed** payment now proven end to end (webhook + recovery job); expired / cancelled not separately tested here |
| DEF-F-007 (Student Fees UI always offers "test gateway") | `apps/web/app/features/finance/fees.tsx` | still **open** (UI not in this brief; see follow-ups) |

## What was built

| Piece | File(s) |
|-------|---------|
| Razorpay REST client (reqwest/rustls, HTTP basic `key_id:key_secret`, configurable base URL, per-request + connect timeouts, bounded exponential retry on timeouts / 429 / 5xx; order create idempotent on `receipt` — retried only after a `GET /v1/orders?receipt=` lookup, duplicate-receipt 400 resolved the same way; refunds idempotent with `X-Refund-Idempotency` + `receipt`; constant-time HMAC-SHA256 webhook + checkout signature checks; secrets redacted from `Debug` and errors) | `crates/infra/src/razorpay.rs` |
| Local Razorpay contract stub (axum; basic-auth check, Orders / Payments-of-order / Refunds endpoints, Razorpay error bodies, duplicate-receipt and over-refund errors, fault injection: status, delay → timeout, "lost answer"; signed webhook + checkout signature builders). Feature `razorpay-stub`, never wired in a server | `crates/infra/src/razorpay.rs::stub` |
| `RazorpayGateway` adapter of `PaymentGatewayPort` (rupees ↔ paise exact, INR only, receipt = `sutra_` + sha256(idempotency key)[..32] ≤ 40 chars, amount / currency of the created order checked, `fetch_outcome` from `GET /v1/orders/{id}/payments`: captured → Captured, attempt in flight → Pending, only failures → Failed, transport failure → 503 never a guess) | `apps/api/src/payment_gateway.rs` |
| Port extension `PaymentGatewayPort::verify_checkout(order, payment, signature)` (default refuses; mock + Razorpay implement HMAC-SHA256(`order_id|payment_id`, key secret)) | `crates/contracts/src/finance/ports.rs`, `modules/finance/src/payments/gateway.rs` |
| Checkout callback verification server-side: `POST /me/payments/{id}/confirm` takes an optional body (`razorpay_payment_id`, `razorpay_order_id`, `razorpay_signature` or camelCase `gateway*`); forged / other order / incomplete → 422 + audit `finance.payment.checkout_rejected`; verified → audit `finance.payment.checkout_verified`; **the fee is completed only on the gateway's own capture report** (`fetch_outcome`) or a verified webhook | `modules/finance/src/payments/http.rs` |
| Webhook: every processed (verified, first-seen) event audited `finance.gateway.webhook` in the payment's tenant; **DEF-F-004**: a verified event refused with 409 (captured amount ≠ payment) is recorded (`gateway_webhook_event.outcome = amount_mismatch`, audit `finance.gateway.webhook_rejected`), replays are `duplicate`. Event id for Razorpay = `"<event>:<entity id>"` from the signed body (the `x-razorpay-event-id` header is not signed) | `payments/http.rs` |
| Recovery job `payment_gateway_recovery` (sweeper, advisory-locked): polls open (`initiated`/`pending`) gateway payments older than `min_age` and `failed` ones younger than `expire_after`; captured → completed (same `apply_outcome` as confirm/webhook), failed → failed, still pending after `expire_after` → `expired` / `TIMEOUT`, gateway error → untouched; exponential back-off per payment (`gateway_polled_at`, `gateway_poll_count`); every poll audited `finance.payment.recovery_poll` | `modules/finance/src/payments/recovery.rs`, `apps/api/src/app.rs::payment_recovery` |
| Settlement reconciliation import: `POST /api/v1/finance/institutions/{id}/gateway-settlements` (CSV, `amountUnit=rupees|paise`, `treasury.manage`) and `GET /api/v1/finance/gateway-settlements/{id}` (`finance.read`). Razorpay recon columns; results `matched`, `amount_mismatch`, `order_mismatch`, `unknown_payment`, `payment_not_completed`, `already_settled`, `duplicate_line`, `refund_matched`, `refund_amount_mismatch`, `unknown_refund`, `ignored`; import + lines append-only; same file (SHA-256) → first import; audited `finance.gateway_settlement.import`. Posts nothing (matched payments go to the bank through the existing `finance_bank_settlement_create`) | `modules/finance/src/payments/settlements.rs` |
| Migration (append-only, RLS): `finance.payment.gateway_polled_at / gateway_poll_count` + recovery index, `finance.gateway_settlement_import`, `finance.gateway_settlement_line` (append-only triggers, `SELECT platform.apply_tenant_rls();`) | `migrations/20261231000410_finance_payment_gateway_razorpay.sql` |
| Refund failure reason names the adapter (was hard-coded "(mock)") | `modules/finance/src/refunds/http.rs` |
| Config + wiring | `apps/api/src/config_b9.rs`, `apps/api/src/app.rs` (`payment_gateway` now `Result`, `BuildError::Payments`) |

### Configuration

| Key | Meaning |
|-----|---------|
| `SUTRA_PAYMENT_GATEWAY=razorpay` | selects the adapter (`disabled` stays the production default; `mock` stays refused in production) |
| `SUTRA_RAZORPAY_KEY_ID` | `rzp_test_…` / `rzp_live_…`; **production requires `rzp_live_`** (test mode captures without money) |
| `SUTRA_RAZORPAY_KEY_SECRET` or `SUTRA_RAZORPAY_KEY_SECRET_FILE` | API key secret (OpenBao `kv/sutraerp/<env>/payments#razorpay_key_secret`); never logged |
| `SUTRA_PAYMENT_WEBHOOK_SECRET` or `SUTRA_PAYMENT_WEBHOOK_SECRET_FILE` | webhook secret; required with razorpay in every environment, never the dev constant, ≥ 32 chars in production |
| `SUTRA_RAZORPAY_BASE_URL` | default `https://api.razorpay.com`; https required in production |
| `SUTRA_RAZORPAY_TIMEOUT_MS` | default 10000 (500…60000) |
| `SUTRA_RAZORPAY_MAX_RETRIES` | default 2 (≤ 5) |
| `SUTRA_PAYMENT_RECOVERY_INTERVAL_SECS` | default 300; 0 = job off (also off when the gateway is disabled) |
| `SUTRA_PAYMENT_RECOVERY_MIN_AGE_SECS` | default 600 (first poll; back-off base) |
| `SUTRA_PAYMENT_RECOVERY_EXPIRE_AFTER_HOURS` | default 48 |
| `SUTRA_PAYMENT_RECOVERY_BATCH` | default 50 |

Razorpay dashboard settings the adapter assumes: **auto-capture enabled** (authorized payments are
treated as pending until Razorpay reports `captured`), webhook URL
`https://<host>/api/v1/finance/gateway/webhook` with events `payment.captured`, `payment.failed`,
`order.paid`.

## Provider documentation reproduced by the stub / tests

* Orders create (receipt ≤ 40, unique, duplicate → 400 "Duplicate request…"): https://razorpay.com/docs/api/orders/create/
* Fetch orders by receipt: https://razorpay.com/docs/api/orders/fetch-all/
* Payments of an order (`items[]` shape): https://razorpay.com/docs/api/orders/fetch-payments/
* Refunds (+ error texts) and idempotent refunds (`X-Refund-Idempotency`, ≥ 10 chars): https://razorpay.com/docs/api/refunds/create-normal/ , https://razorpay.com/docs/api/refunds/normal-refunds-idempotent/
* Webhook validation (`X-Razorpay-Signature` = hex HMAC-SHA256 of the raw body; duplicates expected): https://razorpay.com/docs/webhooks/validate-test/
* Webhook payloads (`payment.captured`, `payment.failed`, `order.paid`): https://razorpay.com/docs/webhooks/payments/
* Checkout signature `hmac_sha256(order_id + "|" + razorpay_payment_id, secret)`: https://razorpay.com/docs/payments/server-integration/nodejs/integration-steps/
* Settlement recon columns: https://razorpay.com/docs/api/settlements/fetch-recon/

## Tests

| Test | What it proves |
|------|----------------|
| `sutra-infra` `razorpay::tests` (11) | HMAC (RFC 4231 vector), tamper / wrong secret / truncated / non-hex signatures refused, webhook parsing + body-derived event id, checkout signature construction, derived receipt / refund keys within Razorpay limits, path-safe ids, outcome rules, secret redaction; **contract tests over real HTTP against the stub**: idempotent order on receipt, lost answer + 5xx retried without a second order, bounded retries → 503, timeouts retried then bounded, outcomes from order payments, wrong credentials → 503 without leaking, refunds idempotent on the key (lost answer), over-refund → 400, stub webhooks / checkout signatures verify |
| `sutra-api` `payment_gateway::tests` (3) | exact paise conversion; the adapter over the stub end to end (port semantics, idempotent create, failed / captured outcomes, webhooks, tamper → 401, checkout signature, refund through a lost answer); unreachable gateway → `Unavailable`, never `Pending` |
| `sutra-api` `config_b9::tests::razorpay_settings_and_production_rules` | defaults, required keys, malformed key id / URL / timeout / retries, dev secret refused, `_FILE` secrets (unreadable file → error), production: test key refused, http refused, short webhook secret refused |
| `sutra-finance` `gateway::tests::checkout_signatures_are_verified`, `settlements::tests` (2) | mock checkout signature; RFC 4180 CSV, Razorpay recon parsing in rupees / paise, BOM, error cases |
| `apps/api/tests/db_p2_pay.rs` (PostgreSQL 18 + stub) | `checkout_is_paid_only_on_a_server_side_capture`, `webhooks_are_idempotent_under_replay_and_concurrency`, `recovery_job_polls_open_and_failed_payments`, `refund_of_a_double_payment_reverses_the_ledger`, `settlement_report_reconciles_and_flags_mismatches`, `recovery_job_is_wired_with_the_sweepers` (scenarios in the file header) |
| Existing `db_b5b.rs` (mock gateway) | unchanged and still green: dev / CI keep the mock |

CI evidence: see the tracker (run ids).

## BLOCKED_EXTERNAL

| Item | What clears it |
|------|----------------|
| Live / test-mode Razorpay transactions (order → Checkout → capture → webhook → refund) | A Razorpay account: test-mode `rzp_test_…` key id + key secret, a webhook secret configured in the dashboard for a public HTTPS URL of a deployed API (`/api/v1/finance/gateway/webhook`, events `payment.captured`, `payment.failed`, `order.paid`), auto-capture on. Then run the same journey with `SUTRA_PAYMENT_GATEWAY=razorpay`. |
| Production go-live | `rzp_live_…` keys + live webhook secret in OpenBao (`kv/sutraerp/production/payments`), KYC-activated account |
| Settlement report format | One real "Settlement Recon" export from the dashboard to confirm the column headers / rupee formatting match the API field names the importer reads |

## Follow-ups (not in this brief)

* Student Fees UI: open Razorpay Checkout (`checkout.js`, needs CSP `script-src`/`frame-src` for `checkout.razorpay.com` and the public key id from the API) and post the callback to `confirm`; hide "Pay now" when the gateway is disabled (DEF-F-007).
* `refund.processed` / `refund.failed` webhooks to finish gateway refunds Razorpay answers as `pending` (today: refund stays `processing` until `/refunds/{id}/complete`).
* Settlement posting from a matched import (today: matched lines are posted with the existing bank settlement endpoint).
