/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * Supported analytics export encodings. The request must choose explicitly.
 */
export type AnalyticsExportFormat =
  | {
      type: "json";
    }
  | {
      type: "csv";
    }
  | {
      type: "unknown";
    };
/**
 * Completion outcome used both as a filter and an aggregate dimension.
 */
export type AnalyticsCompletion =
  | {
      type: "successful";
    }
  | {
      type: "failed";
    }
  | {
      type: "cancelled";
    }
  | {
      type: "incomplete";
    }
  | {
      type: "unknown";
    };
/**
 * Dimensions by which observations may be grouped.
 */
export type AnalyticsGrouping =
  | {
      type: "model";
    }
  | {
      type: "provider";
    }
  | {
      type: "repository";
    }
  | {
      type: "workflow";
    }
  | {
      type: "task_class";
    }
  | {
      type: "time";
    }
  | {
      type: "completion";
    }
  | {
      type: "route";
    }
  | {
      type: "unknown";
    };
/**
 * How sensitive an artifact's contents are.
 *
 * Ordered least to most restrictive; higher classifications gate model routing, export, and display. A wire enum, so it is internally tagged and carries an [`DataClassification::Unknown`] fallback for forward compatibility.
 */
type DataClassification =
  | {
      type: "Public";
    }
  | {
      type: "Internal";
    }
  | {
      type: "Confidential";
    }
  | {
      type: "Secret";
    }
  | {
      type: "Unknown";
    };

interface AnalyticsCatalog {
  bucket: AnalyticsBucket;
  export_request: AnalyticsExportRequest;
  export_result: AnalyticsExportResult;
  page: AnalyticsPage;
  query: AnalyticsQuery;
}
/**
 * A result bucket. Dimension keys correspond in order to `query.group_by`.
 */
export interface AnalyticsBucket {
  dimensions?: string[];
  metrics: AnalyticsMetrics;
}
/**
 * Aggregate values for a grouping bucket.
 */
export interface AnalyticsMetrics {
  cached_tokens?: number | null;
  completion_count?: number | null;
  /**
   * Measured USD cost in millionths of a dollar.
   */
  cost_micros?: number | null;
  cost_per_successful_task_micros?: number | null;
  coverage?: AnalyticsDimensionCoverage;
  escalation_count?: number | null;
  grader_score_micros?: number | null;
  input_tokens?: number | null;
  latency_ms?: number | null;
  output_tokens?: number | null;
  reasoning_tokens?: number | null;
  retry_count?: number | null;
}
/**
 * Coverage is explicit per nullable metric, making partial aggregates visible.
 */
export interface AnalyticsDimensionCoverage {
  cached_tokens: MeasurementCoverage;
  completion_count: MeasurementCoverage;
  cost: MeasurementCoverage;
  cost_per_successful_task: MeasurementCoverage;
  escalation_count: MeasurementCoverage;
  grader_score: MeasurementCoverage;
  input_tokens: MeasurementCoverage;
  latency: MeasurementCoverage;
  output_tokens: MeasurementCoverage;
  reasoning_tokens: MeasurementCoverage;
  retry_count: MeasurementCoverage;
}
/**
 * Number of observations for which a dimension was measured.
 */
export interface MeasurementCoverage {
  measured: number;
  total: number;
}
/**
 * Request for a server-bounded export of an analytics query.
 */
export interface AnalyticsExportRequest {
  format: AnalyticsExportFormat;
  /**
   * Requested row ceiling. The server may impose a smaller ceiling; zero selects the server default.
   */
  max_rows?: number;
  query: AnalyticsQuery;
}
/**
 * A bounded, cursor-paged aggregate query.
 */
export interface AnalyticsQuery {
  cursor?: string | null;
  filters?: AnalyticsFilters;
  group_by?: AnalyticsGrouping[];
  /**
   * Requested page size. The server applies its own upper bound; zero means the server default.
   */
  limit?: number;
}
/**
 * Optional restrictions on observations. Empty lists do not restrict.
 */
export interface AnalyticsFilters {
  completions?: AnalyticsCompletion[];
  models?: string[];
  providers?: string[];
  repositories?: string[];
  routes?: string[];
  task_classes?: string[];
  time?: AnalyticsTimeRange | null;
  workflows?: string[];
}
/**
 * Inclusive start and exclusive end of an analytics query.
 */
export interface AnalyticsTimeRange {
  from?: string | null;
  until?: string | null;
}
/**
 * Metadata for a completed export. Bulk JSON/CSV bytes live in the artifact.
 */
export interface AnalyticsExportResult {
  artifact: ArtifactRef;
  format: AnalyticsExportFormat;
  generated_at: string;
  row_count: number;
  truncated?: boolean;
}
/**
 * A pointer to a stored artifact plus the metadata needed to handle it safely.
 *
 * `id` and `sha256` are deliberately independent: identical bytes dedup to one blob (keyed by `sha256`) but every occurrence is its own `ArtifactRef` with its own id and `sensitivity` (Chapter 14 / STEP 1.4). Classification checks always read the ref in hand, never a row looked up by hash.
 */
interface ArtifactRef {
  byte_length: number;
  id: string;
  /**
   * IANA media type, e.g. `text/plain` or `application/json`.
   */
  media_type: string;
  sensitivity: DataClassification;
  /**
   * Lowercase hex SHA-256 of the blob's bytes (the content address).
   */
  sha256: string;
}
export interface AnalyticsPage {
  items: AnalyticsBucket[];
  next_cursor?: string | null;
}

type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
