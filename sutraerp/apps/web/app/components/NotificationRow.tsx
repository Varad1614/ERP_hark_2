import { Bell, Building2, ListTodo, Megaphone, ShieldAlert, type LucideIcon } from "lucide-react";
import type { Notification, NotificationCategory } from "@sutra/api-client";
import { IconChip, type Tone } from "@sutra/ui";
import { relativeTime } from "../lib/inbox";

/** Icon, tone and label per notification category (bell, inbox and profile preferences). */
export const CATEGORY_META: Record<NotificationCategory, { icon: LucideIcon; tone: Tone; label: string }> = {
  institution: { icon: Building2, tone: "blue", label: "Institution" },
  notice: { icon: Megaphone, tone: "orange", label: "Notices" },
  todo: { icon: ListTodo, tone: "violet", label: "To-dos" },
  security: { icon: ShieldAlert, tone: "rose", label: "Security" },
  system: { icon: Bell, tone: "grey", label: "System" },
};

export function NotificationRow({ n, onOpen }: { n: Notification; onOpen: (n: Notification) => void }) {
  const m = CATEGORY_META[n.category];
  return (
    <button type="button" className={`sb-pop-item${n.readAt ? " is-read" : ""}`} data-notification={n.id} onClick={() => onOpen(n)}>
      <IconChip icon={m.icon} tone={m.tone} size="sm" />
      <div>
        <b>{n.title}</b>
        <small>
          {n.body ? `${n.body} · ` : ""}
          {relativeTime(n.createdAt)}
        </small>
      </div>
      <span className="sb-unread" />
    </button>
  );
}
