import { useParams } from "react-router";
import { CareersPage } from "../features/people/hiring/CareersPage";
import "../features/home/home.css";

/** Public careers page (outside `app-shell`, so no auth guard); 5A. */
export function meta() {
  return [{ title: "Careers · SutraERP" }];
}

export default function Careers() {
  const { slug = "" } = useParams();
  return (
    <main className="mx-auto max-w-4xl p-4">
      <CareersPage slug={slug} />
    </main>
  );
}
