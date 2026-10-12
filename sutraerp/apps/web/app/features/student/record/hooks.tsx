/**
 * React glue of the 3A screens: the `RecordApi` instance (context → HTTP in API mode, mock
 * otherwise), query keys (prefix `student3a`) and `useRecordCommand` — the Batch 2 command runner
 * (toasts, problem+json messages, step-up retry) plus a refetch of the 3A queries.
 */
import { useQueryClient } from "@tanstack/react-query";
import { createContext, useCallback, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { useCommand } from "../../institution/hooks";
import { createHttpRecordApi, type RecordApi } from "./api";
import { createMockRecordApi } from "./mock";

let fallback: RecordApi | null = null;
function defaultApi(): RecordApi {
  fallback ??= USES_API ? createHttpRecordApi() : createMockRecordApi();
  return fallback;
}

const Ctx = createContext<RecordApi | null>(null);

/** Overrides the API instance (tests, previews). */
export function RecordApiProvider({ api, children }: { api: RecordApi; children: ReactNode }) {
  return <Ctx.Provider value={api}>{children}</Ctx.Provider>;
}

export function useRecordApi(): RecordApi {
  return useContext(Ctx) ?? defaultApi();
}

export const rkeys = {
  all: ["student3a"] as const,
  scoped: (...parts: string[]) => ["student3a", ...parts] as const,
};

/** `useCommand` of the admin screens + refetch of every 3A query after success. */
export function useRecordCommand() {
  const base = useCommand();
  const client = useQueryClient();
  const wrap = useCallback(
    <T,>(action: () => Promise<T>) =>
      async () => {
        const r = await action();
        await client.invalidateQueries({ queryKey: rkeys.all });
        return r;
      },
    [client],
  );
  const run = useCallback(<T,>(action: () => Promise<T>, success?: string | ((r: T) => string)) => base.run(wrap(action), success), [base, wrap]);
  const submit = useCallback(<T,>(action: () => Promise<T>, success?: string | ((r: T) => string)) => base.submit(wrap(action), success), [base, wrap]);
  return { run, submit, busy: base.busy, dialog: base.dialog };
}
