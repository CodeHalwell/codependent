/** @jsxImportSource react */
import type { ReactNode } from "react";
import {
  Badge,
  Box,
  Button,
  Row,
  Stack,
  Text,
} from "../react/primitives.js";
import { SurfaceFrame } from "./foundation.js";
import type { SurfaceOptions } from "./types.js";

export interface CheckpointItem {
  id: string;
  ordinal: number;
  kind: "tool_pre" | "tool_post" | "user_prompt" | "manual";
  commitSha: string;
  message: string;
  createdAt: string;
  filesChanged: readonly string[];
}

export interface CheckpointTimelineProps extends SurfaceOptions {
  checkpoints: readonly CheckpointItem[];
  activeCheckpointId?: string;
  onRestoreCheckpoint?: string;
  onPreviewCheckpoint?: string;
}

export function CheckpointTimeline({
  checkpoints,
  activeCheckpointId,
  onRestoreCheckpoint = "restore-checkpoint",
  onPreviewCheckpoint = "preview-checkpoint",
  ...surface
}: CheckpointTimelineProps): ReactNode {
  return (
    <SurfaceFrame
      {...surface}
      title={surface.title ?? "Time Travel & Checkpoints"}
      description={surface.description ?? "Shadow git checkpoints per tool turn for instant step rewind and safe rollbacks"}
    >
      <Stack gap="sm">
        {checkpoints.slice(0, 50).map((cp) => {
          const isActive = cp.id === activeCheckpointId;
          const shortSha = cp.commitSha.slice(0, 8);

          return (
            <Box
              key={cp.id}
              id={`${surface.id}-checkpoint-${cp.id}`}
              border={isActive ? "double" : "rounded"}
              padding="sm"
            >
              <Row align="spaceBetween">
                <Stack gap="xs">
                  <Row gap="xs">
                    <Badge
                      tone={cp.kind === "manual" ? "warning" : "muted"}
                      message={`#${cp.ordinal} ${cp.kind}`}
                    />
                    <Text value={cp.message} weight="bold" />
                    <Text value={`(${shortSha})`} tone="muted" />
                  </Row>

                  <Row gap="xs">
                    <Text
                      value={`${cp.filesChanged.length} file(s) changed: ${cp.filesChanged.slice(0, 3).join(", ")}${cp.filesChanged.length > 3 ? "..." : ""}`}
                      role="caption"
                    />
                    <Text value={cp.createdAt} role="caption" tone="muted" />
                  </Row>
                </Stack>

                <Row gap="xs">
                  <Button
                    id={`${surface.id}-preview-${cp.id}`}
                    label="Inspect Diff"
                    action={onPreviewCheckpoint}
                    payload={{ checkpointId: cp.id }}
                  />
                  <Button
                    id={`${surface.id}-restore-${cp.id}`}
                    label="Restore (Undo)"
                    tone="critical"
                    action={onRestoreCheckpoint}
                    payload={{ checkpointId: cp.id }}
                  />
                </Row>
              </Row>
            </Box>
          );
        })}
      </Stack>
    </SurfaceFrame>
  );
}
