import { BellOff, CheckCheck } from "lucide-react";
import type { Notification } from "@sutra/api-client";
import { BentoGrid, BentoTile, Button, EmptyState, useToast } from "@sutra/ui";
import { useInfiniteQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useNavigate } from "react-router";
import { useSession } from "../lib/auth";
import "../components/shell.css";
import "../features/home/home.css";
import { NotificationRow } from "../components/NotificationRow";
import { inboxKeys, useInbox, useInboxRefresh } from "../lib/inbox";

export function meta() {
  return [{ title: "Notifications · SutraERP" }];
}

/** Full notification inbox: all / unread, mark read, mark all read, load more. */
export default function Notifications() {
  const { role } = useSession();
  const inbox = useInbox();
  const refresh = useInboxRefresh();
  const toast = useToast();
  const navigate = useNavigate();
  const [unreadOnly, setUnreadOnly] = useState(false);
  const q = useInfiniteQuery({
    queryKey: inboxKeys.notifications(role, unreadOnly),
    queryFn: ({ pageParam }) => inbox.notifications({ unreadOnly, cursor: pageParam, limit: 20 }),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.nextCursor ?? undefined,
  });
  const items = q.data?.pages.flatMap((p) => p.items) ?? [];
  const unread = q.data?.pages[0]?.unreadCount ?? 0;

  const open = async (n: Notification) => {
    if (!n.readAt) {
      await inbox.markRead(n.id).catch(() => undefined);
      await refresh();
    }
    if (n.link) navigate(n.link);
  };
  const readAll = async () => {
    const marked = await inbox.markAllRead();
    await refresh();
    toast(marked === 1 ? "1 notification marked read" : `${marked} notifications marked read`);
  };

  return (
    <BentoGrid data-screen="notifications">
      <BentoTile span={12} className="sb-area-head" data-area-head="">
        <div className="sb-area-title">
          <h1>Notifications</h1>
          <p className="sb-area-summary" data-area-summary="">
            {q.isLoading ? "Loading…" : unread ? `${unread} unread · newest first` : "You’re all caught up · newest first"}
          </p>
        </div>
        <div className="sb-area-side">
          <div className="dseg" aria-label="Show">
            <button type="button" aria-pressed={!unreadOnly} className={!unreadOnly ? "on" : ""} onClick={() => setUnreadOnly(false)} data-notif-view="all">
              All
            </button>
            <button type="button" aria-pressed={unreadOnly} className={unreadOnly ? "on" : ""} onClick={() => setUnreadOnly(true)} data-notif-view="unread">
              Unread{unread ? ` · ${unread}` : ""}
            </button>
          </div>
          <Button variant="ghost" icon={<CheckCheck strokeWidth={1.75} />} iconPosition="start" onClick={readAll} disabled={unread === 0} data-read-all>
            Mark all read
          </Button>
        </div>
      </BentoTile>
      <BentoTile span={12}>
        {q.isLoading ? (
          <div className="sb-skeleton" style={{ width: "60%" }} />
        ) : q.isError ? (
          <EmptyState icon={BellOff} title="Notifications are unavailable">
            {q.error instanceof Error ? q.error.message : "Try again shortly."}
          </EmptyState>
        ) : items.length === 0 ? (
          <EmptyState icon={BellOff} title={unreadOnly ? "You’re all caught up" : "No notifications yet"}>
            Updates about your institution, notices and to-dos appear here.
          </EmptyState>
        ) : (
          <div className="nf-list" data-notification-list>
            {items.map((n) => (
              <NotificationRow key={n.id} n={n} onOpen={open} />
            ))}
          </div>
        )}
        {q.hasNextPage && (
          <div className="sb-actions-row">
            <Button variant="ghost" onClick={() => q.fetchNextPage()} disabled={q.isFetchingNextPage}>
              {q.isFetchingNextPage ? "Loading…" : "Load older"}
            </Button>
          </div>
        )}
      </BentoTile>
    </BentoGrid>
  );
}
