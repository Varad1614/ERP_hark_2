/**
 * ERP_EXTENSIONS 5D wire types (docs/extensions-5d-gst.md). The 5D paths are not in the generated
 * OpenAPI client yet, so the shapes live here. Money is always a decimal string.
 */
export type GstRegistration = "regular" | "composition" | "unregistered";
export type GstTreatment = "taxable" | "nil" | "exempt" | "non_gst";
export type GstDocKind = "tax_invoice" | "bill_of_supply" | "credit_note";
export type GstComponent = "cgst" | "sgst" | "igst";

export interface GstState {
  code: string;
  name: string;
}

export interface GstTaxNotePreset {
  id: string;
  label: string;
  text: string;
}

export interface GstProfile {
  institutionId: string;
  configured: boolean;
  registrationType: GstRegistration;
  gstin: string | null;
  legalName: string;
  tradeName: string;
  address: string;
  stateCode: string;
  stateName: string | null;
  defaultTaxNote: string;
  invoicePrefix: string;
  billPrefix: string;
  creditNotePrefix: string;
  sampleNumbers: string[];
  enabled: boolean;
  version: number;
  updatedAt: string | null;
  presets: GstTaxNotePreset[];
  rates: number[];
  states: GstState[];
}

export interface GstProfileInput {
  registrationType: GstRegistration;
  gstin?: string | null;
  legalName: string;
  tradeName?: string;
  address?: string;
  stateCode?: string | null;
  defaultTaxNote?: string;
  invoicePrefix?: string;
  billPrefix?: string;
  creditNotePrefix?: string;
  enabled?: boolean;
}

export interface GstFeeHeadTax {
  sac: string;
  gstRate: string;
  exempt: boolean;
  treatment: GstTreatment;
  taxNote: string;
  taxInclusive: boolean;
  version: number;
}

export interface GstFeeHead {
  feeHeadId: string;
  code: string;
  name: string;
  category: string;
  status: string;
  tax: GstFeeHeadTax | null;
}

export interface GstFeeHeadTaxInput {
  sac: string;
  gstRate: string;
  exempt?: boolean;
  taxNote?: string;
  taxInclusive?: boolean;
}

export interface GstParty {
  studentAccountId: string;
  configured: boolean;
  stateCode: string | null;
  stateName: string | null;
  gstin: string | null;
  legalName: string;
  version: number;
}

export interface GstPartyInput {
  stateCode?: string | null;
  gstin?: string | null;
  legalName?: string;
}

export interface GstDocLine {
  lineNo: number;
  description: string;
  sac: string | null;
  treatment: GstTreatment;
  gstRate: string;
  taxableValue: string;
  cgst: string;
  sgst: string;
  igst: string;
  total: string;
}

export interface GstDocTax {
  component: GstComponent;
  label: string;
  rate: string;
  amount: string;
}

export interface GstTaxDocument {
  id: string;
  kind: GstDocKind;
  title: string;
  status: string;
  number: string | null;
  date: string | null;
  dateDisplay: string | null;
  financialYear: string | null;
  chargeId: string;
  chargeNumber: string;
  adjustmentNumber: string | null;
  supplier: { legalName: string; tradeName: string; gstin: string | null; address: string; stateCode: string; stateName: string | null };
  recipient: { name: string; studentNumber: string | null; gstin: string | null; stateCode: string; stateName: string | null };
  placeOfSupply: string;
  interState: boolean;
  reverseCharge: boolean;
  lines: GstDocLine[];
  taxes: GstDocTax[];
  taxableValue: string;
  cgst: string;
  sgst: string;
  igst: string;
  totalTax: string;
  total: string;
  totalDisplay: string;
  amountInWords: string;
  taxNote: string;
  original: { id: string; number: string | null; date: string | null } | null;
}

export interface GstAmounts {
  taxableValue: string;
  cgst: string;
  sgst: string;
  igst: string;
  totalTax: string;
}

export interface GstSummary {
  institutionId: string;
  gstin: string | null;
  from: string;
  to: string;
  b2b: { recipientGstin: string; recipientName: string; number: string; date: string; invoiceValue: string; placeOfSupply: string; rate: string; amounts: GstAmounts }[];
  b2c: { placeOfSupply: string; interState: boolean; rate: string; amounts: GstAmounts }[];
  hsn: { sac: string; rate: string; count: number; amounts: GstAmounts }[];
  exempt: { intraNil: string; intraExempt: string; intraNonGst: string; interNil: string; interExempt: string; interNonGst: string };
  creditNotes: { registered: boolean; recipientGstin: string | null; recipientName: string; number: string; date: string; originalNumber: string | null; originalDate: string | null; placeOfSupply: string; noteValue: string; amounts: GstAmounts }[];
  documents: { kind: GstDocKind; from: string; to: string; total: number; cancelled: number }[];
  net: GstAmounts;
}

export interface GstPeriod {
  from: string;
  to: string;
}
