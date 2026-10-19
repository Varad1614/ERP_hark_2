/**
 * In-memory stand-in of the slice-9E governance API (mock mode and tests): an open readiness
 * assessment with rated criteria, a draft operational risk, a closed operational review whose
 * evidence is still a draft, and one effective `readiness_threshold` configuration. Legal
 * transitions only (409 otherwise), mirroring the API's state machines (`draft → open →
 * pending_decision → closed`, auto-approval closes at once).
 */
import { ApiProblem } from "@sutra/api-client";
import type { GovernanceApi } from "./api";
import { itemKindsFor, OUTCOMES } from "./rules";
import type { GovernanceCase, GovernanceConfigVersion, GovernanceEvidence, GovernanceItem, GovernanceKind } from "./types";

const conflict = (detail: string) => new ApiProblem({ type: "about:blank", title: "Conflict", status: 409, errorCode: "conflict", detail });
const notFound = () => new ApiProblem({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" });
const invalid = (detail: string) => new ApiProblem({ type: "about:blank", title: "Unprocessable", status: 422, errorCode: "validation-failed", detail });
const stale = () => new ApiProblem({ type: "about:blank", title: "Precondition failed", status: 412, errorCode: "precondition-failed" });

const INST = "00000000-0000-7000-8000-0000000b9e01";
let seq = 900;
const uid = () => `00000000-0000-7000-8000-${String(++seq).padStart(12, "0")}`;
const pad = (n: number) => String(n).padStart(6, "0");
/** Free-form settings JSON as the generated view types it. */
const asSettings = (s: Record<string, unknown>) => s as unknown as GovernanceConfigVersion["settings"];

export function createMockGovernanceApi(): GovernanceApi {
  let caseNo = 0;
  const item = (caseId: string, kind: GovernanceItem["kind"], title: string, extra: Partial<GovernanceItem> = {}): GovernanceItem => ({
    id: uid(),
    caseId,
    kind,
    title,
    status: "open",
    version: 1,
    ...extra,
  });
  const newCase = (kind: GovernanceKind, title: string, extra: Partial<GovernanceCase> = {}): GovernanceCase => {
    const id = uid();
    return {
      id,
      institutionId: INST,
      number: `GOV-${pad(++caseNo)}`,
      kind,
      title,
      domains: [],
      severity: "medium",
      facts: {},
      status: "draft",
      approvalState: "not_requested",
      allowedOutcomes: OUTCOMES[kind],
      items: [],
      version: 1,
      ...extra,
    };
  };

  const readiness = newCase("readiness_assessment", "Campus readiness for NAAC cycle", { status: "open", domains: ["facility", "library"], version: 3 });
  readiness.items = [
    item(readiness.id, "criterion", "Infrastructure adequacy", { status: "done", score: 4, version: 2 }),
    item(readiness.id, "criterion", "Library resources", { status: "in_progress", score: 3, version: 2 }),
  ];
  readiness.averageScore = 3.5;
  const risk = newCase("risk", "Hostel fire-safety audit overdue", { domains: ["hostel"], severity: "high", likelihood: 3, impact: 4, inherentScore: 12 });
  const review = newCase("operational_review", "Quarterly campus operations review", { status: "closed", approvalState: "approved", outcome: "closed_satisfactory", domains: ["maintenance", "transport"], version: 5 });
  const cases: GovernanceCase[] = [readiness, risk, review];

  const configs: GovernanceConfigVersion[] = [
    { id: uid(), institutionId: INST, key: "readiness_threshold", versionNo: 1, settings: asSettings({ minCriterionScore: 3 }), status: "effective", approvalState: "approved", effectiveAt: "2026-09-01T00:00:00Z", version: 3 },
  ];
  const evidence: GovernanceEvidence[] = [
    { id: uid(), institutionId: INST, caseId: review.id, caseNumber: review.number, caseKind: review.kind, caseOutcome: review.outcome, title: "Operations review Q2 evidence", facts: {}, status: "draft", accreditationReady: false, version: 1 },
  ];

  const findCase = (id: string, version?: number) => {
    const c = cases.find((x) => x.id === id);
    if (!c) throw notFound();
    if (version !== undefined && c.version !== version) throw stale();
    return c;
  };
  const touch = (c: GovernanceCase, patch: Partial<GovernanceCase>) => {
    Object.assign(c, patch, { version: c.version + 1 });
    return { ...c, items: [...c.items] };
  };
  const findItem = (id: string, version: number) => {
    for (const c of cases) {
      const it = c.items.find((i) => i.id === id);
      if (it) {
        if (it.version !== version) throw stale();
        return { c, it };
      }
    }
    throw notFound();
  };
  const score = (c: GovernanceCase) => {
    const s = c.items.map((i) => i.score).filter((x): x is number => typeof x === "number");
    return s.length ? s.reduce((a, b) => a + b, 0) / s.length : null;
  };

  return {
    cases: async () => cases.map((c) => ({ ...c, items: [...c.items] })),
    createCase: async (_inst, input) => {
      if (!input.title.trim()) throw invalid("title is required");
      const c = newCase(input.kind, input.title, {
        domains: input.domains ?? [],
        severity: input.severity ?? "medium",
        owner: input.owner ?? null,
        dueOn: input.dueOn ?? null,
        description: input.description ?? null,
        likelihood: input.likelihood ?? null,
        impact: input.impact ?? null,
        inherentScore: input.likelihood && input.impact ? input.likelihood * input.impact : null,
        exceptionType: input.exceptionType ?? null,
        expiresOn: input.expiresOn ?? null,
      });
      cases.unshift(c);
      return { ...c };
    },
    caseCommand: async (id, version, cmd, reason) => {
      const c = findCase(id, version);
      if (cmd === "start") {
        if (c.status !== "draft") throw conflict("only a draft case is started");
        return touch(c, { status: "open" });
      }
      if (cmd === "refresh") {
        if (c.status !== "draft" && c.status !== "open") throw conflict("only a draft or open case is refreshed");
        return touch(c, {});
      }
      if (c.status !== "draft" && c.status !== "open") throw conflict("only a draft or open case is cancelled");
      if (!reason?.trim()) throw invalid("reason is required");
      return touch(c, { status: "cancelled", statusReason: reason });
    },
    submitCase: async (id, version, outcome, rationale, residualScore) => {
      const c = findCase(id, version);
      if (c.status !== "open") throw conflict("only an open case is submitted");
      if (!c.allowedOutcomes.includes(outcome)) throw invalid(`outcome must be one of ${c.allowedOutcomes.join(", ")}`);
      const verifiedOnly = c.kind === "action_plan" || c.kind === "finding";
      const unfinished = c.items.filter((i) => !(i.status === "verified" || i.status === "cancelled" || (!verifiedOnly && i.status === "done")));
      if ((c.kind === "action_plan" || c.kind === "finding") && unfinished.length) throw invalid(`${unfinished.length} item(s) not verified`);
      // Auto-approval (the noop workflow adapter): the case closes at once.
      return touch(c, { status: "closed", approvalState: "approved", outcome, rationale: rationale ?? null, residualScore: residualScore ?? c.residualScore ?? null });
    },
    decideCase: async (id, version, decision, reason) => {
      const c = findCase(id, version);
      if (c.status !== "pending_decision") throw conflict("nothing to decide");
      if (decision === "reject") {
        if (!reason?.trim()) throw invalid("reason is required");
        return touch(c, { status: "open", approvalState: "rejected", outcome: null, statusReason: reason });
      }
      return touch(c, { status: "closed", approvalState: "approved" });
    },
    escalate: async (id, title, reason) => {
      const parent = findCase(id);
      if (parent.status !== "open" && parent.status !== "pending_decision") throw conflict("only an open or pending case is escalated");
      const e = newCase("escalation", title, { status: "open", parentId: parent.id, severity: parent.severity === "critical" ? "critical" : "high", description: reason, domains: parent.domains });
      cases.unshift(e);
      return { ...e };
    },
    addItem: async (caseId, input) => {
      const c = findCase(caseId);
      if (c.status !== "draft" && c.status !== "open") throw conflict("items are added to a draft or open case");
      if (!itemKindsFor(c.kind).includes(input.kind)) throw invalid(`a ${c.kind} takes ${itemKindsFor(c.kind).join(" / ")} items`);
      c.items.push(item(c.id, input.kind, input.title, { owner: input.owner ?? null, dueOn: input.dueOn ?? null, domain: input.domain ?? null, score: input.score ?? null }));
      return touch(c, { averageScore: score(c) });
    },
    progressItem: async (itemId, version, input) => {
      const { c, it } = findItem(itemId, version);
      if (input.status) {
        const ok: Record<string, string[]> = { open: ["in_progress", "done", "cancelled"], in_progress: ["done", "cancelled"], done: ["verified", "in_progress"] };
        if (!(ok[it.status] ?? []).includes(input.status)) throw conflict(`${it.status} → ${input.status} is not allowed`);
        it.status = input.status;
      }
      if (input.score !== undefined && input.score !== null) it.score = input.score;
      if (input.evidenceRef) it.evidenceRef = input.evidenceRef;
      it.version += 1;
      return touch(c, { averageScore: score(c) });
    },
    verifyItem: async (itemId, version, evidenceRef) => {
      const { c, it } = findItem(itemId, version);
      if (it.status !== "done") throw conflict("only a done item is verified");
      const ref = evidenceRef || it.evidenceRef;
      if (!ref) throw invalid("an evidence reference is required");
      Object.assign(it, { status: "verified", evidenceRef: ref, verifiedBy: "mock-admin", version: it.version + 1 });
      return touch(c, {});
    },
    configs: async () => [...configs],
    createConfig: async (_inst, key, settings) => {
      const prev = configs.filter((c) => c.key === key);
      const c: GovernanceConfigVersion = { id: uid(), institutionId: INST, key, versionNo: prev.length + 1, settings: asSettings(settings), status: "draft", approvalState: "not_requested", predecessorId: prev.find((p) => p.status === "effective")?.id ?? null, version: 1 };
      configs.unshift(c);
      return { ...c };
    },
    submitConfig: async (id, version) => {
      const c = configs.find((x) => x.id === id);
      if (!c) throw notFound();
      if (c.version !== version) throw stale();
      if (c.status !== "draft" || c.approvalState === "pending") throw conflict("only a draft version is submitted");
      for (const p of configs) if (p.key === c.key && p.status === "effective") Object.assign(p, { status: "superseded", version: p.version + 1 });
      Object.assign(c, { status: "effective", approvalState: "approved", effectiveAt: new Date().toISOString(), version: c.version + 1 });
      return { ...c };
    },
    decideConfig: async (id, version, decision, reason) => {
      const c = configs.find((x) => x.id === id);
      if (!c) throw notFound();
      if (c.version !== version) throw stale();
      if (c.approvalState !== "pending") throw conflict("nothing to decide");
      if (decision === "reject") Object.assign(c, { approvalState: "rejected", statusReason: reason ?? null, version: c.version + 1 });
      else Object.assign(c, { status: "effective", approvalState: "approved", version: c.version + 1 });
      return { ...c };
    },
    policy: async () => {
      const eff = configs.filter((c) => c.status === "effective");
      const min = eff.find((c) => c.key === "readiness_threshold")?.settings as { minCriterionScore?: number } | undefined;
      const max = eff.find((c) => c.key === "risk_appetite")?.settings as { maxResidualScore?: number } | undefined;
      return { policy: { minCriterionScore: min?.minCriterionScore ?? 3, maxResidualScore: max?.maxResidualScore ?? 6 }, effective: eff };
    },
    evidence: async () => [...evidence],
    createEvidence: async (_inst, caseId, title, summary) => {
      const c = findCase(caseId);
      if (c.status !== "closed") throw conflict(`${c.number} is ${c.status}, not closed`);
      const e: GovernanceEvidence = { id: uid(), institutionId: INST, caseId, caseNumber: c.number, caseKind: c.kind, caseOutcome: c.outcome ?? null, title: title || `${c.number} evidence`, summary: summary ?? null, facts: {}, status: "draft", accreditationReady: false, version: 1 };
      evidence.unshift(e);
      return { ...e };
    },
    publishEvidence: async (id, version) => {
      const e = evidence.find((x) => x.id === id);
      if (!e) throw notFound();
      if (e.version !== version) throw stale();
      if (e.status !== "draft") throw conflict("already published");
      Object.assign(e, { status: "published", accreditationReady: true, publishedAt: new Date().toISOString(), version: e.version + 1 });
      return { ...e };
    },
  };
}
