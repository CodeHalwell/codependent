/** @jsxImportSource react */
import type { ReactNode } from "react";
import {
  Badge,
  Button,
  Checkbox,
  Diff,
  Form,
  KeyValue,
  PatchCard,
  Row,
  Stack,
  Text,
  TextArea,
  Tree,
} from "../react/primitives.js";
import { IntentButton, StatusBadge, SurfaceFrame, VirtualizedCollection, textFallback } from "./foundation.js";
import { toUiJson, type SemanticIntent, type SurfaceOptions } from "./types.js";

export interface WorktreeSummary {
  id: string;
  path: string;
  branch: string;
  head: string;
  status: "clean" | "modified" | "conflicted" | "detached";
  ahead: number;
  behind: number;
  changeCount: number;
}

export interface WorktreeDashboardProps extends SurfaceOptions {
  worktrees: readonly WorktreeSummary[];
  selectedWorktreeId?: string;
  selectAction: string;
  createIntent?: SemanticIntent;
  refreshIntent?: SemanticIntent;
}

export function WorktreeDashboard({ worktrees, selectedWorktreeId, selectAction, createIntent, refreshIntent, ...surface }: WorktreeDashboardProps): ReactNode {
  const actions = [createIntent, refreshIntent].filter((intent): intent is SemanticIntent => intent !== undefined);
  return (
    <SurfaceFrame {...surface} actions={actions}>
      <VirtualizedCollection id={`${surface.id}-worktrees`} label={`${worktrees.length} Git worktrees`} items={worktrees} selectedKey={selectedWorktreeId} emptyMessage="No worktrees found" itemKey={(worktree) => worktree.id}>
        {worktrees.slice(0, 40).map((worktree) => (
          <Stack key={worktree.id} gap="xs" accessibleLabel={`${worktree.branch} worktree, ${worktree.status}`}>
            <Row align="spaceBetween">
              <Stack gap="xs">
                <Text value={worktree.branch} weight="bold" />
                <Text value={worktree.path} role="caption" tone="muted" />
              </Stack>
              <StatusBadge status={worktree.status} />
            </Row>
            <KeyValue entries={{ head: worktree.head, ahead: worktree.ahead, behind: worktree.behind, changes: worktree.changeCount }} />
            <Button action={selectAction} label="Open worktree" payload={toUiJson({ worktreeId: worktree.id })} accessibleLabel={`Open ${worktree.branch} worktree`} />
          </Stack>
        ))}
      </VirtualizedCollection>
    </SurfaceFrame>
  );
}

export interface GitChange {
  id: string;
  path: string;
  status: "added" | "modified" | "deleted" | "renamed" | "untracked" | "conflicted";
  staged: boolean;
  insertions: number;
  deletions: number;
  patch?: string;
}

export interface GitStatusReviewProps extends SurfaceOptions {
  repository: string;
  branch: string;
  changes: readonly GitChange[];
  selectedChangeId?: string;
  selectAction: string;
  stageAction: string;
  unstageAction: string;
  discardIntent: SemanticIntent<{ changeId: string }>;
}

export function GitStatusReview({ repository, branch, changes, selectedChangeId, selectAction, stageAction, unstageAction, discardIntent, ...surface }: GitStatusReviewProps): ReactNode {
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "wide"} description={surface.description ?? `${repository} on ${branch}`}>
      <Tree
        items={changes.map((change) => toUiJson(change))}
        virtualized
        emptyMessage="Working tree clean"
        accessibleLabel={`${changes.length} Git changes in ${repository}`}
        {...(selectedChangeId === undefined ? {} : { selectedKey: selectedChangeId })}
      >
        {changes.slice(0, 50).map((change) => (
          <PatchCard key={change.id} resourceId={change.id} title={change.path} status={change.status} data={toUiJson(change)} accessibleLabel={`${change.path}, ${change.status}, ${change.staged ? "staged" : "unstaged"}`}>
            <Stack gap="xs">
              <Row align="spaceBetween">
                <Row gap="xs"><Badge title={change.status} message={change.status} /><Text value={`+${change.insertions} −${change.deletions}`} /></Row>
                <Row gap="xs">
                  <Button action={selectAction} label="View" payload={toUiJson({ changeId: change.id })} accessibleLabel={`View ${change.path}`} />
                  <Button action={change.staged ? unstageAction : stageAction} label={change.staged ? "Unstage" : "Stage"} payload={toUiJson({ changeId: change.id })} accessibleLabel={`${change.staged ? "Unstage" : "Stage"} ${change.path}`} />
                  <IntentButton intent={{ ...discardIntent, payload: { changeId: change.id } }} />
                </Row>
              </Row>
              {change.patch === undefined ? null : (
                <Diff patch={change.patch} path={change.path} mode="unified" accessibleLabel={`Diff for ${change.path}`} requires={{ feature: "diffView", optional: true }} fallback={textFallback(`Diff for ${change.path}`, change.patch)} />
              )}
            </Stack>
          </PatchCard>
        ))}
      </Tree>
    </SurfaceFrame>
  );
}

export interface CommitComposerProps extends SurfaceOptions {
  message: string;
  stagedChanges: readonly GitChange[];
  amend: "new-commit" | "amend-head";
  sign: "unsigned" | "signed";
  messageChangeAction: string;
  amendChangeAction: string;
  signChangeAction: string;
  commitIntent: SemanticIntent<{ message: string; amend: boolean; sign: boolean }>;
}

export function CommitComposer({ message, stagedChanges, amend, sign, messageChangeAction, amendChangeAction, signChangeAction, commitIntent, ...surface }: CommitComposerProps): ReactNode {
  const disabledReason = stagedChanges.length === 0 ? "Stage at least one change" : message.trim().length === 0 ? "Enter a commit message" : undefined;
  const intent = { ...commitIntent, ...(disabledReason === undefined ? {} : { disabledReason }), payload: { message, amend: amend === "amend-head", sign: sign === "signed" } };
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "narrow"}>
      <Form submitAction={commitIntent.action} accessibleLabel="Commit changes form">
        <Stack gap="sm">
          <TextArea name="commitMessage" value={message} rows={5} changeAction={messageChangeAction} accessibleLabel="Commit message" description="Use a concise imperative subject followed by optional context." />
          <KeyValue entries={{ stagedFiles: stagedChanges.length, insertions: stagedChanges.reduce((sum, change) => sum + change.insertions, 0), deletions: stagedChanges.reduce((sum, change) => sum + change.deletions, 0) }} />
          <Checkbox name="amend" checked={amend === "amend-head"} changeAction={amendChangeAction} accessibleLabel="Amend the current HEAD commit" />
          <Checkbox name="sign" checked={sign === "signed"} changeAction={signChangeAction} accessibleLabel="Cryptographically sign this commit" />
          <Row align="end"><IntentButton intent={intent} /></Row>
        </Stack>
      </Form>
    </SurfaceFrame>
  );
}

export interface ReviewComment {
  id: string;
  path: string;
  line?: number;
  author: string;
  body: string;
  status: "open" | "resolved" | "outdated";
}

export interface CodeReviewPanelProps extends SurfaceOptions {
  reviewId: string;
  title: string;
  summary: string;
  comments: readonly ReviewComment[];
  selectedCommentId?: string;
  resolveIntent: SemanticIntent<{ commentId: string }>;
  replyAction: string;
  submitReviewIntent: SemanticIntent<{ reviewId: string }>;
}

export function CodeReviewPanel({ reviewId, title, summary, comments, selectedCommentId, resolveIntent, replyAction, submitReviewIntent, ...surface }: CodeReviewPanelProps): ReactNode {
  return (
    <SurfaceFrame {...surface} title={title} actions={[{ ...submitReviewIntent, payload: { reviewId } }]}>
      <Stack gap="sm">
        <Text value={title} role="heading" weight="bold" />
        <Text value={summary} />
        <VirtualizedCollection id={`${surface.id}-comments`} label={`${comments.length} review comments`} items={comments} selectedKey={selectedCommentId} emptyMessage="No review comments" itemKey={(comment) => comment.id}>
          {comments.slice(0, 50).map((comment) => (
            <Stack key={comment.id} gap="xs">
              <Row align="spaceBetween"><Text value={`${comment.path}${comment.line === undefined ? "" : `:${comment.line}`} · ${comment.author}`} weight="medium" /><StatusBadge status={comment.status} /></Row>
              <Text value={comment.body} />
              <Row align="end" gap="xs">
                <Button action={replyAction} label="Reply" payload={toUiJson({ commentId: comment.id })} accessibleLabel={`Reply to ${comment.author} on ${comment.path}`} />
                {comment.status === "open" ? <IntentButton intent={{ ...resolveIntent, payload: { commentId: comment.id } }} /> : null}
              </Row>
            </Stack>
          ))}
        </VirtualizedCollection>
      </Stack>
    </SurfaceFrame>
  );
}
