import { BentoGrid } from "@sutra/ui";
import { StudentRegistrationScreen } from "../../features/student/registration";

export function meta() {
  return [{ title: "Registration · SutraERP" }];
}

/** Student-facing Registration (`/registration`): self-registration and add/drop. Slice 3B. */
export default function Registration() {
  return (
    <BentoGrid data-screen="registration">
      <StudentRegistrationScreen />
    </BentoGrid>
  );
}
