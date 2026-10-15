# Re-skin v2 — common rules for every slice worker (R1–R7, R8 integrate)

Canonical copies live in the repo (`sutraerp/docs/reskin2/`: this file, `plan.md`, `slice-briefs.md`, `rs2-ci.sh`) and
are mirrored to `/workspace/work/tools/rs2-worker-common.md`, `/workspace/work/tools/rs2-ci.sh`,
`/workspace/work/brief/reskin2-plan.md`, `/workspace/work/brief/reskin2-slice-briefs.md` (and the project work dir
`/workspace/projects/63e5e795-01c5-4321-b2b9-1a04cf4103fd/work/{tools,brief}/`). If a mirror is missing after a sandbox
wipe, read the repo copy on `origin/reskin2/foundation`.

**Read first:** `sutraerp/docs/reskin2-contracts.md` (LAW), your brief in `slice-briefs.md`, `plan.md`, the audit
`/workspace/work/ui-audit/gap-report.md` (+ `table.md`) and the prototype PNGs `/workspace/work/ui-audit/proto/*.png`
(list `proto/inventory.txt`). The prototype file itself is
`/workspace/projects/63e5e795-01c5-4321-b2b9-1a04cf4103fd/files/uploads/SutraERP-Prototype-Lite.final.html`;
`/workspace/work/ui-audit/proto-capture.cjs <role> pages|x1|x2|x3` re-captures it (Playwright, 1440×960, Plus Jakarta
Sans installed locally) if a PNG is missing. The prototype is the ONLY UI/UX source. Do not copy: the "Prototype · mock
data" tag, "Simulate progress" buttons, the oversized Reports arrow.

## Repo / branches
- Repo `github.com/Varad1614/ERP_hark_2`, app `sutraerp/` (web `apps/web`, design system `packages/ui`).
- Base: `origin/reskin2/foundation` (R0). Your branch: **`reskin2/rN`** (r1 … r7; R8 = `reskin2/integrate`). CI runs on
  `reskin2/**` (push trigger + e2e matrix).
- Pass `credentials: [{"provider":"github"}]` on every shell call that pushes or calls the GitHub API.
- Git identity `Varad1614 <Varad1614@users.noreply.github.com>`.
- **NEVER push, merge or force anything to `erp_merge`** (nor `reskin2/foundation` or another slice's branch). The user
  must approve anything that reaches erp_merge.

## CI-ONLY
- **No local builds or tests at all:** no cargo, no `pnpm build/test/typecheck/lint`, no vitest, no docker, no chromium /
  playwright runs. A "light" typecheck is not allowed either. GitHub Actions is the only compiler.
- Sandbox wipes are frequent (the whole `/workspace/work` mount can vanish): clone into a writable dir (e.g.
  `/workspace/work/rs2-rN`, else the project `work/` dir), **commit and push every file or two**, push without `[skip ci]`.
- A push cancels the previous run on the same branch (`concurrency: cancel-in-progress`): batch one CI round's fixes into
  one push, but never keep unpushed work for more than ~30 min.
- Poll with `bash rs2-ci.sh reskin2/rN [runs]` (credentials github), **sleeping ~4 min between polls** (shell calls cap at
  120 s: sleep in two calls). `RS2_LOG=1` prints failed jobs' log tails (`RS2_TAIL=200`). Green = every job of the run on
  your head sha: Rust, Rust integration, OpenAPI drift, Web (typecheck · lint · unit tests · build), the 8 e2e suites
  (finance, student, academic, ui, exams, people, campus, **reskin2**), Container images.
- If the same failure loops more than twice with no new approach, stop and report it.

## What a slice may touch
- Only its own `apps/web/app/features/<area>/**`, its route modules, the e2e suite(s) driving its screens, and its own
  `seed<Area>()` function in `apps/web/e2e/lib/reskin2-seed.mjs` (+ one `domain()` line in `seedReskin2`).
  Shell / Home / overlays belong to R1.
- **Never edit `packages/ui`.** Need a prop, variant or widget? Put `NEEDS-R0: …` in your status line and final report;
  a throw-away local wrapper inside your feature folder is fine if trivially replaceable (say so). R0 adds it on
  `reskin2/foundation`; you rebase.
- No API / OpenAPI / migration / Rust changes (the re-skin is web-only). If a screen genuinely needs data the API does not
  return, report it — never show invented numbers (KPI sparklines only for real series).

## Keep all functionality
- Every action, form, API call, permission gate, step-up prompt, `If-Match` version and idempotency key stays. Re-skinning
  moves controls (≤5 header tabs + More, kebab menus, side panels, dialogs); it never deletes them.
- Update e2e selectors in the same push. Prefer stable `data-*` hooks over text. Move 1280-wide suites to 1440×960.
- Use the formatters (`formatINR`, `formatNumber`, `formatDays`, `formatDate`, `humanize`, `StatusBadge`): no raw uuids,
  no wire codes, no `12.00 days`, no ungrouped rupees, no mm/dd/yyyy.

## Visual proof
- The `reskin2` suite uploads artifact **`e2e-shots-reskin2`**: every screen at 1440×960 full page as
  `<adm|fac|stu>-<path with / → _>.png`, plus `manifest.json` (file, role, path, h1), `seed-report.txt`, `problems.txt`
  (console / API errors seen on screens — read it, fix yours). Download for your run:
  `curl -fsS https://api.github.com/repos/Varad1614/ERP_hark_2/actions/runs/<run>/artifacts` → artifact id →
  `curl -fsSL -o shots.zip https://api.github.com/repos/Varad1614/ERP_hark_2/actions/artifacts/<id>/zip` (credentials github).
- Side-by-side sheets: `python3 /workspace/work/ui-audit/pair.py out.png protoA appA [protoB appB …]` (prototype left,
  app right, 720 px columns, crop height env `H=560`) into `/workspace/work/rs2-out/rN/`. Look at every pair; fix what
  does not match before reporting.
- Add your area's demo rows to `reskin2-seed.mjs` when your screens would otherwise be empty.

## Status
- Append lines to BOTH `/workspace/work/tools/rs2-status.txt` and
  `/workspace/projects/63e5e795-01c5-4321-b2b9-1a04cf4103fd/work/tools/rs2-status.txt` (`mkdir -p`; time from
  `TZ=Asia/Kolkata date +%H:%M`): `HH:MM RN <what> PROGRESS <overall %>`. Overall %: R0 = 0–12, R1–R7 = 12–90 (≈ 11
  points per slice; report your share), R8 = 90–100. Write `RN CI green <sha> (run <id>)` and `RN DONE <sha>`.

## Final report
Green sha + run id + job table; routes re-skinned (route → prototype PNG); tabs before → after per area; where every moved
action lives now (More / kebab / panel / dialog); e2e selector changes; seed additions; side-by-side sheet paths;
`NEEDS-R0` requests; anything not done.
