//! Export handler producing NDJSON or CSV summaries (Milestone 3, Task 3.4).

use std::fmt::Write as _;

use chrono::Utc;
use codypendent_protocol::{
    AnalyticsExportFormat, AnalyticsExportRequest, AnalyticsExportResult, ClientId,
    CodypendentError, CommandId, DataClassification,
};
use sqlx::SqlitePool;

use crate::analytics::{query, AnalyticsError};
use crate::artifacts::{ArtifactStore, Provenance};
use crate::principal::PeerPrincipal;

pub const DEFAULT_MAX_EXPORT_ROWS: u32 = 1_000;
pub const SERVER_MAX_EXPORT_ROWS: u32 = 10_000;

/// Escape a cell for CSV export, preventing CSV formula injection.
///
/// If a cell begins with '=', '+', '-', '@', tab, or CR, it is prefixed with a single quote.
pub fn escape_csv_cell(value: &str) -> String {
    let needs_formula_escape = value.starts_with('=')
        || value.starts_with('+')
        || value.starts_with('-')
        || value.starts_with('@')
        || value.starts_with('\t')
        || value.starts_with('\r');

    let sanitized = if needs_formula_escape {
        format!("'{value}")
    } else {
        value.to_string()
    };

    if sanitized.contains(',')
        || sanitized.contains('"')
        || sanitized.contains('\n')
        || sanitized.contains('\r')
    {
        format!("\"{}\"", sanitized.replace('"', "\"\""))
    } else {
        sanitized
    }
}

/// Execute an analytics export, bounded by server ceilings and producing an artifact.
pub async fn export(
    pool: &SqlitePool,
    artifacts: &ArtifactStore,
    daemon_uid: u32,
    principal: PeerPrincipal,
    _client_id: ClientId,
    _command_id: CommandId,
    request: &AnalyticsExportRequest,
) -> Result<AnalyticsExportResult, AnalyticsError> {
    if matches!(request.format, AnalyticsExportFormat::Unknown) {
        return Err(AnalyticsError::UnsupportedFormat);
    }

    let max_rows = if request.max_rows == 0 {
        DEFAULT_MAX_EXPORT_ROWS
    } else {
        request.max_rows.min(SERVER_MAX_EXPORT_ROWS)
    };

    // Paged, not asked for in one gulp. `query` clamps any request to its own
    // page ceiling, so a single call for `max_rows + 1` came back holding one
    // page and no more — every export beyond that ceiling silently shipped a
    // page-sized prefix, and `truncated` compared a clamped length against a
    // larger limit and so was always false. An export that quietly drops rows
    // and calls itself complete is worse than one that refuses.
    //
    // So follow the cursor the query already hands back until the export
    // ceiling is reached or the source runs out, and let the ceiling do the
    // truncating.
    let mut query_def = request.query.clone();
    query_def.limit = 0; // let `query` choose its own page size
    query_def.cursor = None;

    let mut items = Vec::new();
    let truncated = loop {
        let page = query(pool, daemon_uid, principal, &query_def).await?;
        let exhausted = page.next_cursor.is_none();
        items.extend(page.items);
        if items.len() > max_rows as usize {
            items.truncate(max_rows as usize);
            break true;
        }
        if exhausted {
            break false;
        }
        query_def.cursor = page.next_cursor;
    };
    let row_count = items.len() as u64;

    let (media_type, bytes) = match request.format {
        AnalyticsExportFormat::Json => {
            let mut buffer = String::new();
            for item in &items {
                let json = serde_json::to_string(item)
                    .map_err(|e| AnalyticsError::InvalidData(e.to_string()))?;
                buffer.push_str(&json);
                buffer.push('\n');
            }
            ("application/x-ndjson", buffer.into_bytes())
        }
        AnalyticsExportFormat::Csv => {
            let mut csv = String::new();
            csv.push_str("dimensions,input_tokens,output_tokens,cached_tokens,reasoning_tokens,cost_micros,latency_ms,grader_score_micros,cost_per_successful_task_micros,retry_count,escalation_count,completion_count\n");
            for item in &items {
                let dim_str = item.dimensions.join(";");
                let escaped_dim = escape_csv_cell(&dim_str);
                let _ = writeln!(
                    csv,
                    "{},{},{},{},{},{},{},{},{},{},{},{}",
                    escaped_dim,
                    item.metrics
                        .input_tokens
                        .map_or(String::new(), |v| v.to_string()),
                    item.metrics
                        .output_tokens
                        .map_or(String::new(), |v| v.to_string()),
                    item.metrics
                        .cached_tokens
                        .map_or(String::new(), |v| v.to_string()),
                    item.metrics
                        .reasoning_tokens
                        .map_or(String::new(), |v| v.to_string()),
                    item.metrics
                        .cost_micros
                        .map_or(String::new(), |v| v.to_string()),
                    item.metrics
                        .latency_ms
                        .map_or(String::new(), |v| v.to_string()),
                    item.metrics
                        .grader_score_micros
                        .map_or(String::new(), |v| v.to_string()),
                    item.metrics
                        .cost_per_successful_task_micros
                        .map_or(String::new(), |v| v.to_string()),
                    item.metrics
                        .retry_count
                        .map_or(String::new(), |v| v.to_string()),
                    item.metrics
                        .escalation_count
                        .map_or(String::new(), |v| v.to_string()),
                    item.metrics
                        .completion_count
                        .map_or(String::new(), |v| v.to_string()),
                );
            }
            ("text/csv", csv.into_bytes())
        }
        // Fail closed: `AnalyticsExportFormat` is `#[non_exhaustive]`, so a newer
        // client can name a format this daemon cannot serialize. Refuse rather
        // than falling back to another encoding under the requested format's name.
        AnalyticsExportFormat::Unknown | _ => return Err(AnalyticsError::UnsupportedFormat),
    };

    let artifact = artifacts
        .put_owned(
            pool,
            principal.uid(),
            media_type,
            DataClassification::Internal,
            Provenance::system("analytics.export"),
            &bytes,
        )
        .await
        .map_err(|e| AnalyticsError::InvalidData(e.to_string()))?;

    Ok(AnalyticsExportResult {
        format: request.format,
        artifact,
        row_count,
        truncated,
        generated_at: Utc::now(),
    })
}

/// Retrieve the cached export result from a previously applied command.
pub async fn export_response(
    pool: &SqlitePool,
    idempotency_key: &str,
) -> Result<AnalyticsExportResult, CodypendentError> {
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT result_json FROM commands WHERE idempotency_key = ? AND status = 'applied'",
    )
    .bind(idempotency_key)
    .fetch_optional(pool)
    .await
    .map_err(|e| CodypendentError::new("internal.command-apply-failed", e.to_string(), true))?;

    let (json,) = row.ok_or_else(|| {
        CodypendentError::new(
            "internal.command-apply-failed",
            "applied analytics export command disappeared",
            true,
        )
    })?;
    let json = json.ok_or_else(|| {
        CodypendentError::new(
            "internal.command-apply-failed",
            "applied analytics export command missing result_json",
            true,
        )
    })?;
    serde_json::from_str::<AnalyticsExportResult>(&json)
        .map_err(|e| CodypendentError::new("internal.command-apply-failed", e.to_string(), true))
}
