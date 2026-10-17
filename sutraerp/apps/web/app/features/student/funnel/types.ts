/** ERP_EXTENSIONS 2A wire types (generated OpenAPI schemas). */
import type { components } from "@sutra/api-client";

type S = components["schemas"];

export type Enquiry = S["AdmissionEnquiry"];
export type EnquiryList = S["AdmissionEnquiryList"];
export type NewEnquiry = S["CreateEnquiry"];
export type CreatedEnquiry = S["CreatedEnquiry"];
export type EnquiryChange = S["UpdateEnquiry"];
export type FunnelStage = S["AdmissionFunnelStage"];
export type Funnel = S["AdmissionFunnel"];
export type FunnelRow = S["AdmissionFunnelRow"];
export type Forecast = S["AdmissionForecast"];
export type Duplicates = S["AdmissionDuplicates"];
export type FunnelSettings = S["AdmissionFunnelSettings"];
export type FunnelSettingsInput = S["AdmissionPutFunnelSettings"];
export type IntakeTarget = S["AdmissionFunnelIntakeTarget"];
