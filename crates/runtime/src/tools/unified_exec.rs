use codypendent_daemon::policy::{CommandScope, PathScope, ScopeVerdict};
use codypendent_daemon::unified_exec::{
    ExecOutput, OpenProcessSpec, ReadBudget, UnifiedExecManager,
};
use codypendent_protocol::{ProposedAction, RunId, SessionId};

use super::shell::{is_denied_env, resolve_program, CommandRequest, Shell};
use super::ToolError;

pub struct ShellExec;

impl ShellExec {
    pub const NAME: &'static str = "shell.exec";

    pub fn proposed_action(request: &CommandRequest) -> ProposedAction {
        Shell::proposed_action(request)
    }

    pub async fn execute(
        request: &CommandRequest,
        read: ReadBudget,
        path_scope: &PathScope,
        command_scope: &CommandScope,
        manager: &UnifiedExecManager,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<ExecOutput, ToolError> {
        let program_str = request.program.to_string_lossy().into_owned();
        if !command_scope.allows_program(&program_str) {
            return Err(ToolError::ProgramNotAllowed(program_str));
        }

        // Resolve once and hand the daemon exactly the path that was checked
        // (the no-TOCTOU seam): the spec's cwd is canonicalized, so a symlinked
        // or `..`-laden request cannot name one directory for the check and
        // another for the open.
        let (cwd, verdict) = path_scope.resolve(&request.cwd);
        match verdict {
            ScopeVerdict::Allowed => {}
            ScopeVerdict::Denied => return Err(ToolError::PathDenied(request.cwd.clone())),
            ScopeVerdict::OutsideRoots => {
                return Err(ToolError::CwdOutOfScope(request.cwd.clone()))
            }
        }

        if let Some(binding) = request.environment.iter().find(|b| is_denied_env(&b.name)) {
            return Err(ToolError::EnvironmentNotAllowed(binding.name.clone()));
        }

        let resolved = resolve_program(&request.program, &cwd)
            .await
            .ok_or_else(|| ToolError::ProgramNotFound(program_str.clone()))?;

        let spec = OpenProcessSpec {
            session_id,
            run_id,
            program: resolved,
            args: request.args.clone(),
            cwd,
            environment: request
                .environment
                .iter()
                .map(|e| (e.name.clone(), e.value.clone()))
                .collect(),
        };

        manager
            .exec(spec, read)
            .await
            .map_err(|e| ToolError::Other(anyhow::anyhow!(e)))
    }
}

pub struct ShellWriteStdin;

impl ShellWriteStdin {
    pub const NAME: &'static str = "shell.write_stdin";

    pub async fn execute(
        process_id: i32,
        input: &str,
        read: ReadBudget,
        manager: &UnifiedExecManager,
        session_id: SessionId,
    ) -> Result<ExecOutput, ToolError> {
        manager
            .write_stdin(session_id, process_id, input, read)
            .await
            .map_err(|e| ToolError::Other(anyhow::anyhow!(e)))
    }
}
