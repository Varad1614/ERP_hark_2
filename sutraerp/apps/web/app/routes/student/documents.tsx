import { BentoGrid } from "@sutra/ui";
import { StudentDocumentsScreen } from "../../features/student/record";

export function meta() {
  return [{ title: "Documents · SutraERP" }];
}

/** Student-facing Documents (`/documents`): own documents and certificate requests. Slice 3A. */
export default function Documents() {
  return (
    <BentoGrid data-screen="documents">
      <StudentDocumentsScreen />
    </BentoGrid>
  );
}
