/**
 * Re-skin v2 component catalogue (dev-only): every @sutra/ui prototype component with sample
 * data, for screenshots next to the prototype (docs/reskin2-contracts.md §6).
 *
 *   /dev/catalogue?role=administrator|faculty|student&theme=light|dark&open=dialog|panel
 *
 * Only served by `react-router dev` or a build with VITE_UI_CATALOGUE=1 (the e2e builds); any
 * other build renders "Not found". Sample values below are catalogue fixtures, never shown in the app.
 */
import {
  ApprovalCard,
  ApprovalList,
  AreaHeader,
  AttendanceGrid,
  Balance,
  BarChart,
  BentoGrid,
  BentoTile,
  Button,
  CardGrid,
  Checkbox,
  CheckboxList,
  ClassCard,
  countMarks,
  CourseCard,
  DataTable,
  DateList,
  DatePicker,
  EmptyState,
  FileField,
  FilterChipRows,
  FilterChips,
  formatDate,
  formatDays,
  formatINR,
  formatNumber,
  FormGrid,
  FormSelect,
  KpiTile,
  ListRow,
  Logo,
  NameCell,
  PersonCard,
  ReportCard,
  RowActions,
  SidePanel,
  StatCells,
  StatusBadge,
  StepDialog,
  TagInput,
  TextArea,
  TextField,
  Toggle,
  useRoleTheme,
  WeekGrid,
  type Mark,
  type ThemeRole,
} from "@sutra/ui";
import { BookOpen, CalendarCheck, CircleDollarSign, ClipboardList, Clock, FileText, Hourglass, Inbox, Percent, Plane, Presentation, Target, UserPlus, Users, Wallet } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useSearchParams } from "react-router";

const CATALOGUE_ENABLED = import.meta.env.DEV || import.meta.env.VITE_UI_CATALOGUE === "1";

const ROLES: ThemeRole[] = ["administrator", "faculty", "student"];

const STUDENTS = [
  ["Aditi Sharma", "SIT25CE011", "B.Tech Computer Engg", "Sem 1", "72", "partial"],
  ["Rohan Patil", "SIT22ME014", "B.Tech Mechanical", "Sem 7", "73", "paid"],
  ["Ishita Iyer", "SIT23EC017", "B.Tech Electronics", "Sem 5", "74", "overdue"],
  ["Kabir Kulkarni", "SIT24CE020", "B.Tech Computer Engg", "Sem 3", "75", "paid"],
  ["Ananya Reddy", "SIT25MB023", "MBA", "Sem 1", "76", "paid"],
] as const;

const ROSTER = ["Aditi Sharma", "Rohan Patil", "Ishita Iyer", "Kabir Kulkarni", "Ananya Reddy", "Vihaan Joshi", "Sneha Nair", "Arjun Gupta", "Diya Menon", "Yash Deshpande", "Kavya Rao", "Siddharth Chatterjee", "Riya Kapoor", "Aditya Mehta", "Pooja Bhat", "Nikhil Verma"].map((name, i) => ({ id: String(i + 1), name }));

export default function Catalogue() {
  const [params] = useSearchParams();
  const role = (ROLES.find((r) => r === params.get("role")) ?? "administrator") as ThemeRole;
  useRoleTheme(role);
  const theme = params.get("theme");
  useEffect(() => {
    document.documentElement.classList.toggle("dk", theme === "dark");
  }, [theme]);
  const [tab, setTab] = useState("overview");
  const [chip, setChip] = useState("all");
  const [fee, setFee] = useState("all");
  const [q, setQ] = useState("");
  const [marks, setMarks] = useState<Record<string, Mark | undefined>>({ "3": "absent", "7": "late" });
  const [dialog, setDialog] = useState(params.get("open") === "dialog");
  const [step, setStep] = useState(0);
  const [panel, setPanel] = useState(params.get("open") === "panel");
  const [docs, setDocs] = useState<string[]>(["Class 12 marksheet", "Aadhaar"]);
  const [programmes, setProgrammes] = useState<string[]>(["cse"]);
  const [due, setDue] = useState("2026-07-15");
  const [done, setDone] = useState(false);
  const counts = useMemo(() => countMarks(ROSTER, marks), [marks]);

  if (!CATALOGUE_ENABLED) {
    return (
      <div className="sb-app">
        <EmptyState title="Not found" />
      </div>
    );
  }
  return (
    <div className="sb-app" data-catalogue={role}>
      <header className="sb-header">
        <Logo />
        <nav className="sb-pills" aria-label="Catalogue role">
          {ROLES.map((r) => (
            <a key={r} href={`?role=${r}${theme ? `&theme=${theme}` : ""}`} className={r === role ? "on" : ""}>
              {r === "administrator" ? "Admin" : r === "faculty" ? "Faculty" : "Student"}
            </a>
          ))}
        </nav>
      </header>
      <main>
        <BentoGrid>
          <h2 className="sb-cat-section">AreaHeader · FilterChips</h2>
          <AreaHeader
            title="Students"
            summary={`${formatNumber(4812)} enrolled · 25 recent records shown`}
            search={{ label: "Search name or roll no.", value: q, onChange: setQ }}
            actions={
              <>
                <Button icon={<UserPlus strokeWidth={1.75} aria-hidden="true" />} iconPosition="start" onClick={() => setDialog(true)}>
                  New admission
                </Button>
              </>
            }
          />
          <FilterChipRows>
            <FilterChips label="Programme" value={chip} onChange={setChip} options={[{ id: "all", label: "All", count: 25 }, { id: "cse", label: "B.Tech Computer Engg", count: 10 }, { id: "me", label: "B.Tech Mechanical", count: 5 }, { id: "ec", label: "B.Tech Electronics", count: 5 }, { id: "mba", label: "MBA", count: 5 }]} />
            <FilterChips label="Fee status" value={fee} onChange={setFee} options={[{ id: "all", label: "All", count: 25 }, { id: "paid", label: "Paid", count: 15 }, { id: "partial", label: "Partial", count: 5 }, { id: "overdue", label: "Overdue", count: 5 }]} />
          </FilterChipRows>
          <AreaHeader
            title="Campus"
            summary="14 rooms in use now · 3 open tickets · 2 buses running late"
            tabs={["Overview", "Spaces", "Services", "Resources", "Hostel", "Transport", "Library", "Governance"].map((l) => ({ id: l.toLowerCase(), label: l }))}
            activeTab={tab}
            onTabChange={setTab}
          />
          <AreaHeader title="Aditi Sharma" summary="SIT25CE011 · B.Tech Computer Engg · Sem 1" back={{ label: "Students", to: "/dev/catalogue" }} tabs={[{ id: "overview", label: "Overview" }, { id: "academics", label: "Academics" }, { id: "attendance", label: "Attendance" }, { id: "fees", label: "Fees" }, { id: "documents", label: "Documents" }]} activeTab="overview" onTabChange={() => {}} />

          <h2 className="sb-cat-section">KpiTile</h2>
          <KpiTile icon={Wallet} tone="green" label="Collected" value={formatINR(34000000, { compact: true })} sub="Since 1 April" trend={[22, 28, 25, 31, 30, 33, 34]} />
          <KpiTile icon={Target} tone="blue" label="Target" value={formatINR(41000000, { compact: true })} sub="Full year 2025-26" />
          <KpiTile icon={Hourglass} tone="amber" label="Pending" value={formatINR(7000000, { compact: true })} sub="Across 3 instalment cycles" progress={18} />
          <KpiTile icon={Percent} tone="violet" label="Collection rate" value="82%" sub="Of the yearly target" trend={[70, 72, 75, 77, 79, 80, 82]} />

          <h2 className="sb-cat-section">Table with NameCell · StatusBadge · RowActions</h2>
          <BentoTile span={12} title="Student records" action={<span className="sb-foot">25 shown</span>}>
            <DataTable
              rows={[...STUDENTS]}
              rowKey={(r) => r[1]}
              columns={[
                { key: "n", header: "Name", render: (r) => <NameCell name={r[0]} onClick={() => setPanel(true)} /> },
                { key: "roll", header: "Roll no.", render: (r) => r[1], muted: true },
                { key: "p", header: "Programme", render: (r) => r[2], muted: true },
                { key: "s", header: "Semester", render: (r) => r[3] },
                { key: "a", header: "Attendance", render: (r) => <StatusBadge status={Number(r[4]) < 75 ? "overdue" : "partial"} labels={{ overdue: `${r[4]}%`, partial: `${r[4]}%` }} /> },
                { key: "f", header: "Fee status", render: (r) => <StatusBadge status={r[5]} /> },
                { key: "x", header: "", render: () => <RowActions primary={{ label: "Open", ghost: true, onClick: () => setPanel(true) }} menu={[{ id: "suspend", label: "Suspend", danger: true }, { id: "transfer", label: "Transfer section" }]} /> },
              ]}
            />
          </BentoTile>

          <h2 className="sb-cat-section">ListRow · ApprovalCard · BarChart</h2>
          <BentoTile span={7} title="Monthly collections" action={<span className="sb-foot">April to September · ₹ lakh</span>}>
            <BarChart
              label="Monthly collections"
              data={[
                ["Apr", 9200000],
                ["May", 3800000],
                ["Jun", 2400000],
                ["Jul", 10800000],
                ["Aug", 4600000],
                ["Sep", 3200000],
              ].map(([l, v], i) => ({ label: String(l), value: Number(v), display: formatINR(Number(v), { compact: true }), soft: i === 5 }))}
              note="July peaks with the first-instalment deadline. September is month to date."
            />
          </BentoTile>
          <BentoTile span={5} title="Fee adjustments" action={<span className="sb-foot">3 to review</span>}>
            <ApprovalList>
              <ApprovalCard icon={CircleDollarSign} title="Riya Kapoor · Merit scholarship 25%" meta={`SIT25EC047 · B.Tech Electronics · ${formatINR(-46250)}`} onApprove={() => {}} onReject={() => {}} />
              <ApprovalCard icon={CircleDollarSign} title="Omkar Agarwal · Late fee waiver" meta={`SIT24MB068 · MBA · ${formatINR(-2500)}`} onApprove={() => {}} onReject={() => {}} />
              <ApprovalCard icon={CircleDollarSign} title="Ananya Reddy · Instalment plan" meta={`2 × ${formatINR(120000)}`} decided={{ label: "Approved", kind: "done" }} />
            </ApprovalList>
          </BentoTile>
          <BentoTile span={7} title="To-dos">
            <ListRow title="Approve 3 fee adjustments" meta="Finance · due today" check={{ checked: done, onChange: setDone, label: "Mark done" }} done={done} badge={{ label: "Due today", kind: "attention" }} />
            <ListRow title="CS201 lab submissions" meta="12 to review · Data Structures" icon={Inbox} tone="violet" primary={{ label: "Review", onClick: () => {} }} menu={[{ id: "snooze", label: "Snooze a day" }, { id: "reassign", label: "Reassign" }]} />
            <ListRow title="Dr. Meera Shah" meta="Casual leave · 3 days · from 22 Sep" avatar="Dr. Meera Shah" badge={{ label: "Pending", kind: "review" }} onOpen={() => setPanel(true)} />
          </BentoTile>
          <BentoTile span={5} title="Mid-semester calendar">
            <DateList
              days={[
                { key: "a", day: "22", month: "Sep", items: [{ meta: "09:30 · Hall A", label: "CS201 Data Structures" }, { meta: "14:00 · Hall B", label: "ME203 Thermodynamics" }] },
                { key: "b", day: "24", month: "Sep", items: [{ meta: "09:30 · Hall A", label: "EC205 Signals & Systems" }] },
              ]}
            />
          </BentoTile>

          <h2 className="sb-cat-section">Cards</h2>
          <CardGrid>
            <PersonCard name="Dr. Meera Shah" role="Associate Professor" detail="meera.shah@sit.edu.in · A-215" tags={[{ label: "Computer Engg" }, { label: "Leave pending", kind: "review" }]} wide />
            <PersonCard name="Prof. Anil Kulkarni" role="Professor and HoD" tags={[{ label: "Computer Engg" }, { label: "Leave pending", kind: "review" }]} />
            <PersonCard name="Neha Joshi" role="Registrar" tags={[{ label: "Administration" }]} />
            <ClassCard code="CS201 · Section A" title="Data Structures" meta="64 students · B-204" icon={Presentation} tone="blue" facts={[{ label: "Next session", value: "Today, 10:30 · B-204" }]} progress={{ label: "Syllabus · 2 of 5 units", value: 40 }} onClick={() => {}} />
            <CourseCard code="CS203" title="Database Systems" meta="4 credits · Core" icon={BookOpen} tone="violet" faculty="Dr. Meera Shah" attendance="86%" units={{ done: 3, total: 5 }} />
            <ReportCard icon={FileText} title="Fee collection" description="Collections against target by month" chart={<svg viewBox="0 0 120 40" aria-hidden="true"><polyline points="0,32 20,24 40,28 60,10 80,18 100,14 120,6" fill="none" stroke="var(--ac)" strokeWidth="2" vectorEffect="non-scaling-stroke" /></svg>} onClick={() => {}} />
            <ReportCard icon={Users} tone="blue" title="Attendance" description="Below 75% by programme" />
            <ReportCard icon={ClipboardList} tone="violet" title="Results" description="Pass rate by course" />
          </CardGrid>

          <h2 className="sb-cat-section">WeekGrid</h2>
          <BentoTile span={12} title="This week" action={<span className="sb-foot">Tap a block for details</span>}>
            <WeekGrid
              days={["Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]}
              todayIndex={1}
              startHour={9}
              endHour={17}
              onSlotClick={() => setPanel(true)}
              slots={[
                { id: "1", day: 0, start: "09:00", end: "10:00", title: "Data Structures", meta: "09:00–10:00 · B-204", tone: "blue" },
                { id: "2", day: 0, start: "11:00", end: "12:00", title: "Database Systems", meta: "11:00–12:00 · B-102", tone: "violet" },
                { id: "3", day: 1, start: "09:00", end: "10:00", title: "Database Systems", meta: "09:00–10:00 · B-102", tone: "violet" },
                { id: "4", day: 1, start: "10:30", end: "11:30", title: "Data Structures", meta: "10:30–11:30 · B-204", tone: "blue" },
                { id: "5", day: 1, start: "14:00", end: "15:00", title: "Student consultation", meta: "14:00–15:00 · Cabin 3-12", tone: "amber" },
                { id: "6", day: 2, start: "14:00", end: "16:00", title: "Data Structures lab", meta: "14:00–16:00 · Lab 3", tone: "blue" },
                { id: "7", day: 4, start: "09:00", end: "11:00", title: "Database Systems lab", meta: "09:00–11:00 · Lab 2", tone: "violet" },
                { id: "8", day: 5, start: "10:00", end: "12:00", title: "Project reviews", meta: "10:00–12:00 · Seminar Hall", tone: "amber" },
              ]}
            />
          </BentoTile>

          <h2 className="sb-cat-section">AttendanceGrid · StatCells · Balance</h2>
          <BentoTile span={8} title="Roster">
            <AttendanceGrid students={ROSTER} marks={marks} onChange={(id, m) => setMarks((x) => ({ ...x, [id]: m }))} />
            <div className="sb-att-foot">
              <span>{`${counts.absent} absent · ${counts.late} late · ${counts.present} present`}</span>
              <Button>Mark the rest present and submit</Button>
            </div>
          </BentoTile>
          <div className="sb-span-4 sb-side">
            <BentoTile title="Today’s count">
              <StatCells columns={3} cells={[{ label: "Present", value: counts.present, kind: "done" }, { label: "Absent", value: counts.absent, kind: "attention" }, { label: "Late", value: counts.late, kind: "review" }]} />
            </BentoTile>
            <BentoTile title="At a glance">
              <StatCells cells={[{ label: "Attendance", value: "72%", sub: "Below 75%" }, { label: "CGPA", value: "8.1", sub: "Sem 1" }, { label: "Fees due", value: formatINR(92500), sub: "Due 30 Sep" }, { label: "Leave", value: formatDays("12.00"), sub: "Balance" }]} />
            </BentoTile>
          </div>
          <BentoTile span={4}>
            <Balance icon={Plane} tone="blue" label="Casual leave" used={4} total={12} caption="Resets 1 January" />
          </BentoTile>
          <BentoTile span={4}>
            <Balance icon={CalendarCheck} tone="green" label="Earned leave" used={6} total={30} />
          </BentoTile>
          <BentoTile span={4}>
            <Balance icon={Clock} tone="amber" label="Duty leave" used={1} total={5} />
          </BentoTile>

          <h2 className="sb-cat-section">Form controls</h2>
          <BentoTile span={12} title="Apply for leave">
            <FormGrid columns={3}>
              <FormSelect label="Leave type" name="leaveType" placeholder="Choose a type" options={[{ value: "casual", label: "Casual leave" }, { value: "earned", label: "Earned leave" }]} defaultValue="casual" />
              <DatePicker label="From" name="from" value={due} onChange={(e) => setDue(e.target.value)} />
              <DatePicker label="To" name="to" />
              <TextField label="Reason" name="reason" placeholder="Family function" wide />
              <TagInput label="Documents required" value={docs} onChange={setDocs} wide hint="Press Enter after each document" />
              <TagInput label="Programmes" value={programmes} onChange={setProgrammes} allowCustom={false} options={[{ value: "cse", label: "B.Tech Computer Engg" }, { value: "me", label: "B.Tech Mechanical" }, { value: "mba", label: "MBA" }]} wide />
              <TextArea label="Notes" name="notes" wide />
            </FormGrid>
            <div className="sb-mt" style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 16 }}>
              <CheckboxList label="Components">
                <Checkbox label="CS201 Data Structures" description="Major core · 4 credits" note="64 seats" defaultChecked />
                <Checkbox label="CS203 Database Systems" description="Major core · 4 credits" note="Full" disabled />
              </CheckboxList>
              <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
                <Toggle label="Visible to students" defaultChecked />
                <FileField label="Upload a file" hint="PDF up to 10 MB" onFile={() => {}} />
              </div>
            </div>
          </BentoTile>

          <h2 className="sb-cat-section">EmptyState</h2>
          <BentoTile span={6} title="Leave requests">
            <EmptyState icon={Inbox} title="No leave requests to review" compact />
          </BentoTile>
          <BentoTile span={6} title="Admissions">
            <EmptyState icon={UserPlus} title="No applications yet" action={<Button onClick={() => setDialog(true)}>New admission</Button>} compact>
              Applications appear here as they arrive.
            </EmptyState>
          </BentoTile>
        </BentoGrid>
      </main>

      <StepDialog
        open={dialog}
        onClose={() => setDialog(false)}
        eyebrow="Students · New admission"
        title={["Applicant details", "Programme & documents", "Fee & confirm"][step] ?? "Applicant details"}
        steps={["Applicant details", "Programme & documents", "Fee & confirm"]}
        step={step}
        onBack={() => setStep((s) => Math.max(0, s - 1))}
        primary={{ label: step === 2 ? "Submit application" : undefined, onClick: () => (step < 2 ? setStep(step + 1) : setDialog(false)) }}
        note={`Step ${step + 1} of 3`}
        data-catalogue-dialog=""
      >
        <FormGrid>
          <TextField label="Full name" placeholder="As on the Class 12 marksheet" wide />
          <DatePicker label="Date of birth" />
          <TextField label="Mobile" placeholder="98765 43210" />
          <TextField label="Email" placeholder="name@example.com" wide />
        </FormGrid>
      </StepDialog>
      <SidePanel
        open={panel}
        onClose={() => setPanel(false)}
        title="Aditi Sharma"
        eyebrow="Students · Record"
        chip={{ label: "Fee partial", kind: "review" }}
        facts={[
          { label: "Roll no.", value: "SIT25CE011" },
          { label: "Programme", value: "B.Tech Computer Engg" },
          { label: "Admitted", value: formatDate("2025-07-21") },
          { label: "Balance", value: formatINR("92500.00") },
        ]}
        stages={["Applied", "Documents verified", "Fee paid", "Enrolled"]}
        stage={2}
        actions={
          <>
            <Button>Open record</Button>
            <Button variant="ghost">Suspend</Button>
          </>
        }
      />
    </div>
  );
}
