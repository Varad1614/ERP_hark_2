#!/usr/bin/env node
/**
 * Batch 8 (Campus · Services · Resources) end-to-end click-through in a real Chromium (integrator).
 *
 *   pnpm e2e:campus          (from sutraerp/; CI matrix entry `campus`, not part of `pnpm -r test`)
 *
 * Reuses the shared harness (`e2e/lib/harness.mjs`): starts the API binary (dev auth, PostgreSQL)
 * and the web app in API mode on a fresh, migrated database, then walks Domains G · H · J across
 * the administrator and student roles, one screenshot per step:
 *
 *   seed     institution → `active` (Batch 2 API); two active students (Aarav linked to the dev
 *            student login, Isha); the site → building → floor of the main campus plus a hostel
 *            room and the library space (8A API).
 *   admin    Infrastructure & rooms: classroom registered under the floor (screen) → booked.
 *   admin    Service desk: ticket → triage → corrective work order → start → complete → verify.
 *   admin    Hostel: policy (screen) → submitted (auto-approved) → active; beds (API); Aarav's
 *            self-service application (API) → bed allocated (screen) → hostel fee in Finance.
 *   admin    Transport: stops + route + activation + Aarav's request + pass (API) → screen.
 *   admin    Procurement: items, store, supplier → requisition → direct sourcing → PO → goods
 *            receipt (API) → receipt accepted, invoice matched and posted (screen); Inventory
 *            stock on hand; Assets: the laptop from the receipt placed in the classroom.
 *   admin    Library: policy → active, member, catalogue title + copy, issue, overdue check (loan
 *            back-dated through psql), return with fine, fine billed through Finance.
 *   admin    Reports: widgets → dashboard → version → submit (auto-approved) → publish (live view);
 *            Regulatory: framework → activate → cycle → collect → validate → package → submit →
 *            submit to regulator → acknowledged.
 *   student  Library: loans, fines, catalogue search → reserve a title that is out; dark mode.
 *
 * Screenshots: $E2E_SHOTS (default /workspace/work/out/b8-campus-shots), prefixed `8-`. Every
 * console error, page error and failed /api response fails the run. Requires `psql` on PATH.
 */
import { existsSync, mkdirSync, readdirSync, unlinkSync } from "node:fs";
import path from "node:path";
import { chromium } from "playwright";
import {
  apiBin,
  apiLogin,
  BASE,
  check,
  children,
  expectRowStatus,
  failed,
  fillForm as fillDialogForm,
  go,
  log,
  newProblems,
  psql,
  row,
  seedInstitution,
  seedStudent,
  settle,
  shot,
  shotCount,
  SHOTS,
  signIn,
  signOut,
  startApi,
  startWeb,
  step,
  stop,
  STUDENT_SUBJECT,
  tile,
  waitFor,
  watch,
} from "./lib/harness.mjs";

const C = "/api/v1/campus";
const R = "/api/v1/resources";

/** Every dialog form closes on success: wait for it so the next dialog mounts fresh. */
async function fillForm(page, name, values) {
  await fillDialogForm(page, name, values);
  await page.locator(`[data-form="${name}"]`).waitFor({ state: "detached", timeout: 15000 });
  await settle(page);
}

/**
 * Re-skin v2: a row shows ONE primary pill (`[data-action]`); every other action sits in its kebab
 * (`[data-row-menu]` → `[data-menu-item=id]`). Finds `id` as a visible button, else opens the kebabs in
 * `scope` (a row or a tile) until one offers it. Returns the element to click, or null.
 */
async function findAction(page, id, scope) {
  const root = scope ?? page;
  const direct = root.locator(`[data-action="${id}"]`).first();
  if (await direct.isVisible().catch(() => false)) return direct;
  const kebabs = root.locator("[data-row-menu]");
  const n = await kebabs.count();
  for (let i = 0; i < n; i++) {
    const k = kebabs.nth(i);
    if (!(await k.isVisible().catch(() => false))) continue;
    await k.click();
    const item = root.locator(`[data-menu-item="${id}"]`).first();
    if (await item.isVisible({ timeout: 500 }).catch(() => false)) return item;
    await page.keyboard.press("Escape");
  }
  return null;
}

/** Clicks action `id` (pill or kebab item) in `scope`, waiting for it to appear, then for the network. */
async function action(page, id, scope) {
  const deadline = Date.now() + 15000;
  for (;;) {
    const el = await findAction(page, id, scope);
    if (el) {
      await el.click();
      await settle(page);
      return;
    }
    if (Date.now() > deadline) throw new Error(`action ${id} not offered (pill or kebab)`);
    await page.waitForTimeout(500);
  }
}

/** Clicks action `id` when it is offered (auto-approval may skip a step). */
async function maybeAction(page, id, scope) {
  const el = await findAction(page, id, scope);
  if (!el) return false;
  await el.click();
  await settle(page);
  return true;
}

/** Clicks action `id` in the table row containing `text` (pill or kebab). */
async function rowAction(page, text, id, scope) {
  const r = row(page, text, scope);
  await r.waitFor({ timeout: 10000 });
  await action(page, id, r);
}

/** Clicks action `id` in the row containing `text` when it is offered. */
async function maybeRowAction(page, text, id, scope) {
  const r = row(page, text, scope);
  if (!(await r.isVisible().catch(() => false))) return false;
  return maybeAction(page, id, r);
}

/** A re-skinned list row (`ListRow`, `.sb-lrow`) containing `text`. */
function lrow(page, text, scope) {
  return (scope ?? page).locator(".sb-lrow", { hasText: text }).first();
}

async function display(page, setting) {
  await page.click('[aria-label="Account menu"]');
  // Re-skin v2 (R1): display settings live in the Theme drawer (avatar menu → Personalization → Theme).
  if (!(await page.locator("[data-pz-group]").count())) await page.click("[data-pz-toggle]");
  await page.click('[data-pz="theme"]');
  await page.locator(`[data-display="${setting}"]`).click();
  await page.keyboard.press("Escape");
  await page.mouse.click(4, 700);
  await settle(page);
}

const iso = (d) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
const TODAY = iso(new Date());
const YEAR = new Date().getFullYear();
const plusDays = (n) => iso(new Date(Date.now() + n * 864e5));

// ---------------------------------------------------------------- run
let api;
let web;
let browser;
const ids = {};
try {
  if (!existsSync(apiBin)) throw new Error(`API binary not found at ${apiBin} — run \`cargo build -p sutra-api\` or set SUTRA_API_BIN`);
  mkdirSync(SHOTS, { recursive: true });
  for (const f of readdirSync(SHOTS)) if (/^(\d\d|fail)-.*\.png$/.test(f)) unlinkSync(path.join(SHOTS, f));
  const existing = psql("SELECT count(*) FROM campus.library_loan");
  if (existing !== "0") throw new Error(`DATABASE_URL must be a fresh migrated database (found ${existing} library loans)`);

  log(`API (dev auth), web on ${BASE}, shots → ${SHOTS}; today ${TODAY}`);
  api = await startApi();
  web = startWeb();

  // ------------------------------------------------------------ seed
  log("— seed: institution, students, infrastructure");
  const admin = await apiLogin("admin@sutra.dev");
  const setup = await seedInstitution(admin);
  Object.assign(ids, setup);
  const inst = setup.id;
  ids.aarav = seedStudent(setup, "Aarav", "Desai", "S2026-0001");
  psql(`UPDATE student.student SET subject_ref = '${STUDENT_SUBJECT}' WHERE id = '${ids.aarav}'`);
  ids.isha = seedStudent(setup, "Isha", "Kulkarni", "S2026-0002");
  const campusId = psql(`SELECT id FROM institution.campus WHERE institution_id = '${inst}' ORDER BY created_at LIMIT 1`);
  const node = async (body) => (await admin.call("POST", `${C}/institutions/${inst}/infrastructure-nodes`, { status: "active", ...body })).node;
  ids.site = await node({ nodeType: "site", campusId, code: "MAIN", name: "Main campus site" });
  ids.building = await node({ nodeType: "building", parentId: ids.site.id, code: "MAIN-B1", name: "Academic block A" });
  ids.floor = await node({ nodeType: "floor", parentId: ids.building.id, code: "MAIN-B1-F1", name: "Block A, first floor" });
  ids.hostelBlock = await node({ nodeType: "building", parentId: ids.site.id, code: "MAIN-H1", name: "Boys' hostel — Krishna" });
  ids.hostelFloor = await node({ nodeType: "floor", parentId: ids.hostelBlock.id, code: "MAIN-H1-F1", name: "Krishna, ground floor" });
  ids.hostelRoom = await node({ nodeType: "space", parentId: ids.hostelFloor.id, code: "MAIN-H1-G01", name: "Room G-01", spaceType: "hostel_room", capacity: 2 });
  ids.library = await node({ nodeType: "space", parentId: ids.floor.id, code: "MAIN-B1-LIB", name: "Central library", spaceType: "library", capacity: 120 });
  log(`  ✓ institution ${inst}, students Aarav + Isha, site → block → floor → hostel room + library`);
  const aarav = await apiLogin("aarav.desai@sutra.dev");

  await waitFor(`${BASE}/sign-in`, web);
  browser = await chromium.launch({ headless: !process.env.E2E_HEADED, executablePath: process.env.CHROMIUM_PATH || undefined, args: ["--no-sandbox", ...(process.env.E2E_CHROMIUM_ARGS ?? "").split(" ").filter(Boolean)] });
  const context = await browser.newContext({ viewport: { width: 1440, height: 960 }, locale: "en-IN", timezoneId: "Asia/Kolkata" });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  watch(page);
  page.setDefaultNavigationTimeout(180000);
  for (const p of ["/sign-in", "/sign-in"]) await go(page, p);

  await signIn(page, "administrator");
  for (const p of ["/campus/infrastructure", "/campus/library", "/reports"]) await go(page, p);
  page.setDefaultNavigationTimeout(30000);
  newProblems();

  // ------------------------------------------------------------ 8A: infrastructure + booking
  await step(page, "admin: Infrastructure — classroom registered under the floor (tree campus → building → floor → rooms)", async () => {
    await go(page, "/campus/infrastructure");
    const t = tile(page, "Campus infrastructure");
    await row(page, "MAIN-B1-F1", t).waitFor();
    await action(page, "node-new", t);
    await fillForm(page, "node", { nodeType: "space", parentId: ids.floor.id, code: "MAIN-B1-101", name: "Room 101", spaceType: "classroom", capacity: "60", status: "active" });
    await row(page, "MAIN-B1-101", t).waitFor();
    await expectRowStatus(page, "MAIN-B1-101", "Active", t);
    const tree = await admin.get(`${C}/institutions/${inst}/infrastructure-nodes`);
    const items = tree.items ?? tree;
    ids.classroom = items.find((n) => n.code === "MAIN-B1-101");
    check(!!ids.classroom, "classroom registered");
    await shot(page, "8-admin-infrastructure-tree");
  });

  await step(page, "admin: Room booking — Room 101 booked for tomorrow (confirmed)", async () => {
    await action(page, "booking-new", tile(page, "Room bookings"));
    await fillForm(page, "reservation", { nodeId: ids.classroom.id, date: plusDays(1), startTime: "10:00", endTime: "12:00", attendees: "45", purpose: "First-year orientation" });
    const b = tile(page, "Room bookings");
    await row(page, "First-year orientation", b).waitFor();
    await maybeRowAction(page, "First-year orientation", "booking-confirm", b);
    await expectRowStatus(page, "First-year orientation", "Confirmed", b);
    await shot(page, "8-admin-room-booking");
  });

  // ------------------------------------------------------------ 8A: service desk
  await step(page, "admin: Service desk — ticket triaged, work order started, completed and verified", async () => {
    await go(page, "/campus/facilities");
    const desk = tile(page, "Service desk");
    await action(page, "ticket-new", desk);
    await fillForm(page, "ticket", { category: "maintenance", priority: "medium", nodeId: ids.classroom.id, title: "Projector not working in Room 101" });
    await row(page, "Projector not working", desk).waitFor();
    await rowAction(page, "Projector not working", "ticket-triage", desk);
    await fillForm(page, "ticket-triage", { priority: "high" });
    await rowAction(page, "Projector not working", "ticket-workorder", desk);
    await fillForm(page, "ticket-workorder", { assignee: "Ravi Kumar (AV)" });
    const wo = tile(page, "Work orders");
    await row(page, "WO-", wo).waitFor();
    await maybeAction(page, "wo-start", wo);
    await action(page, "wo-complete", wo);
    await fillForm(page, "wo-complete", { notes: "Replaced the HDMI cable and projector lamp" });
    await action(page, "wo-verify", wo);
    await expectRowStatus(page, "WO-", "Verified", wo);
    await desk.getByRole("button", { name: "All" }).click().catch(() => {});
    await settle(page);
    await shot(page, "8-admin-service-desk-work-order");
  });

  // ------------------------------------------------------------ 8B: hostel
  await step(page, "admin: Hostel — policy active, Aarav's application allocated a bed (hostel fee raised)", async () => {
    await go(page, "/campus/hostel");
    const pol = tile(page, "Hostel policy");
    await action(page, "hostel-policy-new", pol);
    await fillForm(page, "hostel-policy", { name: `Hostel policy ${YEAR}-${String(YEAR + 1).slice(2)}`, feePerTerm: "45000.00", deposit: "5000.00", curfewTime: "21:30", effectiveFrom: `${YEAR}-01-01` });
    await rowAction(page, "Hostel policy", "hostel-policy-submit", pol);
    await maybeRowAction(page, "Hostel policy", "hostel-policy-approve", pol);
    await rowAction(page, "Hostel policy", "hostel-policy-activate", pol);
    await expectRowStatus(page, "Hostel policy", "Active", pol);
    for (const bed of ["A", "B"]) await admin.call("POST", `${C}/hostel-rooms/${ids.hostelRoom.id}/beds`, { bedCode: bed });
    const app = await aarav.call("POST", `${C}/me/hostel/application`, { institutionId: inst, preference: "Near the library" });
    check(["approved", "under_review"].includes(app.status), `self-service application ${app.number} (${app.status})`);
    await go(page, "/campus/hostel");
    const apps = tile(page, "Admission applications");
    await apps.getByRole("button", { name: app.status === "approved" ? "Approved" : "Under review" }).first().click();
    await settle(page);
    await row(page, app.number, apps).waitFor();
    await maybeRowAction(page, app.number, "hostel-app-approve", apps);
    await rowAction(page, app.number, "hostel-app-allocate", apps);
    await fillForm(page, "hostel-allocate", {});
    const residents = tile(page, "Residents");
    await row(page, "MAIN-H1-G01", residents).waitFor();
    await apps.getByRole("button", { name: "All", exact: true }).first().click();
    await settle(page);
    const charge = psql(`SELECT count(*) FROM finance.charge WHERE student_id = '${ids.aarav}' AND source_type = 'campus_charge'`);
    check(charge === "1", `hostel fee charge raised in Finance (${charge})`);
    await shot(page, "8-admin-hostel-allocation");
  });

  // ------------------------------------------------------------ 8B: transport
  await step(page, "admin: Transport — route active with stops, Aarav's seat and pass (transport fee)", async () => {
    const stops = [];
    for (const [code, name, landmark] of [
      ["ST-GATE", "Main gate", "Campus entrance"],
      ["ST-RLY", "Shivajinagar station", "Railway station, east exit"],
    ])
      stops.push(await admin.call("POST", `${C}/institutions/${inst}/transport-stops`, { code, name, landmark }));
    let route = await admin.call("POST", `${C}/institutions/${inst}/transport-routes`, { code: "R-01", name: "City loop", capacity: 40, fare: "6000.00", effectiveFrom: `${YEAR}-01-01` });
    const v1 = (await admin.get(`${C}/transport-routes/${route.id}/versions`)).items[0];
    await admin.call(
      "POST",
      `${C}/route-versions/${v1.id}/stops`,
      {
        stops: [
          { stopId: stops[1].id, sequence: 1, pickupTime: "07:30" },
          { stopId: stops[0].id, sequence: 2, pickupTime: "07:55" },
        ],
      },
      v1.version,
    );
    route = await admin.call("POST", `${C}/transport-routes/${route.id}/activate`, {}, route.version);
    check(route.status === "active", `route R-01 active (${route.status})`);
    const ride = await aarav.call("POST", `${C}/me/transport/request`, { institutionId: inst, routeId: route.id, stopId: stops[1].id });
    const pass = await admin.call("POST", `${C}/transport-allocations/${ride.id}/pass`, { validFrom: TODAY, validTo: `${YEAR + 1}-06-30` });
    check(pass.status === "active", `transport pass ${pass.number} active`);
    await go(page, "/campus/transport");
    await row(page, "R-01", tile(page, "Routes")).waitFor();
    await row(page, pass.number, tile(page, "Transport passes")).waitFor();
    await shot(page, "8-admin-transport-route-pass");
  });

  // ------------------------------------------------------------ 8D: procurement → stock → assets
  await step(page, "admin: Procurement — supplier → requisition → PO → goods receipt accepted, invoice matched and posted", async () => {
    const item = async (code, name, kind, standardCost) => {
      const i = await admin.call("POST", `${R}/institutions/${inst}/items`, { code, name, kind, unit: "each", standardCost });
      return admin.call("POST", `${R}/items/${i.id}/activate`, {}, i.version);
    };
    ids.paper = await item("PAPER-A4", "A4 copier paper (ream)", "consumable", "250.00");
    ids.laptop = await item("LAPTOP-14", "Laptop 14-inch", "asset", "55000.00");
    ids.store = await admin.call("POST", `${R}/institutions/${inst}/stores`, { code: "CENTRAL", name: "Central store" });
    let sup = await admin.call("POST", `${R}/institutions/${inst}/suppliers`, {
      legalName: "Deccan Office Supplies Pvt Ltd",
      pan: "ABCDE1234F",
      email: "sales@deccanoffice.in",
      bankAccountName: "Deccan Office Supplies Pvt Ltd",
      bankAccountNumber: "001234567890",
      bankIfsc: "HDFC0001234",
    });
    sup = await admin.call("POST", `${R}/suppliers/${sup.id}/register`, {}, sup.version);
    sup = await admin.call("POST", `${R}/suppliers/${sup.id}/submit`, {}, sup.version);
    let req = await admin.call("POST", `${R}/institutions/${inst}/requisitions`, {
      title: "Term start office and lab supplies",
      justification: "Department office and new faculty laptop",
      lines: [
        { itemId: ids.paper.id, quantity: "40", estimatedUnitCost: "250.00" },
        { itemId: ids.laptop.id, quantity: "1", estimatedUnitCost: "55000.00" },
      ],
    });
    req = await admin.call("POST", `${R}/requisitions/${req.id}/submit`, {}, req.version);
    let ev = await admin.call("POST", `${R}/institutions/${inst}/sourcing-events`, { requisitionId: req.id, method: "direct", title: "Term start supplies" });
    ev = await admin.call("POST", `${R}/sourcing-events/${ev.id}/quotations`, { supplierId: sup.id, lines: [{ lineNo: 1, unitPrice: "240.00" }, { lineNo: 2, unitPrice: "52000.00" }] }, ev.version);
    await admin.call("POST", `${R}/sourcing-events/${ev.id}/award`, { quotationId: ev.quotations[0].id }, ev.version);
    let po = await admin.call("POST", `${R}/institutions/${inst}/purchase-orders`, { sourcingEventId: ev.id, deliveryLocationId: ids.store.id });
    po = await admin.call("POST", `${R}/purchase-orders/${po.id}/issue`, {}, po.version);
    ids.po = po;
    const grn = await admin.call("POST", `${R}/purchase-orders/${po.id}/receipts`, { receivedOn: TODAY, deliveryNote: "DN-4471", lines: [{ lineNo: 1, quantity: "40" }, { lineNo: 2, quantity: "1" }] });
    ids.grn = grn;
    await go(page, "/campus/procurement");
    const receipts = tile(page, "Goods receipts");
    await row(page, grn.number, receipts).waitFor();
    await rowAction(page, grn.number, "receipt-accept", receipts);
    await expectRowStatus(page, grn.number, "Accepted", receipts);
    const inv = await admin.call("POST", `${R}/institutions/${inst}/vendor-invoices`, {
      purchaseOrderId: po.id,
      supplierInvoiceNumber: "DOS/2026/1187",
      invoiceDate: TODAY,
      dueDate: plusDays(30),
      lines: [
        { lineNo: 1, quantity: "40", unitPrice: "240.00" },
        { lineNo: 2, quantity: "1", unitPrice: "52000.00" },
      ],
    });
    await go(page, "/campus/procurement");
    const invoices = tile(page, "Vendor invoices");
    await row(page, inv.number, invoices).waitFor();
    await rowAction(page, inv.number, "invoice-match", invoices);
    await maybeRowAction(page, inv.number, "invoice-approve", invoices);
    await rowAction(page, inv.number, "invoice-post", invoices);
    await expectRowStatus(page, inv.number, "Posted", invoices);
    await shot(page, "8-admin-procurement");
  });

  await step(page, "admin: Inventory — accepted receipt on hand in the central store", async () => {
    await go(page, "/campus/inventory");
    const stock = tile(page, "Stock on hand");
    await row(page, "PAPER-A4", stock).waitFor();
    await shot(page, "8-admin-inventory-stock");
  });

  await step(page, "admin: Assets — the laptop from the goods receipt, placed in Room 101", async () => {
    const grn = await admin.get(`${R}/goods-receipts/${ids.grn.id}`).catch(() => null);
    const lines = grn?.lines ?? ids.grn.lines ?? [];
    const line = lines.find((l) => l.lineNo === 2) ?? lines[1];
    let asset = await admin.call("POST", `${R}/institutions/${inst}/assets`, { goodsReceiptLineId: line.id, serialNumber: "LT-2026-0001" });
    asset = await admin.call("POST", `${R}/assets/${asset.id}/assign`, { assigneeKind: "space", assigneeId: ids.classroom.id, note: "Lecture podium" }, asset.version);
    check(asset.status === "assigned", `asset ${asset.assetNumber ?? asset.id} assigned`);
    await go(page, "/campus/assets");
    await row(page, "LT-2026-0001", tile(page, "Asset register")).waitFor();
    await shot(page, "8-admin-assets");
  });

  // ------------------------------------------------------------ 8C: library
  await step(page, "admin: Library — policy active, member added, title catalogued with a copy", async () => {
    await go(page, "/campus/library");
    const pol = tile(page, "Borrowing policy");
    await action(page, "policy-new", pol);
    await fillForm(page, "library-policy", { name: `Library rules ${YEAR}`, effectiveFrom: `${YEAR}-01-01`, maxLoansStudent: "3", finePerDay: "2.00", maxFine: "100.00" });
    await rowAction(page, `Library rules ${YEAR}`, "policy-submit", pol);
    await maybeRowAction(page, `Library rules ${YEAR}`, "policy-approve", pol);
    await rowAction(page, `Library rules ${YEAR}`, "policy-activate", pol);
    await expectRowStatus(page, `Library rules ${YEAR}`, "Active", pol);
    await action(page, "member-new", tile(page, "Members"));
    await fillForm(page, "library-member", { personKind: "student", personId: ids.aarav });
    await row(page, "Aarav", tile(page, "Members")).waitFor();
    const cat = tile(page, "Catalogue");
    await action(page, "record-new", cat);
    await fillForm(page, "catalogue-record", { title: "Fluid Mechanics", authors: "Frank M. White", isbn: "9789353166625", publisher: "McGraw Hill", replacementCost: "650.00" });
    await rowAction(page, "Fluid Mechanics", "record-publish", cat);
    await rowAction(page, "Fluid Mechanics", "record-copy", cat);
    await fillForm(page, "library-copy", { shelfMark: "ENG-532" });
    await rowAction(page, "Fluid Mechanics", "record-copies", cat);
    await row(page, "ACC-", tile(page, "Copies")).waitFor();
    await shot(page, "8-admin-library-catalogue");
  });

  // Student-facing library data: Aarav has a book out; a second title is out with Isha (so Aarav can reserve it).
  await step(page, "seed: library loans for the student view (Aarav holds one title, Isha the only copy of another)", async () => {
    const members = (await admin.get(`${C}/institutions/${inst}/library-memberships`)).items;
    ids.member = members.find((m) => m.personId === ids.aarav) ?? members[0];
    const isha = await admin.call("POST", `${C}/institutions/${inst}/library-memberships`, { personKind: "student", personId: ids.isha });
    const title = async (t, authors, shelfMark) => {
      const rec = await admin.call("POST", `${C}/institutions/${inst}/catalogue`, { title: t, authors, replacementCost: "500.00" });
      await admin.call("POST", `${C}/catalogue-records/${rec.id}/publish`, {}, rec.version);
      const copy = await admin.call("POST", `${C}/catalogue-records/${rec.id}/copies`, { nodeId: ids.library.id, shelfMark });
      return { rec, copy };
    };
    const math = await title("Higher Engineering Mathematics", ["B. S. Grewal"], "MAT-510");
    const algo = await title("Introduction to Algorithms", ["Cormen", "Leiserson", "Rivest", "Stein"], "CSE-005");
    await admin.call("POST", `${C}/institutions/${inst}/library-loans`, { membershipId: ids.member.id, copyId: math.copy.id });
    await admin.call("POST", `${C}/institutions/${inst}/library-loans`, { membershipId: isha.id, copyId: algo.copy.id });
    log("  ✓ Aarav: Higher Engineering Mathematics; Isha: Introduction to Algorithms");
  });

  await step(page, "admin: Library — copy issued to Aarav, overdue check marks the back-dated loan overdue", async () => {
    await go(page, "/campus/library");
    const records = (await admin.get(`${C}/institutions/${inst}/catalogue`)).items;
    ids.fluid = records.find((r) => r.title === "Fluid Mechanics");
    const copies = (await admin.get(`${C}/catalogue-records/${ids.fluid.id}/copies`)).items;
    const circ = tile(page, "Circulation");
    await action(page, "loan-new", circ);
    await fillForm(page, "library-issue", { membershipId: ids.member.id, accessionNumber: copies[0].accessionNumber });
    await row(page, "Fluid Mechanics", circ).waitFor();
    await page.waitForTimeout(300);
    const loans = (await admin.get(`${C}/institutions/${inst}/library-loans`)).items;
    ids.loan = loans.find((l) => l.status === "active" && l.title === "Fluid Mechanics");
    // Back-date relative to the institution's business date (what the API's fine maths uses),
    // not the DB server's UTC current_date: between 18:30 and 24:00 UTC they differ by a day.
    psql(`UPDATE campus.library_loan l SET issued_on = d.today - 19, due_on = d.today - 5
          FROM (SELECT (now() AT TIME ZONE i.timezone)::date AS today FROM institution.institution i WHERE i.id = '${inst}') d
          WHERE l.id = '${ids.loan.id}'`);
    await go(page, "/campus/library");
    await action(page, "overdue-sweep", tile(page, "Circulation"));
    await expectRowStatus(page, "Fluid Mechanics", "Overdue", tile(page, "Circulation"));
    await shot(page, "8-admin-library-overdue");
  });

  await step(page, "admin: Library — returned with a ₹10 fine, billed through Finance", async () => {
    const circ = tile(page, "Circulation");
    await rowAction(page, "Fluid Mechanics", "loan-return", circ);
    const fines = tile(page, "Fines");
    await row(page, "Overdue", fines).waitFor();
    await expectRowStatus(page, "Overdue", "₹10", fines);
    await rowAction(page, "Overdue", "fine-bill", fines);
    await expectRowStatus(page, "Overdue", "Billed", fines);
    await circ.getByRole("button", { name: "All" }).first().click();
    await settle(page);
    await shot(page, "8-admin-library-fine-billed");
  });

  // ------------------------------------------------------------ 8C: reports
  await step(page, "admin: Reports — widgets defined, dashboard version submitted (approved) and published (live view)", async () => {
    await go(page, "/reports/dashboards");
    const widgets = tile(page, "Widgets");
    for (const [code, title, key] of [
      ["W-RESIDENTS", "Hostel residents", "campus.hostel_residents"],
      ["W-FINES", "Library fines unsettled", "campus.library_fines_unsettled"],
      ["W-PAYABLES", "Payables outstanding", "resources.payables_outstanding"],
    ]) {
      await action(page, "widget-new", widgets);
      await fillForm(page, "widget-new", { code, title, metricKey: key, visualization: "stat" });
      await rowAction(page, code, "widget-activate", widgets);
    }
    const dash = tile(page, "Dashboards");
    await action(page, "dashboard-new", dash);
    await fillForm(page, "dashboard-new", { code: "CAMPUS-OPS", name: "Campus operations", category: "campus", purpose: "Residence, library and payables at a glance for the registrar" });
    await rowAction(page, "CAMPUS-OPS", "dashboard-version", dash);
    await fillForm(page, "dashboard-version", { w_0: true, w_1: true, w_2: true, changeNote: "First layout" });
    await rowAction(page, "CAMPUS-OPS", "dashboard-submit", dash);
    await maybeRowAction(page, "CAMPUS-OPS", "dashboard-approve", dash);
    await rowAction(page, "CAMPUS-OPS", "dashboard-publish", dash);
    await expectRowStatus(page, "CAMPUS-OPS", "v1", dash);
    await rowAction(page, "CAMPUS-OPS", "dashboard-assign", dash);
    await fillForm(page, "dashboard-assign", { role: "administrator" });
    await page.locator("[data-dashboard-view]").getByText("Hostel residents").first().waitFor();
    await shot(page, "8-admin-reports-dashboard-live");
  });

  await step(page, "admin: Regulatory — AISHE framework active, cycle collected and validated", async () => {
    await go(page, "/reports/regulatory");
    const fw = tile(page, "Reporting frameworks");
    await action(page, "framework-new", fw);
    await fillForm(page, "framework-new", { code: "AISHE", regulator: "AISHE", name: `AISHE annual data capture ${YEAR}-${String(YEAR + 1).slice(2)}`, m_0: true, m_1: true, m_2: true, manualFields: "naac_grade|NAAC grade|required; remarks|Remarks" });
    await rowAction(page, "AISHE", "framework-submit", fw);
    await maybeRowAction(page, "AISHE", "framework-approve", fw);
    await rowAction(page, "AISHE", "framework-activate", fw);
    await expectRowStatus(page, "AISHE", "Active", fw);
    await rowAction(page, "AISHE", "cycle-new", fw);
    await fillForm(page, "cycle-new", { periodLabel: `${YEAR}-${String(YEAR + 1).slice(2)}`, dueOn: plusDays(45) });
    await action(page, "cycle-collect");
    await fillForm(page, "cycle-collect", { naac_grade: "A+", remarks: "Figures as of the census date" });
    const snaps = page.locator(".sb-tile").filter({ has: page.locator("h4", { hasText: /^Snapshots/ }) }).first();
    await row(page, "#1", snaps).waitFor();
    await rowAction(page, "#1", "snapshot-validate", snaps);
    await expectRowStatus(page, "#1", "Validated", snaps);
    await shot(page, "8-admin-regulatory-validated");
  });

  await step(page, "admin: Regulatory — package assembled, approved, submitted to the regulator and acknowledged", async () => {
    await action(page, "package-new");
    const pk = tile(page, "Submission packages");
    await row(page, "#1", pk).waitFor();
    await maybeAction(page, "package-submit", pk);
    await maybeAction(page, "package-approve", pk);
    await action(page, "package-file", pk);
    await action(page, "package-acknowledge", pk);
    await fillForm(page, "package-acknowledge", { reference: `AISHE/C-41873/${YEAR}` });
    await expectRowStatus(page, "#1", "Acknowledged", pk);
    await shot(page, "8-admin-regulatory-acknowledged");
  });

  // ------------------------------------------------------------ 9E: governance + accreditation (integration)
  await step(page, "admin: Reports → Governance — readiness assessment and a campus risk (G00, moved from Campus)", async () => {
    await admin.call("POST", `${C}/institutions/${inst}/governance-cases`, {
      kind: "readiness_assessment",
      title: "Campus readiness for the academic year",
      dueOn: plusDays(30),
    });
    await admin.call("POST", `${C}/institutions/${inst}/governance-cases`, {
      kind: "risk",
      title: "Hostel fire-safety audit overdue",
      severity: "high",
      likelihood: 3,
      impact: 4,
      dueOn: plusDays(14),
    });
    await go(page, "/campus/governance"); // old link → redirects to Reports → Governance
    await page.waitForURL(/\/reports\/governance$/, { timeout: 15000 });
    const cases = tile(page, "Governance cases");
    await lrow(page, "Hostel fire-safety audit overdue", cases).waitFor();
    await lrow(page, "Campus readiness for the academic year", cases).waitFor();
    await lrow(page, "Hostel fire-safety audit overdue", cases).locator("[data-row-open]").click();
    await page.locator("[data-case]").waitFor();
    await shot(page, "9-admin-governance-case-panel");
    await page.keyboard.press("Escape");
    await shot(page, "9-admin-governance");
  });

  await step(page, "admin: Reports → Accreditation — NAAC framework with weighted criteria (H09)", async () => {
    await admin.call("POST", `/api/v1/services/institutions/${inst}/accreditation-frameworks`, {
      code: "NAAC",
      name: "NAAC Revised Accreditation Framework",
      body: "NAAC",
      criteria: [
        { code: "C1", title: "Curricular aspects", weight: 40, metricKeys: [] },
        { code: "C2", title: "Teaching-learning and evaluation", weight: 35, metricKeys: [] },
        { code: "C3", title: "Infrastructure and learning resources", weight: 25, metricKeys: [] },
      ],
    });
    await go(page, "/reports/accreditation");
    const fw = tile(page, "Accreditation frameworks");
    await lrow(page, "NAAC Revised Accreditation Framework", fw).waitFor();
    await shot(page, "9-admin-accreditation");
  });

  // ------------------------------------------------------------ re-skin v2 proof (R7)
  await step(page, "admin: re-skin v2 — Campus overview + every section chip, Reports tabs (≤5 header tabs, 1440×960 full page)", async () => {
    const paths = ["/campus", "/campus/infrastructure", "/campus/labs", "/campus/hostel", "/campus/facilities", "/campus/transport", "/campus/library", "/campus/procurement", "/campus/inventory", "/campus/assets", "/campus/institution", "/campus/readiness", "/campus/campuses", "/campus/organisation", "/campus/positions", "/campus/policies", "/reports", "/reports/dashboards", "/reports/regulatory", "/reports/accreditation", "/reports/governance"];
    for (const p of paths) {
      await go(page, p);
      await page.waitForTimeout(400);
      const tabs = await page.locator("[data-area-head] nav.sb-tabs a, [data-area-head] nav.sb-tabs button").count();
      check(tabs > 0 && tabs <= 5, `${p}: ${tabs} header tabs (≤5)`);
      await page.screenshot({ path: path.join(SHOTS, `rs2-adm-${p.slice(1).replace(/\//g, "_")}.png`), fullPage: true });
    }
    await go(page, "/reports");
    await page.locator("[data-report]").first().click();
    await page.locator("[data-report-dialog]").waitFor();
    await page.screenshot({ path: path.join(SHOTS, "rs2-adm-reports-dlg.png") });
    await page.keyboard.press("Escape");
  });

  // ------------------------------------------------------------ student: library
  await signOut(page);
  await signIn(page, "student");
  await step(page, "student: Library — loans, billed fine and the catalogue", async () => {
    await go(page, "/library");
    const loans = tile(page, "Borrowed");
    await lrow(page, "Higher Engineering Mathematics", loans).waitFor();
    check(((await page.locator('[data-kpi="my-fines"] b').textContent()) ?? "").includes("₹10"), "fine due ₹10");
    await maybeAction(page, "my-renew", lrow(page, "Higher Engineering Mathematics", loans));
    await shot(page, "8-student-library");
  });

  await step(page, "student: Library — reserves a title that is out on loan", async () => {
    const cat = tile(page, "Catalogue");
    await page.locator("[data-library-search] input, input[data-library-search]").first().fill("Algorithms");
    await settle(page);
    await lrow(page, "Introduction to Algorithms", cat).waitFor();
    await action(page, "my-reserve", lrow(page, "Introduction to Algorithms", cat));
    const res = tile(page, "Reservations & fines");
    await expectRowStatus(page, "Introduction to Algorithms", "Queued", res);
    await shot(page, "8-student-library-reserved");
  });

  await step(page, "student: Library in dark mode", async () => {
    await go(page, "/library");
    await display(page, "theme:dark");
    check(await page.evaluate(() => document.documentElement.classList.contains("dk")), "dark mode on <html>");
    await shot(page, "8-student-library-dark");
    await display(page, "theme:light");
  });

  const late = newProblems();
  if (late.length) failed.push(`errors outside the steps:\n    ${late.join("\n    ")}`);
  else log("  ✓ no unexpected console/page/network errors");
} catch (e) {
  failed.push(`setup: ${e instanceof Error ? e.message : e}`);
  console.error("[e2e] FAIL:", e instanceof Error ? e.message : e);
  if (api) console.error(`[e2e] api log tail:\n${api.tail()}`);
} finally {
  await browser?.close().catch(() => {});
  await stop(web);
  await stop(api);
  for (const c of children) await stop(c);
}
if (failed.length) {
  console.error(`[e2e] ${failed.length} step(s) failed:\n  - ${failed.join("\n  - ")}`);
  process.exit(1);
}
log(`PASS — ${shotCount()} screenshots in ${SHOTS}`);
process.exit(0);
