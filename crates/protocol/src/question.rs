//! Question-domain wire types (adoption 03 — the `user.ask` tool).

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

/// One selectable choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    /// Display text (1–5 words, concise).
    pub label: String,
    /// Explanation of the choice (may be empty).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// One question as asked. `custom` is carried on the wire but deliberately NOT
/// advertised in the tool schema — the model can never disable free-text
/// answers (opencode's Prompt/Info split).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionPrompt {
    /// The complete question.
    pub question: String,
    /// Very short label (≤ 30 chars) shown as the card/tab title.
    pub header: String,
    /// Available choices (may be empty only when `custom` is true).
    pub options: Vec<QuestionOption>,
    /// Allow selecting more than one option.
    #[serde(default)]
    pub multiple: bool,
    /// Allow typing a custom answer (default true).
    #[serde(default = "default_true")]
    pub custom: bool,
}

/// How a question was resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum QuestionOutcome {
    /// One answer array per question, in question order; each answer is the
    /// selected labels (custom text is carried verbatim as a label).
    Answered { answers: Vec<Vec<String>> },
    /// The user dismissed the question; `feedback` is the optional typed
    /// correction fed back to the model (the CorrectedError port).
    Rejected {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_types_round_trip() {
        let prompt = QuestionPrompt {
            question: "Which database should we use?".to_string(),
            header: "Database".to_string(),
            options: vec![
                QuestionOption {
                    label: "SQLite (Recommended)".to_string(),
                    description: "Embedded file-based storage".to_string(),
                },
                QuestionOption {
                    label: "PostgreSQL".to_string(),
                    description: String::new(),
                },
            ],
            multiple: false,
            custom: true,
        };
        let json = serde_json::to_string(&prompt).unwrap();
        let de: QuestionPrompt = serde_json::from_str(&json).unwrap();
        assert_eq!(de, prompt);

        let outcome = QuestionOutcome::Answered {
            answers: vec![vec!["SQLite (Recommended)".to_string()]],
        };
        let json_outcome = serde_json::to_string(&outcome).unwrap();
        let de_outcome: QuestionOutcome = serde_json::from_str(&json_outcome).unwrap();
        assert_eq!(de_outcome, outcome);

        let rejected = QuestionOutcome::Rejected {
            feedback: Some("Use SQLite instead".to_string()),
        };
        let json_rej = serde_json::to_string(&rejected).unwrap();
        let de_rej: QuestionOutcome = serde_json::from_str(&json_rej).unwrap();
        assert_eq!(de_rej, rejected);
    }

    #[test]
    fn unknown_outcome_tag_deserializes_to_unknown() {
        let json = r#"{"type":"FutureOutcomeType","extra":"field"}"#;
        let outcome: QuestionOutcome = serde_json::from_str(json).unwrap();
        assert_eq!(outcome, QuestionOutcome::Unknown);
    }

    #[test]
    fn custom_defaults_true() {
        let json = r#"{"question":"Q?","header":"H","options":[]}"#;
        let prompt: QuestionPrompt = serde_json::from_str(json).unwrap();
        assert!(prompt.custom);
        assert!(!prompt.multiple);
    }
}
