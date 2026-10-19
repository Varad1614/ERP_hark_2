import { BentoGrid, BentoTile } from "@sutra/ui";
import { useSession } from "../../lib/auth";
import { StudentDocumentsScreen } from "../../features/student/record";

export function meta() {
  return [{ title: "Documents · SutraERP" }];
}

/** Student-facing Documents (`/documents`): own documents and certificate requests. Slice 3A. */
export default function Documents() {
  const { role } = useSession();
  if (role !== "student")
    return (
      <BentoGrid data-screen="documents">
        <BentoTile span={12} variant="accent" className="sb-soon">
          <h1>This screen isn’t available</h1>
          <p>Documents is the students’ self-service view; staff use Students → Student 360.</p>
        </BentoTile>
      </BentoGrid>
    );
  return (
    <BentoGrid data-screen="documents">
      <StudentDocumentsScreen />
    </BentoGrid>
  );
}
