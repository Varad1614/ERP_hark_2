/**
 * React glue of the Batch 2 admin screens: the API instance (context, mock or HTTP), the
 * selected institution, query keys and `useCommand` — runs a mutation, shows problem+json
 * messages, opens the shared step-up dialog on `mfa-step-up-required` and retries, then
 * refetches the screen's queries (including the readiness checklist, contract §7).
 */
import { ApiProblem } from "@sutra/api-client";
import { Badge, useToast, type BadgeKind } from "@sutra/ui";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createContext, useCallback, useContext, useState, useSyncExternalStore, type ReactNode } from "react";
import { StepUp } from "../../components/StepUpDialog";
import { USES_API } from "../../lib/api";
import { useAuth } from "../../lib/auth";
import { useInbox } from "../../lib/inbox";
import { createHttpInstitutionApi, type InstitutionApi } from "./api";
import { createMockInstitutionApi } from "./mock";

let defaultApi: InstitutionApi | null = null;
function fallbackApi(): InstitutionApi {
  defaultApi ??= USES_API ? createHttpInstitutionApi() : createMockInstitutionApi();
  return defaultApi;
}

const ApiContext = createContext<InstitutionApi | null>(null);

/** Overrides the API instance (tests, storybook-style previews). */
export function InstitutionApiProvider({ api, children }: { api: InstitutionApi; children: ReactNode }) {
  return <ApiContext.Provider value={api}>{children}</ApiContext.Provider>;
}

export function useInstitutionApi(): InstitutionApi {
  return useContext(ApiContext) ?? fallbackApi();
}

// ---- selected institution (shared by both screens, survives tab switches) ----------------

const SELECTED_KEY = "sutra.selectedInstitution";
const listeners = new Set<() => void>();
let selected: string | null = typeof window !== "undefined" ? window.localStorage.getItem(SELECTED_KEY) : null;

export function selectInstitution(id: string | null) {
  selected = id;
  try {
    if (id) window.localStorage.setItem(SELECTED_KEY, id);
    else window.localStorage.removeItem(SELECTED_KEY);
  } catch {
    /* storage unavailable */
  }
  listeners.forEach((l) => l());
}

export function useSelectedInstitutionId(): string | null {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => selected,
    () => null,
  );
}

/** Query keys (prefix `institution` so one invalidation refreshes both screens). */
export const keys = {
  all: ["institution"] as const,
  institutions: () => ["institution", "list"] as const,
  scoped: (inst: string, what: string, extra = "") => ["institution", inst, what, extra] as const,
};

/** The institution list + the selected one (first by default). */
export function useInstitutions() {
  const api = useInstitutionApi();
  const id = useSelectedInstitutionId();
  const q = useQuery({ queryKey: keys.institutions(), queryFn: () => api.listInstitutions() });
  const list = q.data ?? [];
  const current = list.find((i) => i.id === id) ?? list[0] ?? null;
  return { query: q, list, current };
}

/** Human message of a failed command (problem+json detail / first field violation). */
export function problemText(e: unknown): string {
  if (e instanceof ApiProblem) {
    const v = e.problem.validationErrors?.[0];
    if (v) return `${v.message}`;
    return e.problem.detail ?? e.problem.title;
  }
  return e instanceof Error ? e.message : "Something went wrong";
}

/** Field → message map of a 422 (for inline form errors). */
export function fieldErrors(e: unknown): Record<string, string> {
  if (e instanceof ApiProblem && e.problem.validationErrors) {
    return Object.fromEntries(e.problem.validationErrors.map((x) => [x.field, x.message]));
  }
  return {};
}

/**
 * Runs commands with toasts, step-up retry and refetch. Render `dialog` once in the screen.
 */
export function useCommand() {
  const toast = useToast();
  const client = useQueryClient();
  const { adoptSession } = useAuth();
  const inbox = useInbox();
  const [pending, setPending] = useState<null | (() => Promise<unknown>)>(null);
  const [busy, setBusy] = useState(false);
  const mfa = useQuery({ queryKey: ["institution", "mfa"], queryFn: () => inbox.mfa(), enabled: pending !== null });

  const run = useCallback(
    async <T,>(action: () => Promise<T>, success?: string | ((r: T) => string), opts: { rethrow?: boolean } = {}): Promise<T | undefined> => {
      setBusy(true);
      try {
        const r = await action();
        if (success) toast(typeof success === "function" ? success(r) : success);
        await client.invalidateQueries({ queryKey: keys.all });
        return r;
      } catch (e) {
        if (e instanceof ApiProblem && e.problem.errorCode === "mfa-step-up-required") {
          setPending(() => async () => {
            const r = await action();
            if (success) toast(typeof success === "function" ? success(r) : success);
          });
        } else if (opts.rethrow) {
          throw e;
        } else {
          toast(problemText(e));
        }
        return undefined;
      } finally {
        setBusy(false);
      }
    },
    [client, toast],
  );

  /** For forms: errors are rethrown so the form shows them inline (422 field errors). */
  const submit = useCallback(
    <T,>(action: () => Promise<T>, success?: string | ((r: T) => string)) => run(action, success, { rethrow: true }),
    [run],
  );

  const dialog =
    pending && mfa.data ? (
      <StepUp
        status={mfa.data}
        open
        message="This action needs a fresh verification."
        onClose={() => setPending(null)}
        onDone={async (res) => {
          adoptSession(res);
          const retry = pending;
          setPending(null);
          try {
            await retry();
          } catch (e) {
            toast(problemText(e));
          }
          await client.invalidateQueries({ queryKey: keys.all });
        }}
      />
    ) : null;

  return { run, submit, busy, dialog };
}

// ---- status badges ------------------------------------------------------------------------

const BADGES: Record<string, BadgeKind> = {
  draft: "neutral",
  planned: "neutral",
  registered: "neutral",
  requested: "review",
  in_review: "review",
  pending_approval: "review",
  pending_activation: "review",
  closing: "review",
  configured: "info",
  returned: "attention",
  rejected: "attention",
  published: "done",
  active: "done",
  open: "done",
  effective: "done",
  approved: "info",
  in_effect: "done",
  evaluated: "info",
  accepted: "done",
  suspended: "attention",
  superseded: "neutral",
  retired: "neutral",
  closed: "neutral",
  decommissioned: "neutral",
  archived: "neutral",
  completed: "neutral",
  withdrawn: "neutral",
  discarded: "neutral",
  met: "done",
  unmet: "attention",
  not_applicable: "neutral",
  favourable: "done",
  unfavourable: "attention",
  uncertain: "review",
  // Batch 4 (academic delivery)
  enrollment_open: "done",
  enrollment_closed: "neutral",
  enrolled: "done",
};

/** Contract states spelled the US way (`enrollment_*`) read in the app's en-IN spelling. */
const LABELS: Record<string, string> = { enrollment_open: "Enrolment open", enrollment_closed: "Enrolment closed" };

export const statusLabel = (s: string) => LABELS[s] ?? s.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());

export function StatusBadge({ status }: { status: string }) {
  return <Badge kind={BADGES[status] ?? "neutral"}>{statusLabel(status)}</Badge>;
}
