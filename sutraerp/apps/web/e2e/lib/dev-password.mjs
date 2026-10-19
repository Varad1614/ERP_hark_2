/**
 * Audit 2026-10 (A1 · D1/C1/SEC-7): the dev authenticator has no default password and the web
 * bundle no longer embeds one. Every suite uses ONE value for the API it starts, its API logins
 * and the browser sign-in:
 *
 *   SUTRA_DEV_AUTH_PASSWORD  (CI sets a CI-only value in the workflow env)
 *   E2E_PASSWORD             overrides it for logins against an externally started stack
 *
 * Unset locally → a random per-run value, exported so the API (and `react-router dev`) spawned by
 * the suite inherit it.
 */
import { randomUUID } from "node:crypto";

if (!process.env.SUTRA_DEV_AUTH_PASSWORD) process.env.SUTRA_DEV_AUTH_PASSWORD = `e2e-local-${randomUUID()}`;
// Transitional: web builds from before the typed-password sign-in read VITE_DEV_PASSWORD.
process.env.VITE_DEV_PASSWORD ??= process.env.SUTRA_DEV_AUTH_PASSWORD;

export const DEV_PASSWORD = process.env.E2E_PASSWORD || process.env.SUTRA_DEV_AUTH_PASSWORD;

/**
 * Browser dev sign-in as `role`: types the dev password when the sign-in page asks for it
 * (`[data-password]`), then picks the persona card.
 */
export async function clickDevPersona(page, role) {
  const card = page.locator(`[data-role-card="${role}"]`);
  await card.waitFor();
  const pw = page.locator("[data-password]");
  if (await pw.count()) await pw.first().fill(DEV_PASSWORD);
  await card.click();
}
