import { AreaHeader, BentoGrid, BentoTile } from "@sutra/ui";
import { StudentLibraryScreen } from "../../features/academic/flexible";
import { MyLibraryPanel, useMyLibrarySummary } from "../../features/campus/library";
import { useSession } from "../../lib/auth";

export function meta() {
  return [{ title: "Library · SutraERP" }];
}

/**
 * Student Library (`/library`), re-skin v2 (R7) — the prototype `stu-library.png`: header with a live
 * summary, Borrowed (Renew) and Catalogue (search, Available / Issued, Reserve) from the Batch 8
 * library panel (slice 8C, `MyLibraryPanel`), then reservations / fines / clearance when there are
 * any, and the Batch 4 course resources and library links (slice 4C) below.
 */
export default function Screen() {
  const { role } = useSession();
  const summary = useMyLibrarySummary(role === "student");
  if (role !== "student")
    return (
      <BentoGrid data-screen="library">
        <BentoTile span={12} variant="accent" className="sb-soon">
          <h1>This screen isn’t available</h1>
          <p>Library is the students’ view of the library and course resources.</p>
        </BentoTile>
      </BentoGrid>
    );
  return (
    <BentoGrid data-screen="library">
      <AreaHeader title="Library" summary={summary ?? "Your loans, the catalogue and your course reading"} data-page-head="library" />
      <MyLibraryPanel />
      <StudentLibraryScreen />
    </BentoGrid>
  );
}
