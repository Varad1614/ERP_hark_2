import { Building2, Plus } from "lucide-react";
import { BentoTile, Button, EmptyState, Modal } from "@sutra/ui";
import { useState } from "react";
import { Link } from "react-router";
import type { ActivationDecision, InstitutionCommand } from "./api";
import { QuickForm, REASON, useAsk, type FieldSpec, type RowAction } from "./forms";
import { Actions, CodeName } from "../campus/ui";
import { selectInstitution, StatusBadge, statusLabel, useCommand, useInstitutionApi } from "./hooks";
import type { AcademicPattern, Institution, InstitutionType } from "./types";

const TIMEZONES = ["Asia/Kolkata", "Asia/Dubai", "Asia/Singapore", "Europe/London", "America/New_York", "UTC"];

/** Lifecycle actions offered in each status (contract §4.2; the API enforces them). */
export function lifecycleActions(status: Institution["status"]): (InstitutionCommand | `decide:${ActivationDecision}`)[] {
  switch (status) {
    case "draft":
    case "returned":
    case "rejected":
      return ["configure"];
    case "configured":
      return ["submit"];
    case "pending_approval":
      return ["decide:approve", "decide:return", "decide:reject"];
    case "active":
      return ["suspend", "archive"];
    case "suspended":
      return ["reactivate", "archive"];
    default:
      return [];
  }
}

const LABELS: Record<string, string> = {
  configure: "Mark configured",
  submit: "Submit for activation",
  "decide:approve": "Approve activation",
  "decide:return": "Return for correction",
  "decide:reject": "Reject activation",
  suspend: "Suspend",
  reactivate: "Restore",
  archive: "Archive",
};

/** "Register institution" — replaces the browser-console POST of the Batch 1 checklist. */
export function RegisterInstitution({ onDone }: { onDone: () => void }) {
  const api = useInstitutionApi();
  const { submit } = useCommand();
  return (
    <>
      <h2>Register institution</h2>
      <QuickForm
        name="register-institution"
        submitLabel="Register"
        onCancel={onDone}
        fields={[
          { name: "code", label: "Code", required: true, placeholder: "SIT" },
          { name: "name", label: "Name", required: true, placeholder: "Sutra Institute of Technology", wide: true },
          { name: "timezone", label: "Business timezone", type: "select", initial: "Asia/Kolkata", options: TIMEZONES.map((t) => ({ value: t, label: t })) },
        ]}
        onSubmit={async (v) => {
          const inst = await api.registerInstitution({ code: v.code!.trim().toUpperCase(), name: v.name!.trim(), timezone: v.timezone });
          selectInstitution(inst.id);
          await submit(async () => inst, `Registered ${inst.name}`);
          onDone();
        }}
      />
    </>
  );
}

const INSTITUTION_TYPES: InstitutionType[] = ["college", "university", "autonomous", "affiliated", "school", "other"];
const PATTERNS: AcademicPattern[] = ["semester", "trimester", "annual", "term"];
/** Commands whose reason is required by the API. */
const NEEDS_REASON = new Set(["suspend", "reactivate", "archive", "decide:return", "decide:reject"]);

/** Profile fields (all required for `configure` except website / affiliated university). */
export function profileFields(i: Institution): FieldSpec[] {
  const p = i.profile;
  return [
    { name: "shortName", label: "Short name", required: true, initial: p.shortName ?? "" },
    { name: "institutionType", label: "Institution type", type: "select", initial: p.institutionType ?? "college", options: INSTITUTION_TYPES.map((t) => ({ value: t, label: t })) },
    { name: "affiliatedUniversity", label: "Affiliated university (if affiliated)", initial: p.affiliatedUniversity ?? "" },
    { name: "registrationNumber", label: "Registration number", required: true, initial: p.registrationNumber ?? "" },
    { name: "address", label: "Address", required: true, wide: true, initial: p.address ?? "" },
    { name: "contactPhone", label: "Contact phone", required: true, initial: p.contactPhone ?? "" },
    { name: "officialEmail", label: "Official e-mail", required: true, initial: p.officialEmail ?? "" },
    { name: "administratorEmail", label: "Administrator e-mail", required: true, initial: p.administratorEmail ?? "" },
    { name: "academicPattern", label: "Academic pattern", type: "select", initial: p.academicPattern ?? "semester", options: PATTERNS.map((t) => ({ value: t, label: t })) },
    { name: "workingDays", label: "Working days", required: true, initial: (p.workingDays.length ? p.workingDays : ["mon", "tue", "wed", "thu", "fri"]).join(", ") },
    { name: "defaultLanguage", label: "Default language", initial: p.defaultLanguage ?? "en-IN" },
    { name: "website", label: "Website", initial: p.website ?? "" },
  ];
}

export function InstitutionTab({ institution }: { institution: Institution | null }) {
  const api = useInstitutionApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [registering, setRegistering] = useState(false);
  const [renaming, setRenaming] = useState(false);
  const editProfile = (inst: Institution) =>
    ask({
      title: "Institution profile",
      name: "institution-profile",
      submitLabel: "Save profile",
      fields: profileFields(inst),
      onSubmit: (v) =>
        submit(
          () =>
            api.reviseProfile(inst.id, inst.version, {
              shortName: v.shortName!.trim(),
              institutionType: v.institutionType as InstitutionType,
              affiliatedUniversity: v.affiliatedUniversity?.trim() || null,
              registrationNumber: v.registrationNumber!.trim(),
              address: v.address!.trim(),
              contactPhone: v.contactPhone!.trim(),
              officialEmail: v.officialEmail!.trim(),
              administratorEmail: v.administratorEmail!.trim(),
              academicPattern: v.academicPattern as AcademicPattern,
              workingDays: v.workingDays!.split(",").map((d) => d.trim().toLowerCase().slice(0, 3)).filter(Boolean),
              defaultLanguage: v.defaultLanguage?.trim() || null,
              website: v.website?.trim() || null,
            }),
          "Profile saved",
        ),
    });
  const register = (
    <Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={() => setRegistering(true)} data-register-institution>
      Register institution
    </Button>
  );

  const act = (inst: Institution, a: InstitutionCommand | `decide:${ActivationDecision}`) => {
    const label = LABELS[a] ?? a;
    const call = (reason?: string) => () => (a.startsWith("decide:") ? api.decideActivation(inst.id, inst.version, a.slice(7) as ActivationDecision, reason) : api.institutionCommand(inst.id, inst.version, a as InstitutionCommand, reason));
    const done = (r: Institution) => `${label}: ${r.name} is now ${r.status.replace(/_/g, " ")}`;
    if (NEEDS_REASON.has(a)) return ask({ title: label, name: `institution-${a.replace(":", "-")}`, submitLabel: label, fields: [REASON], onSubmit: (v) => submit(call(v.reason!.trim()), done) });
    void run(call(), done);
  };

  return (
    <>
      <BentoTile span={12} title="Institution profile" action={register}>
        {!institution ? (
          <EmptyState icon={Building2} title="No institution yet">
            Register your institution to start setup: organisation, campuses, programmes and the academic calendar.
          </EmptyState>
        ) : (
          <div data-institution-profile={institution.id}>
            <dl className="sb-dl">
              <dt>Name</dt>
              <dd>
                <CodeName name={institution.name} code={institution.code} />
              </dd>
              <dt>Business timezone</dt>
              <dd>{institution.timezone}</dd>
              <dt>Status</dt>
              <dd data-institution-status={institution.status}>
                <StatusBadge status={institution.status} />
              </dd>
              <dt>Profile</dt>
              <dd data-profile-revision={institution.profile.revision}>
                {institution.profile.revision > 0 ? [institution.profile.shortName, institution.profile.institutionType ? statusLabel(institution.profile.institutionType) : null, `revision ${institution.profile.revision}`].filter(Boolean).join(" · ") : "Not filled in yet"}
              </dd>
            </dl>
            {/* One primary (the next lifecycle step), Edit profile as the ghost companion, the rest in the kebab. */}
            <div className="sb-actions-row" data-institution-actions="">
              {institution.status !== "archived" && (
                <Button size="sm" variant="ghost" onClick={() => editProfile(institution)} disabled={busy} data-action="profile">
                  Edit profile
                </Button>
              )}
              <Actions
                disabled={busy}
                fallbackPrimary={false}
                actions={[
                  ...lifecycleActions(institution.status).map((a): RowAction => ({ id: a, label: LABELS[a] ?? a, onClick: () => act(institution, a), variant: a === "submit" || a === "decide:approve" || a === "configure" || a === "reactivate" ? "primary" : "ghost" })),
                  ...(institution.status !== "archived" ? [{ id: "rename", label: "Rename", onClick: () => setRenaming(true) }] : []),
                ]}
              />
            </div>
            {institution.status === "draft" && (
              <p>
                <small>Fill in the profile, then mark the institution configured.</small>
              </p>
            )}
            {institution.status === "configured" && (
              <p>
                <small>
                  Submitting checks the <Link to="/campus/readiness">activation readiness</Link> checklist first.
                </small>
              </p>
            )}
            {(institution.status === "suspended" || institution.status === "archived") && (
              <p>
                <small>While {institution.status}, campuses, organisation, programmes and calendars are read-only.</small>
              </p>
            )}
          </div>
        )}
      </BentoTile>
      <Modal open={registering} onClose={() => setRegistering(false)} label="Register institution">
        <RegisterInstitution onDone={() => setRegistering(false)} />
      </Modal>
      {institution && (
        <Modal open={renaming} onClose={() => setRenaming(false)} label="Rename institution">
          <h2>Rename institution</h2>
          <QuickForm
            name="rename-institution"
            submitLabel="Save"
            onCancel={() => setRenaming(false)}
            fields={[{ name: "name", label: "Name", required: true, initial: institution.name, wide: true }]}
            onSubmit={async (v) => {
              await submit(() => api.renameInstitution(institution.id, institution.version, v.name!.trim()), "Saved");
              setRenaming(false);
            }}
          />
        </Modal>
      )}
      {prompt}
      {dialog}
    </>
  );
}
