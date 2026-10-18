import { describe, expect, it } from "vitest";
import { CATALOG } from "./api";
import { defaultEntitlement, isModuleActive, licenceBanner, moduleForRoute, moduleForWorkspace, REGISTRY, visibleNav, visibleWorkspaces, type EffectiveEntitlement } from "./entitlements";
import { ROLE_CONFIG } from "./roles";

const pkg = (key: string) => REGISTRY.packages.find((p) => p.key === key)!.modules;
const ent = (modules: string[], over: Partial<EffectiveEntitlement> = {}): EffectiveEntitlement => ({ ...defaultEntitlement(), modules, ...over });
const ids = (nav: { id: string }[]) => nav.map((n) => n.id);

describe("entitlement registry (shared catalog/entitlements.json)", () => {
  it("has the fifteen modules and four tiered packages", () => {
    expect(REGISTRY.modules.map((m) => m.key)).toHaveLength(15);
    expect(REGISTRY.packages.map((p) => p.key)).toEqual(["core_academic", "academic_plus", "institution_operations", "complete_erp"]);
    expect(pkg("complete_erp")).toHaveLength(15);
    expect(REGISTRY.modules.find((m) => m.key === "payroll")?.requires).toEqual(["hr"]);
    expect(defaultEntitlement().package).toBe("complete_erp");
  });

  it("maps routes and workspaces onto modules (core = null)", () => {
    expect(moduleForRoute("/finance/fees")).toBe("fees");
    expect(moduleForRoute("/fees")).toBe("fees");
    expect(moduleForRoute("/people")).toBe("hr");
    expect(moduleForRoute("/attendance")).toBe("attendance");
    expect(moduleForRoute("/library")).toBe("library");
    expect(moduleForRoute("/reports/dashboards")).toBe("reporting");
    expect(moduleForRoute("/campus/hostel")).toBeNull();
    expect(moduleForRoute("/")).toBeNull();
    expect(moduleForRoute("/profile")).toBeNull();
    expect(moduleForRoute("/workspaces/W-E06")).toBe("payroll");
    expect(moduleForWorkspace("W-E03")).toBe("hr");
    expect(moduleForWorkspace("W-C01")).toBe("admissions");
    expect(moduleForWorkspace("W-C04")).toBe("students");
    expect(moduleForWorkspace("W-J10")).toBe("assets");
    expect(moduleForWorkspace("W-J03")).toBe("procurement");
    expect(moduleForWorkspace("W-A01")).toBeNull();
    expect(moduleForWorkspace("W-G01")).toBeNull();
  });
});

describe("navigation filtering", () => {
  it("keeps everything on complete_erp and while the entitlement is loading", () => {
    for (const r of ["administrator", "faculty", "student"] as const) {
      expect(visibleNav(ROLE_CONFIG[r].nav, ent(pkg("complete_erp")))).toHaveLength(ROLE_CONFIG[r].nav.length);
      expect(visibleNav(ROLE_CONFIG[r].nav, undefined)).toHaveLength(ROLE_CONFIG[r].nav.length);
    }
    expect(isModuleActive(undefined, "payroll")).toBe(true);
    expect(isModuleActive(ent([]), null)).toBe(true);
  });

  it("hides the pills of inactive modules on core_academic", () => {
    const core = ent(pkg("core_academic"));
    expect(ids(visibleNav(ROLE_CONFIG.administrator.nav, core))).toEqual(["home", "students", "academics", "examinations", "campus", "workspaces"]);
    expect(ids(visibleNav(ROLE_CONFIG.student.nav, core))).toEqual(["home", "timetable", "courses", "assignments", "results", "documents", "workspaces"]);
    expect(ids(visibleNav(ROLE_CONFIG.faculty.nav, core))).toEqual(["home", "classes", "attendance", "assessments", "my-students", "timetable", "resources", "workspaces"]);
    const ops = ent(pkg("institution_operations"));
    expect(ids(visibleNav(ROLE_CONFIG.administrator.nav, ops))).toContain("people");
    expect(ids(visibleNav(ROLE_CONFIG.student.nav, ops))).toContain("library");
  });

  it("filters palette workspaces by module", () => {
    const core = ent(pkg("core_academic"));
    const keys = visibleWorkspaces(CATALOG, core).map((w) => w.key);
    expect(keys).toContain("W-C01");
    expect(keys).toContain("W-I02");
    expect(keys).toContain("W-A01");
    expect(keys).toContain("W-G01");
    expect(keys).not.toContain("W-E06");
    expect(keys).not.toContain("W-F01");
    expect(keys).not.toContain("W-G03");
    expect(keys).not.toContain("W-J08");
    expect(visibleWorkspaces(CATALOG, ent(pkg("complete_erp")))).toHaveLength(CATALOG.length);
  });
});

describe("licence banner", () => {
  it("is quiet for an active perpetual licence and warns 30 days ahead", () => {
    expect(licenceBanner(defaultEntitlement())).toBeNull();
    expect(licenceBanner(undefined)).toBeNull();
    expect(licenceBanner(ent([], { daysRemaining: 90, validUntil: "2027-03-31T00:00:00Z" }))).toBeNull();
    expect(licenceBanner(ent([], { daysRemaining: 12, validUntil: "2027-03-31T00:00:00Z" }))?.status).toBe("renewal_due");
  });

  it("explains grace, expiry (read-only + export) and suspension", () => {
    const g = licenceBanner(ent([], { status: "grace", daysRemaining: 9, validUntil: "2027-03-31T00:00:00Z", graceEndsAt: "2027-04-15T00:00:00Z" }))!;
    expect(g.title).toBe("Licence renewal due");
    expect(g.detail).toContain("9 days");
    const e = licenceBanner(ent([], { status: "expired", readOnly: true }))!;
    expect(e.title).toContain("read-only");
    expect(e.detail).toContain("export");
    expect(licenceBanner(ent([], { status: "suspended", readOnly: true }))?.status).toBe("suspended");
  });
});
