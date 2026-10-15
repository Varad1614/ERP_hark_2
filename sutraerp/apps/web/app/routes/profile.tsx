import { LogOut, ShieldCheck } from "lucide-react";
import { ApiProblem, type CategoryPreference, type Profile, type UpdateProfile } from "@sutra/api-client";
import { Badge, BentoGrid, BentoTile, Button, Checkbox, FormGrid, FormSelect, Initials, TextField, useToast } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useEffect, useState, type FormEvent } from "react";
import { useNavigate } from "react-router";
import { useAuth, useSession } from "../lib/auth";
import "../components/shell.css";
import { inboxKeys, useInbox, useInboxRefresh } from "../lib/inbox";
import { ROLE_CONFIG, roleTitle } from "../lib/roles";
import { CATEGORY_META } from "../components/NotificationRow";

export function meta() {
  return [{ title: "My profile · SutraERP" }];
}

const LOCALES = [
  { value: "en-IN", label: "English (India)" },
  { value: "hi-IN", label: "हिन्दी (भारत)" },
  { value: "mr-IN", label: "मराठी (भारत)" },
  { value: "en-GB", label: "English (UK)" },
];
const ZONES = ["Asia/Kolkata", "UTC", "Asia/Dubai", "Asia/Singapore", "Europe/London", "America/New_York"].map((z) => ({ value: z, label: z }));

export interface ProfileDraft {
  displayName: string;
  locale: string;
  timeZone: string;
  contactEmail: string;
  contactPhone: string;
}

/** Client-side checks mirroring the API (which stays authoritative). */
export function validateProfile(d: ProfileDraft): Record<string, string> {
  const e: Record<string, string> = {};
  if (!d.displayName.trim()) e.displayName = "Your name is required";
  else if (d.displayName.trim().length > 120) e.displayName = "At most 120 characters";
  if (d.contactEmail.trim() && !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(d.contactEmail.trim())) e.contactEmail = "Enter an e-mail address";
  if (d.contactPhone.trim() && !/^\+[1-9]\d{6,14}$/.test(d.contactPhone.trim())) e.contactPhone = "Use international format, e.g. +919876543210";
  return e;
}

/** Only the members that changed (empty contact fields clear them). */
export function profilePatch(p: Profile, d: ProfileDraft, prefs: CategoryPreference[]): UpdateProfile {
  const patch: UpdateProfile = {};
  if (d.displayName.trim() !== p.displayName) patch.displayName = d.displayName.trim();
  if (d.locale !== p.locale) patch.locale = d.locale;
  if (d.timeZone !== p.timeZone) patch.timeZone = d.timeZone;
  if (d.contactEmail.trim() !== (p.contactEmail ?? "")) patch.contactEmail = d.contactEmail.trim();
  if (d.contactPhone.trim() !== (p.contactPhone ?? "")) patch.contactPhone = d.contactPhone.trim();
  const changed = prefs.filter((x) => {
    const old = p.notificationPreferences.find((o) => o.category === x.category);
    return !old || old.inApp !== x.inApp || old.email !== x.email;
  });
  if (changed.length) patch.notificationPreferences = changed;
  return patch;
}

function draftOf(p: Profile): ProfileDraft {
  return { displayName: p.displayName, locale: p.locale, timeZone: p.timeZone, contactEmail: p.contactEmail ?? "", contactPhone: p.contactPhone ?? "" };
}

/** Self-service profile: name, language, time zone, contact details, notification preferences. */
export default function ProfilePage() {
  const { role } = useSession();
  const inbox = useInbox();
  const refresh = useInboxRefresh();
  const toast = useToast();
  const navigate = useNavigate();
  const q = useQuery({ queryKey: inboxKeys.profile(role), queryFn: () => inbox.profile() });
  const [draft, setDraft] = useState<ProfileDraft | null>(null);
  const [prefs, setPrefs] = useState<CategoryPreference[]>([]);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(false);
  const { signOut } = useAuth();

  useEffect(() => {
    if (q.data) {
      setDraft(draftOf(q.data));
      setPrefs(q.data.notificationPreferences);
    }
  }, [q.data]);

  if (q.isError) {
    return (
      <BentoGrid data-screen="profile">
        <BentoTile span={12} title="My profile">
          <p className="sb-field-error">{q.error instanceof Error ? q.error.message : "Profile unavailable"}</p>
        </BentoTile>
      </BentoGrid>
    );
  }
  if (!q.data || !draft) return <BentoGrid data-screen="profile"><BentoTile span={12}><div className="sb-skeleton" /></BentoTile></BentoGrid>;
  const profile = q.data;
  const set = (k: keyof ProfileDraft) => (e: { target: { value: string } }) => setDraft({ ...draft, [k]: e.target.value });

  const save = async (e: FormEvent) => {
    e.preventDefault();
    const v = validateProfile(draft);
    setErrors(v);
    if (Object.keys(v).length) return;
    const patch = profilePatch(profile, draft, prefs);
    if (Object.keys(patch).length === 0) {
      toast("Nothing to save");
      return;
    }
    setBusy(true);
    try {
      await inbox.updateProfile(profile.version, patch);
      await refresh();
      setEditing(false);
      toast("Profile saved");
    } catch (err) {
      if (err instanceof ApiProblem && err.problem.validationErrors?.length) {
        setErrors(Object.fromEntries(err.problem.validationErrors.map((x) => [x.field, x.message])));
      } else if (err instanceof ApiProblem && err.problem.status === 412) {
        toast("Your profile changed in another tab — reloaded the latest version.");
        await refresh();
      } else toast(err instanceof Error ? err.message : "Could not save");
    } finally {
      setBusy(false);
    }
  };

  const savePrefs = async () => {
    const patch = profilePatch(profile, draftOf(profile), prefs);
    if (!patch.notificationPreferences) {
      toast("Nothing to save");
      return;
    }
    setBusy(true);
    try {
      await inbox.updateProfile(profile.version, { notificationPreferences: patch.notificationPreferences });
      await refresh();
      toast("Preferences saved");
    } catch (err) {
      if (err instanceof ApiProblem && err.problem.status === 412) {
        toast("Your profile changed in another tab — reloaded the latest version.");
        await refresh();
      } else toast(err instanceof Error ? err.message : "Could not save");
    } finally {
      setBusy(false);
    }
  };
  const roleTitles = profile.roles.map((r) => roleTitle(r)).join(", ");
  const localeLabel = LOCALES.find((l) => l.value === profile.locale)?.label ?? profile.locale;

  return (
    <BentoGrid data-screen="profile">
      <BentoTile span={12} className="pf-head" data-area-head="">
        <Initials name={profile.displayName} size="xl" tone="accent" />
        <div className="pf-who">
          <Badge kind="neutral">{roleTitles || ROLE_CONFIG[role].title}</Badge>
          <h1>{profile.displayName}</h1>
          <p data-area-summary="">
            {profile.loginId} · Sutra Institute of Technology
          </p>
        </div>
        <div className="sb-area-side">
          <Button variant="ghost" icon={<ShieldCheck strokeWidth={1.75} />} iconPosition="start" onClick={() => navigate("/account/security")} data-profile-security>
            Security & sign-in
          </Button>
          <Button
            variant="ghost"
            icon={<LogOut strokeWidth={1.75} />}
            iconPosition="start"
            onClick={async () => {
              await signOut();
              navigate("/sign-in", { replace: true });
            }}
            data-profile-sign-out
          >
            Sign out
          </Button>
        </div>
      </BentoTile>
      <BentoTile
        span={8}
        title="Details"
        action={
          !editing && (
            <button type="button" onClick={() => setEditing(true)} data-edit-profile>
              Edit
            </button>
          )
        }
      >
        {editing ? (
          <form onSubmit={save} noValidate data-profile-form>
            <FormGrid columns={2}>
              <TextField label="Display name" name="displayName" value={draft.displayName} maxLength={120} error={errors.displayName} onChange={set("displayName")} />
              <FormSelect label="Language" name="locale" options={LOCALES.some((l) => l.value === draft.locale) ? LOCALES : [...LOCALES, { value: draft.locale, label: draft.locale }]} value={draft.locale} error={errors.locale} onChange={set("locale")} />
              <FormSelect label="Time zone" name="timeZone" options={ZONES.some((z) => z.value === draft.timeZone) ? ZONES : [...ZONES, { value: draft.timeZone, label: draft.timeZone }]} value={draft.timeZone} error={errors.timeZone} onChange={set("timeZone")} />
              <TextField label="Contact e-mail" name="contactEmail" type="email" value={draft.contactEmail} error={errors.contactEmail} onChange={set("contactEmail")} />
              <TextField label="Mobile" name="contactPhone" type="tel" placeholder="+919876543210" value={draft.contactPhone} error={errors.contactPhone} onChange={set("contactPhone")} />
            </FormGrid>
            <div className="sb-actions-row">
              <Button type="submit" disabled={busy} data-save-profile>
                {busy ? "Saving…" : "Save changes"}
              </Button>
              <Button
                variant="ghost"
                disabled={busy}
                onClick={() => {
                  setDraft(draftOf(profile));
                  setErrors({});
                  setEditing(false);
                }}
              >
                Cancel
              </Button>
            </div>
          </form>
        ) : (
          <dl className="sb-dl pf-facts" data-profile-facts>
            <dt>Login</dt>
            <dd>{profile.loginId}</dd>
            <dt>Display name</dt>
            <dd>{profile.displayName}</dd>
            <dt>Email</dt>
            <dd>{profile.contactEmail || "—"}</dd>
            <dt>Mobile</dt>
            <dd>{profile.contactPhone || "—"}</dd>
            <dt>Language</dt>
            <dd>{localeLabel}</dd>
            <dt>Time zone</dt>
            <dd>{profile.timeZone}</dd>
            <dt>Roles</dt>
            <dd>{roleTitles || "—"}</dd>
          </dl>
        )}
      </BentoTile>
      <BentoTile span={4} title="Preferences" className="pf-prefs">
        <div data-prefs>
          {prefs.map((p) => (
            <div key={p.category} className="pf-pref">
              <b>{CATEGORY_META[p.category].label}</b>
              <Checkbox
                label="In app"
                aria-label={`${CATEGORY_META[p.category].label} in app`}
                checked={p.inApp}
                disabled={p.category === "security"}
                onChange={(e) => setPrefs((xs) => xs.map((x) => (x.category === p.category ? { ...x, inApp: e.target.checked } : x)))}
              />
              <Checkbox
                label="E-mail"
                aria-label={`${CATEGORY_META[p.category].label} by e-mail`}
                checked={p.email}
                onChange={(e) => setPrefs((xs) => xs.map((x) => (x.category === p.category ? { ...x, email: e.target.checked } : x)))}
              />
            </div>
          ))}
        </div>
        {errors.notificationPreferences && <p className="sb-field-error">{errors.notificationPreferences}</p>}
        <div className="sb-actions-row">
          <Button size="sm" disabled={busy} onClick={savePrefs} data-save-prefs>
            Save preferences
          </Button>
        </div>
      </BentoTile>
    </BentoGrid>
  );
}
