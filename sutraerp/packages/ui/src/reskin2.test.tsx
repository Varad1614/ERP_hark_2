import { fireEvent, render, screen } from "@testing-library/react";
import { CircleDollarSign, Presentation, Wallet } from "lucide-react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import {
  ApprovalCard,
  AreaHeader,
  AttendanceGrid,
  ClassCard,
  countMarks,
  DatePicker,
  FilterChips,
  formatDate,
  formatDateShort,
  formatDays,
  formatINR,
  formatNumber,
  formatPercent,
  formatTime,
  humanize,
  initialsOf,
  KpiTile,
  ListRow,
  nextMark,
  PersonCard,
  splitTabs,
  StatusBadge,
  StepDialog,
  TagInput,
  WeekGrid,
  type Mark,
} from "./index";

describe("formatters", () => {
  it("formats rupees with Indian grouping and no needless paise", () => {
    expect(formatINR(140000)).toBe("₹1,40,000");
    expect(formatINR("160000.00")).toBe("₹1,60,000");
    expect(formatINR("40645.16")).toBe("₹40,645.16");
    expect(formatINR("46250.5")).toBe("₹46,250.50");
    expect(formatINR(-2500)).toBe("\u2212₹2,500");
    expect(formatINR("12345678.00", { paise: "always" })).toBe("₹1,23,45,678.00");
    expect(formatINR(999.6, { paise: "never" })).toBe("₹1,000");
    expect(formatINR(null)).toBe("—");
    expect(formatINR("0.00")).toBe("₹0");
  });

  it("compacts rupees to lakh / crore", () => {
    expect(formatINR(34000000, { compact: true })).toBe("₹3.4 Cr");
    expect(formatINR(9200000, { compact: true })).toBe("₹92 L");
    expect(formatINR(46250, { compact: true })).toBe("₹46,250");
  });

  it("formats counts, days and percents", () => {
    expect(formatNumber("12.00")).toBe("12");
    expect(formatNumber(4812)).toBe("4,812");
    expect(formatNumber("2.50")).toBe("2.5");
    expect(formatNumber("12.999")).toBe("13");
    expect(formatDays("12.00")).toBe("12 days");
    expect(formatDays(1)).toBe("1 day");
    expect(formatDays("1.5")).toBe("1.5 days");
    expect(formatDays(0)).toBe("0 days");
    expect(formatPercent(82)).toBe("82%");
    expect(formatPercent(0.725, { ratio: true })).toBe("72.5%");
  });

  it("formats dates as dd MMM yyyy without time-zone drift", () => {
    expect(formatDate("2026-07-15")).toBe("15 Jul 2026");
    expect(formatDate("2026-01-01")).toBe("1 Jan 2026");
    expect(formatDateShort("2026-09-30")).toBe("30 Sep");
    expect(formatDate("2026-07-14T20:00:00Z")).toBe("15 Jul 2026");
    expect(formatTime("09:05:00")).toBe("09:05");
    expect(formatDate(undefined)).toBe("—");
  });

  it("turns wire codes into human labels", () => {
    expect(humanize("not_submitted")).toBe("Not submitted");
    expect(humanize("approvalPending")).toBe("Approval pending");
    expect(humanize("UNDER-REVIEW")).toBe("Under review");
    expect(humanize("nptel_course")).toBe("NPTEL course");
    expect(initialsOf("Dr. Meera Shah")).toBe("MS");
    expect(initialsOf("Aditi")).toBe("AD");
  });
});

describe("re-skin components", () => {
  it("AreaHeader keeps at most five tabs and folds the rest into More", () => {
    expect(splitTabs([1, 2, 3, 4, 5]).overflow).toEqual([]);
    expect(splitTabs([1, 2, 3, 4, 5, 6, 7])).toEqual({ visible: [1, 2, 3, 4], overflow: [5, 6, 7] });
    const onTab = vi.fn();
    render(
      <AreaHeader
        title="Campus"
        summary="12 rooms in use · 3 open tickets"
        tabs={["Overview", "Spaces", "Services", "Resources", "Hostel", "Transport"].map((l) => ({ id: l.toLowerCase(), label: l }))}
        activeTab="transport"
        onTabChange={onTab}
      />,
    );
    expect(screen.getByRole("heading", { name: "Campus" })).toBeTruthy();
    expect(screen.getByText("12 rooms in use · 3 open tickets")).toBeTruthy();
    expect(screen.getAllByRole("button", { pressed: false }).length).toBe(4);
    const more = screen.getByRole("button", { name: /More · Transport/ });
    fireEvent.click(more);
    fireEvent.click(screen.getByRole("menuitem", { name: "Hostel" }));
    expect(onTab).toHaveBeenCalledWith("hostel");
  });

  it("FilterChips selects one option", () => {
    const onChange = vi.fn();
    render(<FilterChips label="Fee status" options={[{ id: "all", label: "All", count: 25 }, { id: "paid", label: "Paid", count: 15 }]} value="all" onChange={onChange} />);
    fireEvent.click(screen.getByRole("button", { name: /Paid/ }));
    expect(onChange).toHaveBeenCalledWith("paid");
    expect(screen.getByRole("button", { name: /All/ }).getAttribute("aria-pressed")).toBe("true");
  });

  it("ListRow shows one primary action and the rest in the kebab", () => {
    const approve = vi.fn();
    const suspend = vi.fn();
    render(<ListRow title="Aditi Sharma" meta="SIT25CE011 · B.Tech Computer Engg" avatar="Aditi Sharma" badge={{ label: "Partial", kind: "review" }} primary={{ label: "Approve", onClick: approve }} menu={[{ id: "suspend", label: "Suspend", danger: true, onSelect: suspend }]} />);
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    expect(approve).toHaveBeenCalled();
    expect(screen.queryByRole("menuitem", { name: "Suspend" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "More actions" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Suspend" }));
    expect(suspend).toHaveBeenCalled();
    expect(screen.getByText("AS")).toBeTruthy();
  });

  it("ApprovalCard approves, rejects and shows the outcome", () => {
    const ok = vi.fn();
    const no = vi.fn();
    const { rerender } = render(<ApprovalCard icon={CircleDollarSign} title="Riya Kapoor · Merit scholarship 25%" meta="−₹46,250" onApprove={ok} onReject={no} />);
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    fireEvent.click(screen.getByRole("button", { name: "Reject" }));
    expect(ok).toHaveBeenCalled();
    expect(no).toHaveBeenCalled();
    rerender(<ApprovalCard icon={CircleDollarSign} title="Riya Kapoor" decided={{ label: "Approved", kind: "done" }} />);
    expect(screen.getByText("Approved")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
  });

  it("KpiTile renders a sparkline with the 7 days label for a real series", () => {
    const { container } = render(<KpiTile icon={Wallet} tone="green" label="Collected" value="₹3.4 Cr" sub="Since 1 April" trend={[1, 3, 2, 5, 4, 6, 7]} />);
    expect(container.querySelector("svg.sb-spark")).toBeTruthy();
    expect(screen.getByText("7 days")).toBeTruthy();
  });

  it("cards render name, role and progress", () => {
    render(
      <>
        <PersonCard name="Dr. Meera Shah" role="Associate Professor" tags={[{ label: "Computer Engg" }, { label: "Leave pending", kind: "review" }]} wide />
        <ClassCard code="CS201 · Section A" title="Data Structures" meta="64 students · B-204" icon={Presentation} facts={[{ label: "Next session", value: "Today, 10:30" }]} progress={{ label: "Syllabus · 2 of 5 units", value: 40 }} />
      </>,
    );
    expect(screen.getByText("MS")).toBeTruthy();
    expect(screen.getByText("40%")).toBeTruthy();
    expect(screen.getByRole("progressbar").getAttribute("aria-valuenow")).toBe("40");
  });

  it("WeekGrid places blocks by time and marks today", () => {
    const onSlot = vi.fn();
    const { container } = render(
      <WeekGrid days={["Mon", "Tue", "Wed"]} todayIndex={1} slots={[{ id: "a", day: 1, start: "10:30", end: "11:30", title: "Data Structures", meta: "10:30–11:30 · B-204" }]} onSlotClick={onSlot} />,
    );
    expect(screen.getByText("Today")).toBeTruthy();
    expect(container.querySelector('[data-slot="a"]')?.getAttribute("data-rows")).toBe("5-7");
    fireEvent.click(screen.getByRole("button", { name: /Data Structures/ }));
    expect(onSlot).toHaveBeenCalled();
  });

  it("AttendanceGrid cycles present → absent → late → present", () => {
    expect(nextMark(undefined)).toBe("absent");
    expect(nextMark("absent")).toBe("late");
    expect(nextMark("late")).toBe("present");
    function Harness() {
      const [marks, setMarks] = useState<Record<string, Mark | undefined>>({});
      const students = [
        { id: "1", name: "Aditi Sharma" },
        { id: "2", name: "Rohan Patil" },
      ];
      const c = countMarks(students, marks);
      return (
        <>
          <AttendanceGrid students={students} marks={marks} onChange={(id, m) => setMarks((x) => ({ ...x, [id]: m }))} />
          <p>{`${c.absent} absent · ${c.late} late · ${c.present} present`}</p>
        </>
      );
    }
    render(<Harness />);
    fireEvent.click(screen.getByRole("button", { name: /Aditi Sharma/ }));
    expect(screen.getByText("1 absent · 0 late · 1 present")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Aditi Sharma/ }));
    expect(screen.getByText("0 absent · 1 late · 1 present")).toBeTruthy();
  });

  it("StepDialog shows eyebrow, close, stepper and Next", () => {
    const next = vi.fn();
    const close = vi.fn();
    render(
      <StepDialog open onClose={close} eyebrow="Students · New admission" title="Applicant details" steps={["Applicant details", "Programme & documents", "Fee & confirm"]} step={0} primary={{ onClick: next }}>
        <p>body</p>
      </StepDialog>,
    );
    expect(screen.getByText("Students · New admission")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Next/ }));
    expect(next).toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(close).toHaveBeenCalled();
  });

  it("DatePicker shows dd MMM yyyy and keeps an ISO value", () => {
    const onChange = vi.fn();
    render(<DatePicker label="Due date" value="2026-07-15" onChange={onChange} />);
    expect(screen.getByText("15 Jul 2026")).toBeTruthy();
    const input = screen.getByLabelText("Due date") as HTMLInputElement;
    expect(input.type).toBe("date");
    expect(input.value).toBe("2026-07-15");
  });

  it("TagInput adds tags on Enter and removes them", () => {
    function Harness() {
      const [v, setV] = useState<string[]>(["Marksheet"]);
      return <TagInput label="Documents" value={v} onChange={setV} />;
    }
    render(<Harness />);
    const input = screen.getByLabelText("Documents");
    fireEvent.change(input, { target: { value: "Aadhaar" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(screen.getByText("Aadhaar")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Remove Marksheet" }));
    expect(screen.queryByText("Marksheet")).toBeNull();
  });

  it("StatusBadge humanises a wire status and picks its colour", () => {
    render(<StatusBadge status="approval_pending" />);
    const b = screen.getByText("Approval pending");
    expect(b.className).toContain("sb-badge--review");
  });
});
