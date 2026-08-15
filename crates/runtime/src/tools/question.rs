//! The `user.ask` tool (adoption 03).
//!
//! Solicits answers or confirmation from the user through structured cards
//! with radio/checkbox choices or free-text responses.

use codypendent_protocol::{QuestionOption, QuestionPrompt};

use super::ToolError;

pub struct AskUser;

impl AskUser {
    pub const NAME: &'static str = "user.ask";

    /// The JSON schema advertised to models.
    /// Deliberately omits `custom` (defaults to true) so the model cannot
    /// disable free-text write-ins.
    pub fn definition() -> serde_json::Value {
        serde_json::json!({
            "name": Self::NAME,
            "description": "Ask the user one or more structured questions with selectable choices or free-text answers. Use this when requirements are ambiguous, to solicit design preferences, or to confirm choices before taking action.",
            "parameters": {
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "description": "The list of questions to ask.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": {
                                    "type": "string",
                                    "description": "The complete question to ask the user."
                                },
                                "header": {
                                    "type": "string",
                                    "description": "Short label (≤ 30 chars, 1-3 words) shown as the question card title or tab header."
                                },
                                "options": {
                                    "type": "array",
                                    "description": "The selectable options. Each option has a label and an optional description.",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": {
                                                "type": "string",
                                                "description": "The choice text (1-5 words)."
                                            },
                                            "description": {
                                                "type": "string",
                                                "description": "Explanation of what choosing this option means."
                                            }
                                        },
                                        "required": ["label"]
                                    }
                                },
                                "multiple": {
                                    "type": "boolean",
                                    "description": "Allow selecting multiple choices (checkboxes) instead of one (radio)."
                                }
                            },
                            "required": ["question", "header", "options"]
                        }
                    }
                },
                "required": ["questions"]
            }
        })
    }
}

/// Parse and validate tool arguments into a `Vec<QuestionPrompt>`.
pub fn parse_ask_user(args: &serde_json::Value) -> Result<Vec<QuestionPrompt>, ToolError> {
    let questions_value = args
        .get("questions")
        .ok_or_else(|| ToolError::InvalidArguments {
            tool: AskUser::NAME,
            reason: "missing required 'questions' field".to_string(),
        })?;

    let questions_array =
        questions_value
            .as_array()
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: AskUser::NAME,
                reason: "'questions' must be an array".to_string(),
            })?;

    if questions_array.is_empty() {
        return Err(ToolError::InvalidArguments {
            tool: AskUser::NAME,
            reason: "'questions' array must not be empty".to_string(),
        });
    }

    let mut prompts = Vec::with_capacity(questions_array.len());

    for (idx, item) in questions_array.iter().enumerate() {
        let q_obj = item
            .as_object()
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: AskUser::NAME,
                reason: format!("question at index {idx} must be an object"),
            })?;

        let question = q_obj
            .get("question")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: AskUser::NAME,
                reason: format!("question at index {idx} requires a non-empty 'question' string"),
            })?
            .to_string();

        let header = q_obj
            .get("header")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: AskUser::NAME,
                reason: format!("question at index {idx} requires a non-empty 'header' string"),
            })?
            .to_string();

        let options_array = q_obj
            .get("options")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: AskUser::NAME,
                reason: format!("question at index {idx} requires an 'options' array"),
            })?;

        let mut options = Vec::with_capacity(options_array.len());
        for (opt_idx, opt_val) in options_array.iter().enumerate() {
            let opt_obj = opt_val
                .as_object()
                .ok_or_else(|| ToolError::InvalidArguments {
                    tool: AskUser::NAME,
                    reason: format!("option {opt_idx} in question {idx} must be an object"),
                })?;

            let label = opt_obj
                .get("label")
                .and_then(|v| v.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| ToolError::InvalidArguments {
                    tool: AskUser::NAME,
                    reason: format!(
                        "option {opt_idx} in question {idx} requires a non-empty 'label' string"
                    ),
                })?
                .to_string();

            let description = opt_obj
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();

            options.push(QuestionOption { label, description });
        }

        let multiple = q_obj
            .get("multiple")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Model cannot disable free-text write-ins; always true.
        let custom = true;

        prompts.push(QuestionPrompt {
            question,
            header,
            options,
            multiple,
            custom,
        });
    }

    Ok(prompts)
}

/// Render the answered question(s) for model observation.
/// Empty answers render as literal `"(Unanswered)"`.
pub fn render_answers(questions: &[QuestionPrompt], answers: &[Vec<String>]) -> String {
    if questions.is_empty() {
        return "No questions asked.".to_string();
    }

    if questions.len() == 1 {
        let q = &questions[0];
        let ans = answers.first().map(|v| v.as_slice()).unwrap_or(&[]);
        let ans_str = if ans.is_empty() {
            "Unanswered"
        } else {
            &ans.join(", ")
        };
        return format!("{}: {}", q.header, ans_str);
    }

    let mut out = String::from("User answers:\n");
    for (i, q) in questions.iter().enumerate() {
        let ans = answers.get(i).map(|v| v.as_slice()).unwrap_or(&[]);
        let ans_str = if ans.is_empty() {
            "Unanswered"
        } else {
            &ans.join(", ")
        };
        out.push_str(&format!("- {}: {}\n", q.header, ans_str));
    }
    out.trim_end().to_string()
}

/// Format the model-facing refusal / rejection message when a user dismisses a question.
pub fn render_rejection(feedback: Option<&str>) -> String {
    match feedback.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(fb) => format!(
            "question rejected: the user declined to answer and said: {fb}. Treat this as a correction and continue; do not re-ask."
        ),
        None => "question rejected: the user declined to answer. Treat this as a correction and continue; do not re-ask.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_ask_user_json() {
        let json = serde_json::json!({
            "questions": [
                {
                    "question": "Which database engine?",
                    "header": "Database",
                    "options": [
                        { "label": "SQLite", "description": "Local embedded" },
                        { "label": "PostgreSQL", "description": "Networked SQL" }
                    ],
                    "multiple": false
                }
            ]
        });

        let prompts = parse_ask_user(&json).unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].question, "Which database engine?");
        assert_eq!(prompts[0].header, "Database");
        assert_eq!(prompts[0].options.len(), 2);
        assert!(!prompts[0].multiple);
        assert!(prompts[0].custom);
    }

    #[test]
    fn parse_invalid_missing_questions() {
        let json = serde_json::json!({});
        assert!(parse_ask_user(&json).is_err());
    }

    #[test]
    fn parse_invalid_empty_questions() {
        let json = serde_json::json!({ "questions": [] });
        assert!(parse_ask_user(&json).is_err());
    }

    #[test]
    fn render_answers_single_and_multi() {
        let q1 = QuestionPrompt {
            question: "DB?".to_string(),
            header: "Database".to_string(),
            options: vec![],
            multiple: false,
            custom: true,
        };
        let out = render_answers(std::slice::from_ref(&q1), &[vec!["SQLite".to_string()]]);
        assert_eq!(out, "Database: SQLite");

        let out_unanswered = render_answers(std::slice::from_ref(&q1), &[vec![]]);
        assert_eq!(out_unanswered, "Database: Unanswered");

        let q2 = QuestionPrompt {
            question: "Auth?".to_string(),
            header: "Auth".to_string(),
            options: vec![],
            multiple: true,
            custom: true,
        };
        let out_multi = render_answers(
            &[q1, q2],
            &[
                vec!["SQLite".to_string()],
                vec!["JWT".to_string(), "OAuth".to_string()],
            ],
        );
        assert_eq!(
            out_multi,
            "User answers:\n- Database: SQLite\n- Auth: JWT, OAuth"
        );
    }

    #[test]
    fn render_rejection_with_and_without_feedback() {
        let with_fb = render_rejection(Some("Use Postgres instead"));
        assert_eq!(
            with_fb,
            "question rejected: the user declined to answer and said: Use Postgres instead. Treat this as a correction and continue; do not re-ask."
        );

        let without_fb = render_rejection(None);
        assert_eq!(
            without_fb,
            "question rejected: the user declined to answer. Treat this as a correction and continue; do not re-ask."
        );
    }
}
