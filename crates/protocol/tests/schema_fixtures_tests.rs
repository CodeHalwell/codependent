//! Schema generation fixtures and stability tests (Adoption 12 A6).

#[cfg(feature = "schema-export")]
mod schema_tests {
    use codypendent_protocol::ids::{
        ApprovalId, CheckpointId, CommandId, DocumentId, ModelId, PromptId, QuestionId, RunId,
        SessionId, UserId,
    };
    use codypendent_protocol::{
        AnalyticsQuery, ArtifactRef, AutomationBindingRequest, BundleManifest, Catchup, Command,
        DataClassification, Diagnostic, DiagnosticSeverity, DiffRequest, DirtyBufferDigest,
        EditorSelection, Envelope, IdeContextUpdate, IdeRequest, InboxListQuery, Location, Payload,
        Position, Range, SessionEvent, SessionSearchQuery, SourceProvenance, TextEdit,
        WorkspaceEdit,
    };
    use schemars::schema_for;

    fn assert_schema_root<T: schemars::JsonSchema>(expected_title: &str) {
        let schema = schema_for!(T);
        assert_eq!(
            schema
                .schema
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.title.as_deref()),
            Some(expected_title),
        );
        assert!(serde_json::to_value(schema)
            .expect("schema must serialize")
            .is_object());
    }

    #[test]
    fn schema_generates_valid_json_for_ids() {
        let model_id_schema = schema_for!(ModelId);
        let json = serde_json::to_string_pretty(&model_id_schema).expect("serialize schema");
        assert!(json.contains("string") || json.contains("type"));

        let session_id_schema = schema_for!(SessionId);
        let session_json =
            serde_json::to_string_pretty(&session_id_schema).expect("serialize schema");
        assert!(session_json.contains("string") || session_json.contains("type"));

        let run_id_schema = schema_for!(RunId);
        assert!(serde_json::to_string(&run_id_schema).is_ok());

        let cmd_id_schema = schema_for!(CommandId);
        assert!(serde_json::to_string(&cmd_id_schema).is_ok());

        let user_id_schema = schema_for!(UserId);
        assert!(serde_json::to_string(&user_id_schema).is_ok());

        let approval_id_schema = schema_for!(ApprovalId);
        assert!(serde_json::to_string(&approval_id_schema).is_ok());

        let question_id_schema = schema_for!(QuestionId);
        assert!(serde_json::to_string(&question_id_schema).is_ok());

        let checkpoint_id_schema = schema_for!(CheckpointId);
        assert!(serde_json::to_string(&checkpoint_id_schema).is_ok());

        let prompt_id_schema = schema_for!(PromptId);
        assert!(serde_json::to_string(&prompt_id_schema).is_ok());

        let doc_id_schema = schema_for!(DocumentId);
        assert!(serde_json::to_string(&doc_id_schema).is_ok());
    }

    #[test]
    fn authoritative_protocol_schema_roots_are_available() {
        assert_schema_root::<Command>("Command");
        assert_schema_root::<Envelope>("Envelope");
        assert_schema_root::<Payload>("Payload");
        assert_schema_root::<SessionEvent>("SessionEvent");
        assert_schema_root::<Catchup>("Catchup");
        assert_schema_root::<ArtifactRef>("ArtifactRef");
        assert_schema_root::<DataClassification>("DataClassification");
        assert_schema_root::<Position>("Position");
        assert_schema_root::<Range>("Range");
        assert_schema_root::<EditorSelection>("EditorSelection");
        assert_schema_root::<DirtyBufferDigest>("DirtyBufferDigest");
        assert_schema_root::<IdeContextUpdate>("IdeContextUpdate");
        assert_schema_root::<Location>("Location");
        assert_schema_root::<TextEdit>("TextEdit");
        assert_schema_root::<WorkspaceEdit>("WorkspaceEdit");
        assert_schema_root::<DiffRequest>("DiffRequest");
        assert_schema_root::<IdeRequest>("IdeRequest");
        assert_schema_root::<DiagnosticSeverity>("DiagnosticSeverity");
        assert_schema_root::<Diagnostic>("Diagnostic");
        assert_schema_root::<SourceProvenance>("SourceProvenance");
        assert_schema_root::<SessionSearchQuery>("SessionSearchQuery");
        assert_schema_root::<InboxListQuery>("InboxListQuery");
        assert_schema_root::<AnalyticsQuery>("AnalyticsQuery");
        assert_schema_root::<AutomationBindingRequest>("AutomationBindingRequest");
        assert_schema_root::<BundleManifest>("BundleManifest");
    }
}
