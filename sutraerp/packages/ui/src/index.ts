/**
 * @sutra/ui — Soft Bento component library for SutraERP.
 *
 * Import the stylesheet once at the app root: `import "@sutra/ui/styles.css"`.
 */
export { cx } from "./components/cx";
export { TONES, toneStyle, type Tone } from "./components/tones";
export { Button, type ButtonProps } from "./components/Button";
export { Badge, KeyTag, type BadgeKind } from "./components/Badge";
export { IconChip, type IconChipProps } from "./components/IconChip";
export { BentoGrid, BentoTile, type BentoTileProps, type Span } from "./components/BentoTile";
export { StatTile, type StatTileProps } from "./components/StatTile";
export { Sparkline } from "./components/Sparkline";
export { PageHeader } from "./components/PageHeader";
export { applyDisplayPrefs, readDisplayPrefs, useDisplayPrefs, Segmented, DEFAULT_DISPLAY_PREFS, type DisplayPrefs } from "./components/DisplayPrefs";
export { DataTable, type Column, type DataTableProps } from "./components/DataTable";
export { EmptyState } from "./components/EmptyState";
export { Drawer, type DrawerProps } from "./components/Drawer";
export { Modal, type ModalProps } from "./components/Modal";
export { ToastProvider, useToast } from "./components/Toast";
export { CommandPalette, type CommandPaletteProps, type PaletteItem } from "./components/CommandPalette";
export { Input, Select, SearchField, type SelectOption } from "./components/Input";
export { Shell, PillNav, Logo, IconButton, Avatar, type NavItem, type NavLinkLikeProps, type ShellProps } from "./components/Shell";
export { Tabs, FilterChip, type TabItem } from "./components/Tabs";
export { Popover } from "./components/Popover";
export { Steps } from "./components/Steps";
export { RoleTheme, useRoleTheme, ROLE_ACCENTS, type ThemeRole } from "./components/RoleTheme";

// ---- Re-skin v2 (docs/reskin2-contracts.md): prototype page anatomy + formatters ----
export {
  formatINR,
  formatNumber,
  formatDays,
  formatPercent,
  formatDate,
  formatDateShort,
  formatWeekdayDate,
  formatDateTime,
  formatTime,
  formatMonth,
  groupIndian,
  humanize,
  initialsOf,
  plural,
  statusKind,
  toneFor,
  type InrOptions,
} from "./components/format";
export { MenuButton, MenuList, useMenu, type MenuItem } from "./components/Menu";
export { AreaHeader, splitTabs, MAX_AREA_TABS, type AreaHeaderProps, type AreaTab } from "./components/AreaHeader";
export { FilterChips, FilterChipRows, type ChipOption } from "./components/FilterChips";
export { Initials, NameCell, type InitialsSize } from "./components/Initials";
export { ListRow, RowActions, type ListRowProps } from "./components/ListRow";
export { CardGrid, ClassCard, CourseCard, KeyValue, PersonCard, ProgressBar, ReportCard, type ClassCardProps, type PersonCardProps } from "./components/Cards";
export { ApprovalCard, ApprovalList, type ApprovalCardProps } from "./components/ApprovalCard";
export { KpiTile, type KpiTileProps } from "./components/KpiTile";
export { Balance, BarChart, DateList, StatCells, barKind, type BarDatum } from "./components/Widgets";
export { WeekGrid, type WeekSlot } from "./components/WeekGrid";
export { AttendanceGrid, countMarks, nextMark, type AttendanceStudent, type Mark } from "./components/AttendanceGrid";
export { StepDialog, Stepper, type StepDialogProps } from "./components/StepDialog";
export { Checkbox, CheckboxList, DatePicker, Field, FileField, FormGrid, FormSelect, TagInput, TextArea, TextField, Toggle, type FormSelectOption } from "./components/Form";
export { SidePanel, type SidePanelProps } from "./components/SidePanel";
export { StatusBadge } from "./components/StatusBadge";
