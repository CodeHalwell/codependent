/** @jsxImportSource react */
import type { ReactNode } from "react";
import {
  Badge,
  Box,
  Button,
  Diff,
  Row,
  Select,
  Stack,
  Text,
} from "../react/primitives.js";
import { StatusBadge, SurfaceFrame, textFallback } from "./foundation.js";
import type { SemanticIntent, SurfaceOptions } from "./types.js";

export interface FileDiffSummary {
  path: string;
  additions: number;
  deletions: number;
  status: "modified" | "added" | "deleted" | "renamed";
  oldPath?: string;
  patch?: string;
}

export interface DiffInspectorProps extends SurfaceOptions {
  files: readonly FileDiffSummary[];
  selectedFile?: string;
  mode?: "unified" | "sideBySide";
  onSelectFile?: string;
  onToggleMode?: string;
  onRevertFile?: string;
}

export function DiffInspector({
  files,
  selectedFile,
  mode = "unified",
  onSelectFile = "select-diff-file",
  onToggleMode = "toggle-diff-mode",
  onRevertFile = "revert-diff-file",
  ...surface
}: DiffInspectorProps): ReactNode {
  const activeFile = files.find((f) => f.path === selectedFile) ?? files[0];
  const totalAdditions = files.reduce((acc, f) => acc + f.additions, 0);
  const totalDeletions = files.reduce((acc, f) => acc + f.deletions, 0);

  const headerActions: readonly SemanticIntent[] = [
    {
      action: onToggleMode,
      label: mode === "unified" ? "Side-by-side" : "Unified",
      payload: { currentMode: mode },
    },
    ...(surface.actions ?? []),
  ];

  return (
    <SurfaceFrame
      {...surface}
      title={surface.title ?? "Diff Inspector"}
      description={surface.description ?? "Review, inspect hunks, and manage workspace file mutations"}
      actions={headerActions}
    >
      <Stack gap="sm">
        {/* File Navigator Bar */}
        <Row align="spaceBetween">
          <Row gap="xs">
            <Badge tone="positive" message={`+${totalAdditions}`} />
            <Badge tone="critical" message={`-${totalDeletions}`} />
            <Select
              id={`${surface.id}-file-selector`}
              name="diff-file"
              accessibleLabel="Select file to inspect diff"
              options={files.slice(0, 100).map((f) => ({
                label: `${f.path} (+${f.additions}/-${f.deletions})`,
                value: f.path,
              }))}
              value={activeFile?.path}
              changeAction={onSelectFile}
            />
          </Row>
          {activeFile && (
            <Row gap="xs">
              <StatusBadge status={activeFile.status} />
              <Button
                id={`${surface.id}-revert-${activeFile.path}`}
                label="Revert File"
                action={onRevertFile}
                payload={{ path: activeFile.path }}
              />
            </Row>
          )}
        </Row>

        {/* Diff View Area */}
        {activeFile?.patch ? (
          <Diff
            id={`${surface.id}-diff-view-${activeFile.path}`}
            patch={activeFile.patch.slice(0, 200_000)}
            path={activeFile.path}
            mode={mode}
            requires={{ feature: "diffView" }}
            fallback={textFallback(activeFile.path, activeFile.patch.slice(0, 50_000))}
          />
        ) : (
          <Box border="single" padding="sm">
            <Text value="No diff available for the selected file." tone="muted" />
          </Box>
        )}
      </Stack>
    </SurfaceFrame>
  );
}
