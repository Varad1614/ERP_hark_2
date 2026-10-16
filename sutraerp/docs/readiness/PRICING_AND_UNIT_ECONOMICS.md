# SutraERP — pricing and unit economics (Part 2 · Stage 7, S7-commercial)

Branch `part2/s7-commercial` (base `part2/s4-int` @ `fac412ce`). Docs only. Currency INR, **all prices exclusive of GST**.

> **Labels.** Every SutraERP price below is a **PROPOSED list price (ASSUMPTION)** — no customer has been quoted and no price has
> been tested in a sale. Every competitor price is **UNVERIFIED**: taken from public web pages (vendor pages, marketplace listings
> or third-party blogs, cited by URL, read 2026-10-16), not from a written quote. Every cost, effort and financial input is an
> **ASSUMPTION** (no delivery history exists). GST at 18 % on software / services is an ASSUMPTION to be confirmed by the company's
> chartered accountant. The product is offered as Pilot-ready (see [MODULE_CATALOGUE.md](MODULE_CATALOGUE.md)); prices must not be
> presented with any "production-ready" claim.

## 1. Pricing principles

1. **One metric:** active enrolled students on the institution's census date (proposed: 31 August each academic year), counted
   from the system (Student records in `active` status). Staff logins are unlimited. ASSUMPTION.
2. **Subscription by default** (annual licence including Standard support and all updates); a perpetual licence + AMC is offered
   only where a tender requires it (§3.3).
3. **Self-hosted by default:** SutraERP installs offline on the college's own server (Stage 5 on-prem bundle). The college owns the
   hardware and the data. Managed hosting is optional and not yet drilled (§3.7).
4. **Design-partner pilot pricing** until a real institution has accepted the product: 50 % off the first-year licence in exchange
   for structured UAT, acceptance criteria agreed up front, and a reference if accepted. ASSUMPTION.
5. **Annual escalation capped at 7 %** for multi-year contracts. ASSUMPTION.

## 2. Market reference (UNVERIFIED)

| Vendor / source | Published price | Model | Source (read 2026-10-16) |
|---|---|---|---|
| Fedena (Foradian) | ₹50,000 / ₹75,000 / ₹1,00,000 + GST per year, unlimited users | flat per institution | quoted by [codingclave.com](https://codingclave.com/blog/college-erp-software-india) and [eduvy.in](https://eduvy.in/news/college-erp-pricing-in-india-what-colleges-actually-pay), both citing fedena.com — UNVERIFIED |
| Edusys College | ₹144 per student per year | per student | SoftwareSuggest listing via [codingclave.com](https://codingclave.com/blog/college-erp-software-india) — UNVERIFIED |
| EDUMAAT | ₹20–35 per student per month (₹240–420 / year) | per student | same — UNVERIFIED |
| Aveon College Management | ₹300 (Basic) – ₹550 (Enterprise) per student | per student | same — UNVERIFIED |
| iCloudEMS | ₹250 per user per year (listing also shows a conflicting USD figure) | per user | SoftwareSuggest via [eduvy.in](https://eduvy.in/news/college-erp-pricing-in-india-what-colleges-actually-pay) — UNVERIFIED |
| Kalvisalai | Standard ₹80–180, Premium ₹180–400 per student per year; minimum 800 students for colleges; cloud-hosted | per student | [kalvisalai.in/pricing](https://kalvisalai.in/pricing) (vendor page) — UNVERIFIED |
| Codingclave College ERP | ₹7,999 / month cloud or ₹79,999 one-time (vendor's own blog); eduvy.in reports the same vendor at ₹3,00,000 one-time or ₹10,000 / month — the sources conflict | flat | [codingclave.com](https://codingclave.com/blog/college-erp-software-india), [eduvy.in](https://eduvy.in/news/college-erp-pricing-in-india-what-colleges-actually-pay) — UNVERIFIED |
| MasterSoft (IITMS), Linways, Academia ERP (Serosoft), Camu, Entab | not published (quote only) | — | same pages — UNVERIFIED |
| One-time cost ranges (one vendor's estimate) | implementation ₹0–3 lakh, data migration ₹25,000–2 lakh, training ₹25,000–1.5 lakh, on-prem hardware ₹2–5 lakh, AMC 15–20 % of licence for one-time deals | — | [codingclave.com](https://codingclave.com/blog/college-erp-software-india) — UNVERIFIED |
| Razorpay gateway fee (paid by the college) | 2 % platform fee + 18 % GST (≈ 2.36 % all-in) on domestic methods | per transaction | reported by [eduvy.in](https://eduvy.in/news/college-erp-pricing-in-india-what-colleges-actually-pay) citing Razorpay — UNVERIFIED |

Reading: published college-ERP prices span roughly **₹50–550 per student per year** (UNVERIFIED). Flat-fee vendors are far cheaper
per student at 2,000 students (Fedena's top tier ≈ ₹50 / student). SutraERP's proposed prices sit in the middle-to-upper part of
the per-student range; the justification has to be what is *verified* — enforced maker-checker approvals, tamper-evident audit,
forced tenant isolation, on-prem data ownership with tested backup/restore — not breadth of features, where several modules are
Pilot-ready with gaps. **Commercial risk:** price-sensitive colleges will compare against flat-fee SaaS; the design-partner discount
and on-prem ownership are the levers, not a lower list price.

## 3. Price list (PROPOSED — ASSUMPTION)

### 3.1 Annual licence (subscription; includes Standard support and updates)

Packages are the cumulative tiers of the entitlement registry (`catalog/entitlements.json`, `part2/s7-entitle` @ `bf48b7f6`; see
[COMMERCIAL_PACKAGING.md](COMMERCIAL_PACKAGING.md) §1).

| Package (registry key) | Modules | ₹ per active student per year | Annual minimum | At 2,000 students |
|---|---|---:|---:|---:|
| `core_academic` | students, admissions, academics, attendance, examinations | 150 | 1,25,000 | 3,00,000 |
| `academic_plus` | + fees, library, reporting | 220 | 1,75,000 | 4,40,000 |
| `institution_operations` | + hr, payroll, hostel, transport | 270 | 2,50,000 | 5,40,000 |
| `complete_erp` | + procurement, inventory, assets (all 15) | 300 | 3,00,000 | 6,00,000 |

Add-on modules on a lower tier (registry `addOnModules`; registry dependencies and the commercial prerequisites of
COMMERCIAL_PACKAGING.md §4.2 / §5 apply), ₹ per active student per year, minimum ₹20,000 each:

| fees | library | reporting | hr | payroll | hostel | transport | procurement | inventory | assets |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 40 | 20 | 20 | 25 | 25 | 20 | 15 | 15 | 10 | 10 |

Bundle logic: `core_academic` + fees + library + reporting = ₹230 vs `academic_plus` ₹220; `academic_plus` + hr + payroll + hostel +
transport = ₹305 vs `institution_operations` ₹270; `institution_operations` + procurement + inventory + assets = ₹305 vs
`complete_erp` ₹300. Additional institution in the same tenant: +20 % of that institution's licence.

Until reconciliation items R1 / R2 (COMMERCIAL_PACKAGING.md §4.2) are tested, `core_academic` is quoted **with the `fees` add-on**
(₹190 / student) for any college that charges admission or examination fees.

### 3.2 One-time services

| Item | Price | Includes |
|---|---:|---|
| Implementation — `core_academic` | 2,00,000 | offline install on the college server (runbook), TLS / DNS, Keycloak realm + MFA for privileged roles, institution / programme / curriculum / calendar set-up, approval roles (maker ≠ checker), entitlement applied (`SUTRA_ENTITLEMENT_DEFAULT=none` + stored entitlement), backup timer + offsite copy, monitoring stack, 4 training days, 2 weeks go-live support |
| Implementation — `academic_plus` | 3,00,000 | as above + fee structures, library, reporting set-up, 6 training days |
| Implementation — `institution_operations` | 4,25,000 | as above + HR / payroll configuration, hostel and transport set-up, 7 training days |
| Implementation — `complete_erp` | 5,00,000 | all of the above + procurement & stores, 8 training days |
| Data migration — base | 75,000 | the 9 import targets (campuses, org units, programmes, curricula, staff, students + guardians, historical results, fee heads, opening balances) via the import engine (dry run, approval, reconciliation report); up to 2 academic years of history from clean templates or one clean legacy export |
| Data migration — each extra academic year | 25,000 | same targets |
| Data cleansing / transformation, other entities | 8,000 per person-day | time & material, estimated after a sample file (the import worker notes a real legacy export is needed first) |
| Training — additional day | 12,000 onsite (+ travel at actuals) · 8,000 remote | role-based (office, faculty, exam cell, finance, IT) |
| Online fee collection go-live (Razorpay) | 40,000 | test-mode run with the college's `rzp_test_…` keys, webhook secret on a public HTTPS URL, settlement-export reconciliation, then live switch — this is exactly the Stage 4 BLOCKED_EXTERNAL item |
| SMTP relay + identity hardening | 30,000 | realm SMTP (activation / recovery mails), production realm-admin grant for identity sync |
| Monitoring alert channels | 25,000 | college SMTP, chat webhook, SMS gateway webhook, dead-man service, delivery test (MONITORING_AND_SUPPORT.md §9–§10) |
| LDAP / AD federation (Keycloak) | 60,000 (cap; T&M) | **not tested** — scoped after a feasibility check |
| Custom report | 25,000 each | — |
| Customisation / integration development | 8,000 per person-day | T&M, fixed-price after scoping |

### 3.3 Perpetual licence alternative (tender use only)

| Item | Price |
|---|---|
| One-time licence | 2.5 × the annual subscription at the contracted student count (e.g. `complete_erp`, 2,000 students: ₹15,00,000) |
| AMC (from year 2; updates + Standard support) | 20 % of the one-time licence per year (₹3,00,000 in the example) |

AMC applies **only** to perpetual licences; subscriptions already include support and updates.

### 3.4 Support uplifts (annual) — terms in [SUPPORT_AND_MAINTENANCE.md](SUPPORT_AND_MAINTENANCE.md)

| Tier | Price |
|---|---|
| Standard | included in the licence / AMC |
| Priority | higher of ₹1,20,000 or 20 % of the annual licence |
| Critical 24×7 | higher of ₹3,00,000 or 40 % of the annual licence |

### 3.5 Hosting and server

| Option | Who pays whom | Indicative cost |
|---|---|---|
| Self-hosted (default) | college buys / provides the server | Minimum spec (pilot, ≤ ~2,000 students): 4 vCPU, 8 GiB RAM, 100 GiB SSD; recommended: 8 vCPU, 16 GiB RAM, 250 GiB SSD/NVMe RAID 1, plus separate backup disk / NAS ≥ 2× data, UPS ≥ 15 min ([INFRASTRUCTURE_REPORT.md](https://github.com/Varad1614/ERP_hark_2/blob/part2/s5-deploy/sutraerp/docs/readiness/INFRASTRUCTURE_REPORT.md) §3 — sizing *estimates* from a GitHub runner, not college hardware). Hardware price ASSUMPTION: ₹1.5–2.5 lakh (minimum) / ₹3–4.5 lakh (recommended) + ₹25–40k UPS + ₹40–60k NAS. |
| Managed hosting (optional) | college pays the vendor monthly | PROPOSED ₹45,000 / month up to 2,500 students (8 vCPU / 16 GiB / 250 GiB in an Indian region, offsite backups, monitoring, patching). **Not offerable until a cloud hosting drill passes** (COMMERCIAL_READINESS.md). |
| Payment gateway | college pays Razorpay directly | ≈ 2.36 % of online collections (UNVERIFIED, §2) — not vendor revenue |

## 4. Price examples (2,000 students, excl. GST)

| | `core_academic` (+ fees add-on) | `academic_plus` | `institution_operations` | `complete_erp` |
|---|---:|---:|---:|---:|
| Annual licence | 3,80,000 | 4,40,000 | 5,40,000 | 6,00,000 |
| Implementation | 2,00,000 | 3,00,000 | 4,25,000 | 5,00,000 |
| Data migration (base, 2 years) | 75,000 | 75,000 | 75,000 | 75,000 |
| Razorpay go-live + SMTP / identity | 70,000 | 70,000 | 70,000 | 70,000 |
| **Year-1 total (list)** | **7,25,000** | **8,85,000** | **11,10,000** | **12,45,000** |
| Year-1 total with design-partner pilot (50 % off licence) | 5,35,000 | 6,65,000 | 8,40,000 | 9,45,000 |
| Year-2 licence (+7 %) | 4,06,600 | 4,70,800 | 5,77,800 | 6,42,000 |

## 5. Cost assumptions (ASSUMPTION — no delivery history)

| Input | Value | Basis |
|---|---|---|
| Loaded cost of an engineer / implementer | ₹5,000 per person-day | ASSUMPTION (≈ ₹1.1 lakh / month loaded, 22 days) |
| Implementation effort, `complete_erp` | 50 person-days | ASSUMPTION; on-prem install itself measured at 91 s on a runner (INFRASTRUCTURE_REPORT.md §2.1) — effort is configuration, master data, roles and acceptance, not installation |
| Data migration effort | 12 person-days | ASSUMPTION |
| Training delivery | 8 trainer-days + ₹40,000 travel | ASSUMPTION |
| Integrations (Razorpay go-live, SMTP / identity) | 8 person-days | ASSUMPTION |
| Hypercare, first 8 weeks | 20 person-days | ASSUMPTION |
| Standard support, steady state | 3 person-days / month (+5 % per year) | ASSUMPTION |
| Product maintenance allocation (security patches, releases) | 15 % of licence revenue | ASSUMPTION |
| Sales / pre-sales cost per won college (CAC) | ₹1,50,000 one-time | ASSUMPTION |
| Account management (year 2+) | ₹30,000 / year | ASSUMPTION |
| General overhead | 10 % of revenue | ASSUMPTION |
| Third-party licence fees | ₹0 assumed (bundle is open-source components) | **RISK, not verified:** MinIO is AGPL-3.0 and is shipped built from source inside the on-prem bundle; Keycloak, Flowable, NATS are Apache-2.0, PostgreSQL its own licence, Valkey BSD-3 (licence names from general knowledge, not re-checked in this stage). Needs a legal review of redistribution obligations before the first sale |

## 6. Cost and margin model — one 2,000-student institution, `complete_erp`, self-hosted

Inputs above (the arithmetic is reproducible from §5; a working script was kept outside the repo). All figures ASSUMPTION.

### 6.1 Year 1

| Revenue | ₹ | Cost | ₹ |
|---|---:|---|---:|
| Licence 2,000 × ₹300 | 6,00,000 | Implementation 50 pd | 2,50,000 |
| Implementation | 5,00,000 | Migration 12 pd | 60,000 |
| Data migration (base) | 75,000 | Training 8 days + travel | 80,000 |
| Razorpay go-live | 40,000 | Integrations 8 pd | 40,000 |
| SMTP + identity hardening | 30,000 | Hypercare 20 pd | 1,00,000 |
| | | Standard support 36 pd | 1,80,000 |
| | | Product maintenance (15 % licence) | 90,000 |
| | | Sales / pre-sales (CAC) | 1,50,000 |
| | | Overhead (10 % revenue) | 1,24,500 |
| **Revenue** | **12,45,000** | **Cost** | **10,74,500** |
| | | **Gross margin** | **1,70,500 (13.7 %)** |

### 6.2 Five-year view

| Year | Revenue | Cost | Margin | Margin % |
|---|---:|---:|---:|---:|
| 1 | 12,45,000 | 10,74,500 | 1,70,500 | 13.7 % |
| 2 | 6,42,000 | 3,79,500 | 2,62,500 | 40.9 % |
| 3 | 6,86,940 | 4,00,185 | 2,86,755 | 41.7 % |
| 4 | 7,35,026 | 4,22,129 | 3,12,897 | 42.6 % |
| 5 | 7,86,478 | 4,45,411 | 3,41,067 | 43.4 % |
| **5-year** | **40,95,443** | **27,21,724** | **13,73,719** | **33.5 %** |

Year 2+ cost = support (36 pd, +5 %/yr) + product maintenance (15 % of licence) + account management ₹30,000 + overhead (10 % of licence);
licence escalates 7 %/yr.

### 6.3 Sensitivity (Year 1 / Year 2 margin)

| Scenario | Y1 revenue | Y1 margin | Y2 margin |
|---|---:|---:|---:|
| Base (above) | 12,45,000 | 1,70,500 (13.7 %) | 2,62,500 (40.9 %) |
| Design-partner pilot (50 % off Y1 licence) | 9,45,000 | −99,500 (−10.5 %) | 2,62,500 (40.9 %) |
| Implementation overruns by 50 % (75 pd) | 12,45,000 | 45,500 (3.7 %) | 2,62,500 (40.9 %) |
| 1,000 students (licence at the ₹3,00,000 minimum) | 9,45,000 | −54,500 (−5.8 %) | 21,750 (6.8 %) |
| 3,000 students | 15,45,000 | 3,95,500 (25.6 %) | 5,03,250 (52.3 %) |

Readings: the first college is loss-making on pilot terms and only pays back in year 2; support cost is largely fixed, so
institutions below ~1,500 students on `complete_erp` barely cover support at the minimum fee — either raise the minimum or sell
`core_academic`; implementation effort is the largest controllable risk (no delivery history yet).

### 6.4 The college's view (TCO, 2,000 students, `complete_erp`, ASSUMPTION)

| | Year 1 | Year 2+ |
|---|---:|---:|
| SutraERP (list, §4) | 12,45,000 | 6,42,000 (+7 %/yr) |
| Server (recommended spec) + UPS + NAS (mid-points) | ≈ 4,57,500 | refresh ~every 5 years |
| College IT time (≈ 0.25 FTE for backups, patching, first-line support — RACI in SUPPORT_AND_MAINTENANCE.md) | own staff | own staff |
| Razorpay fees on online collections | ≈ 2.36 % of collections (UNVERIFIED) | same |
| Per student (vendor + hardware) | ≈ ₹851 | ≈ ₹321 |

### 6.5 Managed hosting unit economics (PROPOSED, not yet offerable)

| Monthly cost item (ASSUMPTION) | ₹ |
|---|---:|
| VM 8 vCPU / 16 GiB, Indian region | 22,000 |
| 250 GiB SSD | 2,500 |
| Object storage + offsite backup copy | 3,500 |
| Operations (backup checks, patching, monitoring) ≈ 1 pd | 5,000 |
| **Cost** | **33,000** |
| **Price** | **45,000** |
| **Margin** | **12,000 (26.7 %)** |

## 7. What must happen before these prices are used in a quote

1. Re-check this price list against the entitlement registry when `part2/s7-entitle` is integrated (packages here match registry `version: 1` @ `bf48b7f6`), and close reconciliation items R1 / R2 (COMMERCIAL_PACKAGING.md §4.2).
2. Chartered-accountant review of GST treatment and invoicing; legal review of open-source redistribution (MinIO AGPL-3.0) and of the
   EULA, DPA (DPDP Act 2023) and offboarding terms.
3. Replace effort assumptions with the actuals from the first design-partner pilot.
4. Obtain at least three written competitor quotes on identical scope; the §2 figures are UNVERIFIED web listings.
