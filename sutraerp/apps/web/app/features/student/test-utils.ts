/**
 * Test helpers of the re-skinned Students screens (R2): a row's / header's secondary actions live
 * in kebab menus (`[data-row-menu]`, the Student 360 "More" `[data-student-more]`), so a command
 * is found either as the row's primary pill (`[data-action]`) or as a menu item (`[data-menu-item]`).
 */
import { fireEvent, waitFor } from "@testing-library/react";

export function findAction(id: string): HTMLElement | null {
  const direct = document.querySelector<HTMLElement>(`[data-action="${id}"]`);
  if (direct) return direct;
  const item = document.querySelector<HTMLElement>(`[data-menu-item="${id}"]`);
  if (item) return item;
  for (const k of document.querySelectorAll<HTMLElement>('[data-row-menu][aria-expanded="false"], [data-student-more][aria-expanded="false"]')) {
    fireEvent.click(k);
    const hit = document.querySelector<HTMLElement>(`[data-menu-item="${id}"]`);
    if (hit) return hit;
  }
  return null;
}

/** Clicks the command `id` (primary pill or kebab / More menu item) once it is rendered. */
export async function clickAction(id: string) {
  const el = await waitFor(() => {
    const e = findAction(id);
    if (!e) throw new Error(`action ${id} not rendered`);
    return e;
  });
  fireEvent.click(el);
}
