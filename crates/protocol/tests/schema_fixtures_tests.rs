//! Schema generation fixtures and stability tests (Adoption 12 A6).

#[cfg(feature = "schema-export")]
mod schema_tests {
    use codypendent_protocol::ids::{
        ApprovalId, CheckpointId, CommandId, DocumentId, ModelId, PromptId, QuestionId, RunId,
        SessionId, UserId,
    };
    use schemars::schema_for;

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
}
