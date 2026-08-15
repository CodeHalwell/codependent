#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProcessState {
    pub has_exited: bool,
    pub exit_code: Option<i32>,
    pub failure_message: Option<String>,
}

impl ProcessState {
    pub fn exited(&self, exit_code: Option<i32>) -> Self {
        Self {
            has_exited: true,
            exit_code,
            failure_message: self.failure_message.clone(),
        }
    }

    pub fn failed(&self, message: String) -> Self {
        Self {
            has_exited: true,
            exit_code: self.exit_code,
            failure_message: Some(message),
        }
    }
}
