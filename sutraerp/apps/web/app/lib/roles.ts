/**
 * Role personas and Home-dashboard content, ported from the Soft Bento prototype.
 *
 * These are presentation fixtures for Batch 0: every value here is replaced by data from the
 * owning module's API as its batch lands (see docs/batches.md). Nothing in this file is business
 * truth (ADR-L5-0015: the client is non-authoritative).
 */
import type { LucideIcon } from "lucide-react";
import type { BadgeKind, Tone } from "@sutra/ui";
import type { Role } from "@sutra/api-client";
import { Icons as I } from "./icons";

export type { Role };
export const ROLES: readonly Role[] = ["administrator", "faculty", "student"];

export interface NavEntry {
  id: string;
  label: string;
  icon: LucideIcon;
  /** Other screens that light up this pill (e.g. Academics for /academic-delivery). */
  match?: string[];
}
/**
 * A home KPI tile: label, icon and destination only. 9E: the fixture numbers are gone — every
 * value is read live in `features/home/live.tsx`, and a figure without a source renders pending.
 */
export interface Kpi {
  icon: LucideIcon;
  tone: Tone;
  label: string;
  to: string;
}
export interface Todo {
  title: string;
  meta: string;
  status: Extract<BadgeKind, "attention" | "review" | "neutral">;
}
export interface Shortcut {
  icon: LucideIcon;
  label: string;
  to: string;
}
export interface Notification {
  icon: LucideIcon;
  tone: Tone;
  title: string;
  meta: string;
}

export interface RoleConfig {
  title: string;
  name: string;
  first: string;
  initials: string;
  loginId: string;
  blurb: string;
  nav: NavEntry[];
  hero: { sub: string; cta: string; to: string };
  now: { time: string; title: string; sub: string; next: Array<[string, string, string]> };
  kpis: Kpi[];
  todos: Todo[];
  shortcuts: Shortcut[];
  notifications: Notification[];
}

const nav = (id: string, label: string, icon: LucideIcon, match?: string[]): NavEntry => (match ? { id, label, icon, match } : { id, label, icon });

export const ROLE_CONFIG: Record<Role, RoleConfig> = {
  administrator: {
    title: "Administrator",
    name: "Dhruv Naik",
    first: "Dhruv",
    initials: "DN",
    loginId: "admin@sutra.dev",
    blurb: "Institution overview, approvals, finance and people.",
    nav: [
      nav("home", "Home", I.House), nav("students", "Students", I.GraduationCap), nav("academics", "Academics", I.BookOpen, ["/academic-delivery"]),
      nav("examinations", "Examinations", I.ClipboardCheck), nav("finance", "Finance", I.Wallet), nav("people", "People", I.Users),
      nav("campus", "Campus", I.Building2), nav("reports", "Reports", I.ChartBar), nav("workspaces", "Workspaces", I.LayoutGrid),
    ],
    hero: { sub: "A clear view of your institution, and what needs you today.", cta: "Review priority work", to: "todo:0" },
    now: { time: "11:00", title: "Admissions check-in", sub: "Registrar’s office · 30 min", next: [["09:30", "Department coordinators", "Room 2"], ["14:30", "Academic planning", "45 min"]] },
    kpis: [
      { icon: I.GraduationCap, tone: "orange", label: "Students enrolled", to: "students" },
      { icon: I.Users, tone: "green", label: "Faculty on roll", to: "people" },
      { icon: I.Wallet, tone: "violet", label: "Fee collection", to: "finance" },
      { icon: I.Building2, tone: "blue", label: "Open work orders", to: "campus/facilities" },
    ],
    todos: [
      { title: "Admission documents to review", meta: "Students · Registrar’s office · Today, 4:00 PM", status: "attention" },
      { title: "Fee adjustments for review", meta: "Finance · Accounts office · Today", status: "review" },
      { title: "Result verification", meta: "Examinations · Exam cell · 18 Sep", status: "review" },
      { title: "Staff leave requests", meta: "People · HR operations · 19 Sep", status: "neutral" },
    ],
    shortcuts: [
      { icon: I.UserPlus, label: "New admission", to: "students/admissions" },
      { icon: I.Wallet, label: "Fee overview", to: "finance" },
      { icon: I.CalendarCheck, label: "Exam calendar", to: "examinations" },
      { icon: I.Contact, label: "Staff directory", to: "people" },
    ],
    notifications: [
      { icon: I.FileText, tone: "orange", title: "14 admission files uploaded", meta: "Registrar’s office · 10 min ago" },
      { icon: I.Wallet, tone: "green", title: "Fee batch reconciled: ₹42.6 L", meta: "Accounts office · 1 h ago" },
      { icon: I.ClipboardCheck, tone: "violet", title: "Semester 3 results ready to verify", meta: "Exam cell · 3 h ago" },
      { icon: I.Plane, tone: "rose", title: "6 staff leave requests pending", meta: "HR operations · Yesterday" },
    ],
  },
  faculty: {
    title: "Faculty",
    name: "Dr. Meera Shah",
    first: "Meera",
    initials: "MS",
    loginId: "meera.shah@sutra.dev",
    blurb: "Classes, attendance, assessments and mentoring.",
    nav: [
      nav("home", "Home", I.House), nav("classes", "My classes", I.Presentation), nav("attendance", "Attendance", I.UserCheck),
      nav("assessments", "Assessments", I.PencilLine), nav("my-students", "Students", I.GraduationCap), nav("timetable", "Timetable", I.CalendarDays),
      nav("leave", "Leave", I.Plane), nav("resources", "Resources", I.Library), nav("workspaces", "Workspaces", I.LayoutGrid),
    ],
    hero: { sub: "Your teaching day, with the details that matter.", cta: "Open next class", to: "classes" },
    now: { time: "10:30", title: "Data Structures", sub: "CS201 · Section A · B-204", next: [["09:00", "Database Systems", "B-102"], ["14:00", "Student consultation", "60 min"]] },
    kpis: [
      { icon: I.Presentation, tone: "orange", label: "Classes today", to: "classes" },
      { icon: I.Users, tone: "green", label: "My students", to: "my-students" },
      { icon: I.Inbox, tone: "violet", label: "To review", to: "assessments" },
      { icon: I.UserCheck, tone: "blue", label: "Avg attendance", to: "attendance" },
    ],
    todos: [
      { title: "Take attendance · Data Structures", meta: "CS201 · Section A · B-204 · Today, 10:30 AM", status: "attention" },
      { title: "Review lab submissions", meta: "Linked lists lab · Today", status: "review" },
      { title: "Student check-ins", meta: "Academic mentoring · 17 Sep", status: "neutral" },
      { title: "Assessment material review", meta: "CS201 · Unit 2 · 18 Sep", status: "review" },
    ],
    shortcuts: [
      { icon: I.UserCheck, label: "Mark attendance", to: "attendance" },
      { icon: I.Upload, label: "Upload material", to: "resources" },
      { icon: I.Table2, label: "Gradebook", to: "assessments" },
      { icon: I.Plane, label: "Apply leave", to: "leave" },
    ],
    notifications: [
      { icon: I.Inbox, tone: "violet", title: "5 new lab submissions", meta: "CS201 · Linked lists lab · 20 min ago" },
      { icon: I.CalendarDays, tone: "blue", title: "Room change: Database Systems → B-104", meta: "Timetable office · 2 h ago" },
      { icon: I.UserCheck, tone: "orange", title: "Attendance pending for CS203", meta: "Yesterday’s session · 5 h ago" },
      { icon: I.Megaphone, tone: "green", title: "Innovation Week judges needed", meta: "Student affairs · Yesterday" },
    ],
  },
  student: {
    title: "Student",
    name: "Aarav Desai",
    first: "Aarav",
    initials: "AD",
    loginId: "aarav.desai@sutra.dev",
    blurb: "Timetable, coursework, results and fees.",
    nav: [
      nav("home", "Home", I.House), nav("timetable", "Timetable", I.CalendarDays), nav("courses", "Courses", I.BookOpen, ["/registration"]),
      nav("assignments", "Assignments", I.PencilLine), nav("results", "Results", I.Award), nav("fees", "Fees", I.Wallet),
      nav("documents", "Documents", I.FileText), nav("library", "Library", I.Library), nav("workspaces", "Workspaces", I.LayoutGrid),
    ],
    hero: { sub: "Make room for learning. Your day is right here.", cta: "View my timetable", to: "timetable" },
    now: { time: "10:30", title: "Data Structures", sub: "Dr. Meera Shah · B-204", next: [["09:00", "Mathematics II", "A-103"], ["13:30", "Database Systems lab", "Lab 2"]] },
    kpis: [
      { icon: I.UserCheck, tone: "orange", label: "Attendance", to: "timetable" },
      { icon: I.Award, tone: "green", label: "CGPA", to: "results" },
      { icon: I.PencilLine, tone: "violet", label: "Due this week", to: "assignments" },
      { icon: I.Wallet, tone: "blue", label: "Fees", to: "fees" },
    ],
    todos: [
      { title: "Linked lists: practice assignment", meta: "Data Structures · CS201 · Today, 5:00 PM", status: "attention" },
      { title: "Read: relational database design", meta: "Database Systems · CS203 · 17 Sep", status: "neutral" },
      { title: "Mathematics II assessment", meta: "MA202 · Unit assessment · 21 Sep", status: "review" },
    ],
    shortcuts: [
      { icon: I.CalendarDays, label: "Timetable", to: "timetable" },
      { icon: I.Award, label: "Results", to: "results" },
      { icon: I.CreditCard, label: "Pay fees", to: "fees" },
      { icon: I.FilePlus, label: "Request document", to: "documents" },
    ],
    notifications: [
      { icon: I.PencilLine, tone: "violet", title: "Linked lists assignment due today", meta: "CS201 · 5:00 PM" },
      { icon: I.Award, tone: "green", title: "Semester 2 grade card published", meta: "Exam cell · 1 h ago" },
      { icon: I.Library, tone: "amber", title: "“Database System Concepts” due back 19 Sep", meta: "Library · 4 h ago" },
      { icon: I.Megaphone, tone: "orange", title: "Innovation Week registrations open", meta: "Student affairs · Yesterday" },
    ],
  },
};

export const ANNOUNCEMENT = { title: "Innovation Week begins 23 September", meta: "Student affairs · 23–25 Sep, Main campus" };

export const BADGE_LABEL: Record<Todo["status"], string> = { attention: "Due soon", review: "In review", neutral: "Upcoming" };

/** Batch that delivers each role screen (docs/batches.md). */
export const SCREEN_BATCH: Record<string, { batch: number; area: string }> = {
  students: { batch: 3, area: "Student lifecycle (C)" },
  documents: { batch: 3, area: "Student lifecycle (C)" },
  registration: { batch: 3, area: "Student lifecycle (C)" },
  academics: { batch: 2, area: "Institution (B)" },
  "academic-delivery": { batch: 4, area: "Academic (D)" },
  "my-students": { batch: 4, area: "Academic (D)" },
  classes: { batch: 4, area: "Academic (D)" },
  attendance: { batch: 4, area: "Academic (D)" },
  timetable: { batch: 4, area: "Academic (D)" },
  courses: { batch: 4, area: "Academic (D)" },
  assignments: { batch: 4, area: "Academic (D)" },
  finance: { batch: 5, area: "Finance (F)" },
  fees: { batch: 5, area: "Finance (F)" },
  examinations: { batch: 6, area: "Examinations (I)" },
  assessments: { batch: 6, area: "Examinations (I)" },
  results: { batch: 6, area: "Examinations (I)" },
  people: { batch: 7, area: "People & workforce (E)" },
  leave: { batch: 7, area: "People & workforce (E)" },
  campus: { batch: 2, area: "Institution (B) campus setup; Campus operations (G) and resources (J) since Batch 8" },
  resources: { batch: 4, area: "Academic (D) course resources; Resources (J) in Batch 8" },
  library: { batch: 4, area: "Academic (D) library links; Campus library (G05) since Batch 8" },
  reports: { batch: 8, area: "Reports & analytics (H07 dashboards, H08 regulatory reporting)" },
  profile: { batch: 1, area: "Platform (A)" },
  notices: { batch: 1, area: "Platform (A)" },
  todos: { batch: 1, area: "Platform (A)" },
};

/** Screens reachable from a pill's page instead of their own pill (prototype navigation). */
export const SECONDARY_SCREENS: Record<string, { label: string; icon: LucideIcon }> = {
  "academic-delivery": { label: "Academic delivery", icon: I.Presentation },
  registration: { label: "Registration", icon: I.ClipboardCheck },
};

export function routeFor(id: string): string {
  if (id === "home") return "/";
  return `/${id}`;
}

export function isRole(value: unknown): value is Role {
  return typeof value === "string" && (ROLES as readonly string[]).includes(value);
}
