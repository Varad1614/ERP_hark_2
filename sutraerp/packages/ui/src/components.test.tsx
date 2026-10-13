import { fireEvent, render, screen } from "@testing-library/react";
import { Inbox, LayoutGrid, Users } from "lucide-react";
import { describe, expect, it, vi } from "vitest";
import { applyDisplayPrefs, Badge, CommandPalette, DataTable, EmptyState, StatTile, Steps, ToastProvider, useToast } from "./index";

describe("@sutra/ui", () => {
  it("renders a badge with its kind class", () => {
    render(<Badge kind="attention">Due soon</Badge>);
    expect(screen.getByText("Due soon").className).toContain("sb-badge--attention");
  });

  it("renders a clickable stat tile", () => {
    const onClick = vi.fn();
    render(<StatTile icon={Inbox} tone="violet" label="To review" value="12" hint="Lab submissions" onClick={onClick} />);
    fireEvent.click(screen.getByRole("button"));
    expect(onClick).toHaveBeenCalled();
    expect(screen.getByText("12")).toBeTruthy();
  });

  it("renders table rows or an empty state", () => {
    const cols = [{ key: "n", header: "Name", render: (r: { n: string }) => r.n }];
    const { rerender } = render(<DataTable columns={cols} rows={[{ n: "Aarav" }]} rowKey={(r) => r.n} />);
    expect(screen.getByRole("table")).toBeTruthy();
    rerender(<DataTable columns={cols} rows={[]} rowKey={(r) => r.n} empty={<EmptyState title="No records" />} />);
    expect(screen.getByText("No records")).toBeTruthy();
  });

  it("marks completed and current steps", () => {
    const { container } = render(<Steps stages={["Draft", "Submitted", "In review", "Approved"]} current={1} />);
    expect(container.querySelectorAll("li.ok").length).toBe(1);
    expect(container.querySelector("li.cur")?.textContent).toContain("Submitted");
  });

  it("filters and picks in the command palette", () => {
    const onSelect = vi.fn();
    render(
      <CommandPalette
        open
        onClose={() => {}}
        onSelect={onSelect}
        items={[
          { id: "a", title: "Workspaces", subtitle: "Screen", group: "Screens", icon: LayoutGrid, tone: "grey" },
          { id: "b", title: "Fee Management", subtitle: "W-F01 · Workspace", group: "Workspaces", icon: LayoutGrid, tone: "green" },
        ]}
      />,
    );
    fireEvent.change(screen.getByLabelText("Command palette", { selector: "input" }), { target: { value: "fee" } });
    fireEvent.keyDown(screen.getByLabelText("Command palette", { selector: "input" }), { key: "Enter" });
    expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ id: "b" }));
  });

  it("shows toasts", () => {
    function Fire() {
      const toast = useToast();
      return <button onClick={() => toast("Saved")}>go</button>;
    }
    render(
      <ToastProvider>
        <Fire />
      </ToastProvider>,
    );
    fireEvent.click(screen.getByText("go"));
    expect(screen.getByRole("status").textContent).toContain("Saved");
  });
});

describe("re-skin additions", () => {
  it("draws a sparkline only for a real series and marks pending KPI tiles", () => {
    const a = render(<StatTile icon={Users} tone="blue" label="Avg" value="88%" trend={[80, 90, 88]} trendLabel="Last 3 sessions" />);
    expect(a.container.querySelector("svg.sb-spark polyline")).toBeTruthy();
    const b = render(<StatTile icon={Users} tone="green" label="Faculty" value="—" pending="Arrives in Batch 7" />);
    expect(b.container.querySelector(".sb-stat.is-pending")).toBeTruthy();
    expect(b.container.querySelector("svg.sb-spark")).toBeNull();
    expect(b.getByText("Arrives in Batch 7")).toBeTruthy();
  });

  it("applies display prefs as classes on <html>", () => {
    const el = document.createElement("html");
    applyDisplayPrefs({ mode: "dark", contrast: "high", font: "easy" }, el);
    expect([...el.classList].sort()).toEqual(["dk", "easy", "hc"]);
    applyDisplayPrefs({ mode: "light", contrast: "standard", font: "sutra" }, el);
    expect(el.classList.length).toBe(0);
  });
});
