import { BentoGrid, BentoTile } from "@sutra/ui";
import { useSession } from "../../lib/auth";
import { StudentRegistrationScreen } from "../../features/student/registration";

export function meta() {
  return [{ title: "Registration · SutraERP" }];
}

/** Student-facing Registration (`/registration`): self-registration and add/drop. Slice 3B. */
export default function Registration() {
  const { role } = useSession();
  if (role !== "student")
    return (
      <BentoGrid data-screen="registration">
        <BentoTile span={12} variant="accent" className="sb-soon">
          <h1>This screen isn’t available</h1>
          <p>Registration is the students’ self-service view; staff use Students → Student 360.</p>
        </BentoTile>
      </BentoGrid>
    );
  return (
    <BentoGrid data-screen="registration">
      <StudentRegistrationScreen />
    </BentoGrid>
  );
}
