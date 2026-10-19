/**
 * Audit remediation 2026-10, slice A2 (SEC-1 / C-1): an institution / person / course name that
 * carries markup is rendered as literal text everywhere it can appear in the web client — the
 * tenant switcher `<select>` of Campus / Reports (AreaFrame), the shared Select / FormSelect
 * pickers, the page summary, the careers page rich text, the receipt print preview and the CSV
 * exports. No element is created from the name and no handler runs.
 *
 * (The server now refuses `<` / `>` in identity names; these tests cover rows stored before that.)
 */
import { FormSelect, Select } from "@sutra/ui";
import { render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const FLAG = "__sutraXss";
const PAYLOADS = [
  `<img src=x onerror="window.${FLAG}=1">`,
  `<svg onload="window.${FLAG}=1"></svg>`,
  `"><script>window.${FLAG}=1</script>`,
  `<a href="javascript:window.${FLAG}=1">x</a>`,
];

type Flagged = Window & { [FLAG]?: number };

function noInjectedElements(root: ParentNode) {
  expect(root.querySelector("img, script, iframe, object, embed")).toBeNull();
  expect(root.querySelector("[onerror], [onload], [onclick]")).toBeNull();
  expect(root.querySelector('a[href^="javascript:"]')).toBeNull();
}

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
  window.localStorage.clear();
  delete (window as Flagged)[FLAG];
});
afterEach(() => {
  expect((window as Flagged)[FLAG]).toBeUndefined();
});

describe("SEC-1: names render as text, never as markup", () => {
  it.each(PAYLOADS)("shared Select / FormSelect options: %s", (name) => {
    const { container } = render(
      <>
        <Select label="Institution" options={[{ value: "a", label: name }, { value: "b", label: "Sutra Institute" }]} defaultValue="a" onChange={() => undefined} />
        <FormSelect label="Institution" options={[{ value: "a", label: name }]} defaultValue="a" onChange={() => undefined} />
      </>,
    );
    const options = [...container.querySelectorAll("option")];
    expect(options.filter((o) => o.textContent === name)).toHaveLength(2);
    noInjectedElements(container);
  });

  it("the Campus / Reports tenant switcher shows a markup name literally", async () => {
    const name = PAYLOADS[0]!;
    const { createMockInstitutionApi } = await import("./mock");
    const { InstitutionApiProvider, selectInstitution } = await import("./hooks");
    const { AreaFrame } = await import("../campus/AreaFrame");
    const renderScreen = (await import("../../test/harness")).renderScreen;
    const api = createMockInstitutionApi();
    const evil = await api.registerInstitution({ code: "XSSEXAM", name, timezone: "Asia/Kolkata" });
    await api.registerInstitution({ code: "SIT", name: "Sutra Institute of Technology", timezone: "Asia/Kolkata" });
    selectInstitution(evil.id);
    const { container } = await renderScreen(
      <InstitutionApiProvider api={api}>
        <AreaFrame screen="campus" title="Campus" summary={<span data-summary>{name}</span>} tabs={[{ id: "overview", label: "Overview", to: "/campus" }]} activeTab="overview">
          {(inst) => <p data-current>{inst?.name}</p>}
        </AreaFrame>
      </InstitutionApiProvider>,
      { path: "/campus" },
    );
    const select = await waitFor(() => {
      const s = container.querySelector('select[name="institution"]') as HTMLSelectElement | null;
      if (!s) throw new Error("no switcher yet");
      return s;
    });
    const labels = [...select.options].map((o) => o.textContent);
    expect(labels).toContain(name);
    expect(select.value).toBe(evil.id);
    expect((await screen.findAllByText(name)).length).toBeGreaterThan(0);
    noInjectedElements(container);
  });

  it("careers rich text decodes server-escaped markup into text", async () => {
    const { RichText } = await import("../people/hiring/CareersPage");
    const name = PAYLOADS[0]!;
    // What `careers::description_html` sends for that line (escape_html).
    const escaped = name.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
    const { container } = render(<RichText html={`<p>${escaped}</p><ul><li>${escaped}</li></ul>`} />);
    expect(container.querySelector("p")?.textContent).toBe(name);
    expect(container.querySelector("li")?.textContent).toBe(name);
    noInjectedElements(container);
  });

  it("the receipt print preview escapes the title and the slip text", async () => {
    const { previewHtml } = await import("../finance/printing/printer");
    const name = PAYLOADS[0]!;
    const html = previewHtml(`Received from ${name}`, 48, name);
    expect(html).not.toContain("<img");
    expect(html).toContain("&lt;img src=x onerror=");
    const doc = new DOMParser().parseFromString(html, "text/html");
    expect(doc.title).toBe(name);
    expect(doc.querySelector("pre")?.textContent).toBe(`Received from ${name}`);
    noInjectedElements(doc.body);
  });

  it("CSV exports neutralise formula-looking names", async () => {
    const { csvRow } = await import("../../lib/csv");
    expect(csvRow(['=HYPERLINK("http://x")', "@cmd", "+1+1", "-1250.50", "Sutra"])).toBe(`"'=HYPERLINK(""http://x"")",'@cmd,'+1+1,-1250.50,Sutra`);
  });
});
