/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * A typed navigation target. Clients never need to interpret an arbitrary URL.
 */
export type InboxDeepLink =
  | {
      approval_id: string;
      type: "Approval";
    }
  | {
      question_id: string;
      type: "Question";
    }
  | {
      session_id: string;
      type: "Session";
    }
  | {
      run_id: string;
      session_id: string;
      type: "Run";
    }
  | {
      type: "Workflow";
      workflow_id: string;
    }
  | {
      plugin_id: string;
      type: "Plugin";
    }
  | {
      repository_id: string;
      type: "Repository";
    }
  | {
      type: "Unknown";
    };
/**
 * The human work or notification represented by an inbox entry.
 */
export type InboxEntryKind =
  | {
      type: "ApprovalRequest";
    }
  | {
      type: "AgentQuestion";
    }
  | {
      type: "RunCompleted";
    }
  | {
      type: "RunFailed";
    }
  | {
      type: "BudgetWarning";
    }
  | {
      type: "WorkflowBlocked";
    }
  | {
      type: "PluginPermissionChanged";
    }
  | {
      type: "RunnerFailed";
    }
  | {
      type: "Unknown";
    };
/**
 * Durable source identity from which the daemon derives the deduplication key.
 */
export type InboxSourceIdentity =
  | {
      approval_id: string;
      type: "Approval";
    }
  | {
      question_id: string;
      type: "Question";
    }
  | {
      run_id: string;
      type: "Run";
    }
  | {
      budget_id: string;
      type: "Budget";
    }
  | {
      type: "Workflow";
      workflow_id: string;
    }
  | {
      plugin_id: string;
      type: "Plugin";
    }
  | {
      runner_id: string;
      type: "Runner";
    }
  | {
      type: "Unknown";
    };
/**
 * Read/lifecycle state of a durable inbox entry.
 */
export type InboxEntryState =
  | {
      type: "Unread";
    }
  | {
      type: "Acknowledged";
    }
  | {
      type: "Dismissed";
    }
  | {
      type: "Resolved";
    }
  | {
      type: "Unknown";
    };
/**
 * Idempotent state change requested for an inbox entry.
 */
export type InboxMutation =
  | {
      entry_id: string;
      type: "Acknowledge";
    }
  | {
      entry_id: string;
      type: "Dismiss";
    }
  | {
      type: "Unknown";
    };

interface InboxCatalog {
  entry: InboxEntry;
  mutation: InboxMutation;
  page: InboxPage;
  query: InboxListQuery;
}
/**
 * Repository-authorized client projection of an inbox row.
 *
 * There is intentionally no `owner_id`: authorization and owner scoping are repository concerns and cannot be selected or asserted by a client.
 */
export interface InboxEntry {
  acknowledged_at?: string | null;
  created_at: string;
  deep_link: InboxDeepLink;
  dismissed_at?: string | null;
  id: string;
  kind: InboxEntryKind;
  repository_id: string;
  /**
   * Set only when the authoritative source operation resolves. Inbox acknowledgement and dismissal never decide an approval or question.
   */
  resolved_at?: string | null;
  source: InboxSource;
  state?: InboxEntryState;
  summary?: string;
  title: string;
}
/**
 * Stable provenance used by the repository to deduplicate an entry.
 */
export interface InboxSource {
  /**
   * Stable within an owner. Replaying the same source must reuse this key.
   */
  dedup_key: string;
  identity: InboxSourceIdentity;
  run_id?: string | null;
  session_id?: string | null;
  workflow_id?: string | null;
}
/**
 * A cursor page returned by an inbox list operation.
 */
export interface InboxPage {
  items: InboxEntry[];
  next_cursor?: string | null;
}
/**
 * Cursor-based inbox list request.
 */
export interface InboxListQuery {
  cursor?: string | null;
  filters?: InboxListFilters;
  limit?: number | null;
}
/**
 * Optional list restrictions. An empty value means all authorized entries.
 */
export interface InboxListFilters {
  kinds?: InboxEntryKind[];
  repository_ids?: string[];
  states?: InboxEntryState[];
}

type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
