/** Native metrology carries its own definition and validity. */
export interface NativeMeasurement {
    metric_type: string; unit: string; value: number | null; state: string; reason?: string | null;
    reference_bandwidth_hz: number | null; location: string; method: string;
    averaging_seconds: number | null; max_age_seconds?: number;
    quality?: string; quality_reasons?: string[]; value_kind?: string;
    [key: string]: unknown;
}
