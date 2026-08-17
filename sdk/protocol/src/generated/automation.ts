/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

export type AutomationApprovalMode =
  | ("inherit" | "always_require" | "policy_driven" | "unknown")
  | {
      preapproved: {
        approval_receipt: string;
      };
    };
export type ConcurrencyPolicy = "allow" | "skip" | "queue" | "replace" | "unknown";
export type MissedRunPolicy =
  | ("skip" | "run_once" | "unknown")
  | {
      catch_up: {
        max_occurrences: number;
      };
    };
/**
 * The event or schedule that can invoke a binding.
 */
export type TriggerSource =
  | {
      expression: string;
      timezone: string;
      type: "cron";
    }
  | {
      at: string;
      type: "one_time";
    }
  | {
      endpoint_id: string;
      events?: string[];
      installation_id?: number | null;
      type: "git_hub_webhook";
    }
  | {
      endpoint_id: string;
      signature: WebhookSignatureScheme;
      /**
       * Reference to daemon-owned secret material, never the secret itself.
       */
      signing_key_ref: string;
      type: "signed_webhook";
    }
  | {
      provider?: string | null;
      type: "ci_failure";
      workflows?: string[];
    }
  | {
      type: "repository_change";
    }
  | {
      type: "code_graph_change";
    }
  | {
      ecosystems?: string[];
      type: "dependency_alert";
    }
  | {
      type: "manual";
    }
  | {
      type: "api";
    }
  | {
      type: "unknown";
    };
export type WebhookSignatureScheme = "hmac_sha256" | "ed25519" | "unknown";
/**
 * Normalized CRUD requests. The containing command provides idempotency.
 */
export type AutomationBindingRequest =
  | {
      binding: AutomationBindingDraft;
      type: "create";
    }
  | {
      id: string;
      type: "get";
    }
  | {
      query: AutomationBindingQuery;
      type: "list";
    }
  | {
      id: string;
      patch: AutomationBindingPatch;
      type: "update";
    }
  | {
      id: string;
      type: "delete";
    }
  | {
      type: "unknown";
    };

interface AutomationCatalog {
  binding: AutomationBinding;
  page: AutomationBindingPage;
  query: AutomationBindingQuery;
  request: AutomationBindingRequest;
  source: TriggerSource;
}
export interface AutomationBinding {
  created_at: string;
  enabled?: boolean;
  filters?: TriggerFilters;
  id: string;
  invocation?: InvocationPolicy;
  name: string;
  repository_id: string;
  source: TriggerSource;
  updated_at: string;
  workflow_id: string;
  workflow_version: string;
}
/**
 * Common source filters. Values are public event metadata, never credentials.
 */
export interface TriggerFilters {
  actors?: string[];
  branches?: string[];
  labels?: string[];
  metadata?: {
    [k: string]: string | undefined;
  };
  paths?: string[];
}
/**
 * Per-binding invocation controls, independent of the workflow definition.
 */
export interface InvocationPolicy {
  approval_mode?: AutomationApprovalMode & string;
  budget_ceiling?: BudgetCeiling | null;
  concurrency?: ConcurrencyPolicy & string;
  deduplication?: DeduplicationPolicy;
  missed_run?: MissedRunPolicy & string;
  retry?: TriggerRetryPolicy;
}
export interface BudgetCeiling {
  cost_micros?: number | null;
  tokens?: number | null;
  tool_calls?: number | null;
  wall_time_seconds?: number | null;
}
export interface DeduplicationPolicy {
  /**
   * Names of normalized event fields which form the identity.
   */
  identity_fields?: string[];
  window_seconds?: number;
}
export interface TriggerRetryPolicy {
  backoff_multiplier?: number;
  initial_delay_seconds?: number;
  max_attempts?: number;
  max_delay_seconds?: number | null;
}
export interface AutomationBindingPage {
  items: AutomationBinding[];
  next_cursor?: string | null;
}
export interface AutomationBindingQuery {
  cursor?: string | null;
  enabled?: boolean | null;
  limit?: number | null;
  repository_id?: string | null;
  workflow_id?: string | null;
}
export interface AutomationBindingDraft {
  enabled?: boolean;
  filters?: TriggerFilters;
  invocation?: InvocationPolicy;
  name: string;
  repository_id: string;
  source: TriggerSource;
  workflow_id: string;
  workflow_version: string;
}
/**
 * Sparse update. Nested policy values are replaced as a normalized unit.
 */
export interface AutomationBindingPatch {
  enabled?: boolean | null;
  filters?: TriggerFilters | null;
  invocation?: InvocationPolicy | null;
  name?: string | null;
  repository_id?: string | null;
  source?: TriggerSource | null;
  workflow_id?: string | null;
  workflow_version?: string | null;
}

type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
