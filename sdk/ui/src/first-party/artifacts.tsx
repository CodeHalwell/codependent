/** @jsxImportSource react */
import type { ReactNode } from "react";
import {
  ArtifactCard,
  Audio,
  Button,
  Code,
  Diff,
  Image,
  JsonTree,
  Markdown,
  PatchCard,
  Row,
  Stack,
  Tabs,
  TestReport,
  Text,
  Tree,
} from "../react/primitives.js";
import { StatusBadge, SurfaceFrame, VirtualizedCollection, textFallback } from "./foundation.js";
import { toUiJson, type SemanticIntent, type SurfaceOptions } from "./types.js";

export interface ArtifactEntry {
  id: string;
  title: string;
  mediaType: string;
  revision: number;
  size?: number;
  status: "ready" | "streaming" | "error" | "stale";
  path?: string;
}

export interface ArtifactBrowserProps extends SurfaceOptions {
  artifacts: readonly ArtifactEntry[];
  selectedArtifactId?: string;
  selectAction: string;
  refreshIntent?: SemanticIntent;
}

export function ArtifactBrowser({ artifacts, selectedArtifactId, selectAction, refreshIntent, ...surface }: ArtifactBrowserProps): ReactNode {
  return (
    <SurfaceFrame {...surface} actions={refreshIntent === undefined ? [] : [refreshIntent]}>
      <VirtualizedCollection
        id={`${surface.id}-artifacts`}
        label={`${artifacts.length} artifacts`}
        items={artifacts}
        selectedKey={selectedArtifactId}
        emptyMessage="No artifacts have been produced"
        itemKey={(artifact) => artifact.id}
      >
        {artifacts.slice(0, 40).map((artifact) => (
          <ArtifactCard
            key={artifact.id}
            resourceId={artifact.id}
            title={artifact.title}
            status={artifact.status}
            data={toUiJson(artifact)}
            accessibleLabel={`${artifact.title}, ${artifact.mediaType}, revision ${artifact.revision}, ${artifact.status}`}
          >
            <Row align="spaceBetween">
              <Row gap="xs">
                <Text value={artifact.mediaType} role="caption" tone="muted" />
                <StatusBadge status={artifact.status} />
              </Row>
              <Button
                action={selectAction}
                label="Open"
                payload={toUiJson({ artifactId: artifact.id, revision: artifact.revision })}
                accessibleLabel={`Open ${artifact.title}`}
              />
            </Row>
          </ArtifactCard>
        ))}
      </VirtualizedCollection>
    </SurfaceFrame>
  );
}

export interface DocumentViewerProps extends SurfaceOptions {
  artifact: ArtifactEntry;
  source: string;
  outline?: readonly { id: string; label: string; level: number }[];
  selectedSectionId?: string;
  navigateAction?: string;
}

export function DocumentViewer({ artifact, source, outline = [], selectedSectionId, navigateAction, ...surface }: DocumentViewerProps): ReactNode {
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "wide"}>
      <Stack gap="sm">
        <ArtifactCard resourceId={artifact.id} status={artifact.status} data={toUiJson(artifact)} accessibleLabel={`${artifact.title} document`} />
        {outline.length === 0 ? null : (
          <Tree
            items={outline.map((entry) => toUiJson(entry))}
            emptyMessage="No document outline"
            virtualized={outline.length > 100}
            accessibleLabel="Document outline"
            {...(selectedSectionId === undefined ? {} : { selectedKey: selectedSectionId })}
            {...(navigateAction === undefined ? {} : { description: `Navigation intent: ${navigateAction}` })}
          />
        )}
        <Markdown source={source} accessibleLabel={`${artifact.title} contents`} />
      </Stack>
    </SurfaceFrame>
  );
}

export interface CodeViewerProps extends SurfaceOptions {
  artifact: ArtifactEntry;
  source: string;
  language: string;
  startLine?: number;
  selection?: { startLine: number; endLine: number };
  actions?: readonly SemanticIntent<{ artifactId: string }>[];
}

export function CodeViewer({ artifact, source, language, startLine = 1, selection, actions = [], ...surface }: CodeViewerProps): ReactNode {
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "wide"} actions={actions.map((intent) => ({ ...intent, payload: { artifactId: artifact.id } }))}>
      <ArtifactCard resourceId={artifact.id} title={artifact.title} status={artifact.status} data={toUiJson({ ...artifact, selection })}>
        <Code
          value={source}
          language={language}
          startLine={startLine}
          lineNumbers
          wrap={false}
          accessibleLabel={`${artifact.title} source code, ${language}`}
        />
      </ArtifactCard>
    </SurfaceFrame>
  );
}

export interface DiffHunk { id: string; header: string; patch: string; status?: string }

export interface DiffReviewProps extends SurfaceOptions {
  artifactId: string;
  path: string;
  before?: string;
  after?: string;
  patch: string;
  mode: "unified" | "sideBySide";
  hunks: readonly DiffHunk[];
  selectedHunkId?: string;
  selectHunkAction: string;
  applyIntent?: SemanticIntent<{ artifactId: string }>;
  discardIntent?: SemanticIntent<{ artifactId: string }>;
}

export function DiffReview({ artifactId, path, before, after, patch, mode, hunks, selectedHunkId, selectHunkAction, applyIntent, discardIntent, ...surface }: DiffReviewProps): ReactNode {
  const actions: SemanticIntent[] = [];
  if (discardIntent !== undefined) actions.push({ ...discardIntent, payload: { artifactId } });
  if (applyIntent !== undefined) actions.push({ ...applyIntent, payload: { artifactId } });
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "full"} actions={actions}>
      <PatchCard resourceId={artifactId} title={path} status="review" data={toUiJson({ artifactId, path, hunks })} accessibleLabel={`Diff review for ${path}`}>
        <Stack gap="sm">
          <Diff
            patch={patch}
            path={path}
            mode={mode}
            accessibleLabel={`${mode} diff for ${path}`}
            requires={{ feature: "diffView", optional: true }}
            fallback={textFallback(`Diff for ${path}`, patch)}
            {...(before === undefined ? {} : { before })}
            {...(after === undefined ? {} : { after })}
          />
          <Tabs
            tabs={hunks.map((hunk) => ({ id: hunk.id, label: hunk.header }))}
            activeId={selectedHunkId ?? hunks[0]?.id ?? "none"}
            changeAction={selectHunkAction}
            accessibleLabel="Diff hunks"
          />
        </Stack>
      </PatchCard>
    </SurfaceFrame>
  );
}

export interface TestCaseResult { id: string; name: string; status: "passed" | "failed" | "skipped"; durationMs: number; message?: string }

export interface TestResultsViewerProps extends SurfaceOptions {
  reportId: string;
  suite: string;
  tests: readonly TestCaseResult[];
  rerunIntent?: SemanticIntent<{ reportId: string }>;
  openTestAction: string;
}

export function TestResultsViewer({ reportId, suite, tests, rerunIntent, openTestAction, ...surface }: TestResultsViewerProps): ReactNode {
  const passed = tests.filter((test) => test.status === "passed").length;
  const failed = tests.filter((test) => test.status === "failed").length;
  return (
    <SurfaceFrame {...surface} actions={rerunIntent === undefined ? [] : [{ ...rerunIntent, payload: { reportId } }]}>
      <TestReport resourceId={reportId} title={suite} status={failed === 0 ? "passed" : "failed"} data={toUiJson({ passed, failed, total: tests.length })} accessibleLabel={`${suite}: ${passed} passed, ${failed} failed`}>
        <VirtualizedCollection id={`${surface.id}-tests`} label={`${suite} test cases`} items={tests} emptyMessage="No test cases" itemKey={(test) => test.id}>
          {tests.slice(0, 50).map((test) => (
            <Row key={test.id} align="spaceBetween">
              <Stack gap="xs">
                <Text value={test.name} />
                {test.message === undefined ? null : <Text value={test.message} role="caption" tone={test.status === "failed" ? "critical" : "muted"} />}
              </Stack>
              <Row gap="xs">
                <StatusBadge status={test.status} />
                <Button action={openTestAction} label="Open" payload={toUiJson({ testId: test.id })} accessibleLabel={`Open ${test.name}`} />
              </Row>
            </Row>
          ))}
        </VirtualizedCollection>
      </TestReport>
    </SurfaceFrame>
  );
}

export interface MediaViewerProps extends SurfaceOptions {
  artifact: ArtifactEntry;
  source: string;
  kind: "image" | "audio";
  alt: string;
  transcript?: string;
  actions?: readonly SemanticIntent<{ artifactId: string }>[];
}

export function MediaViewer({ artifact, source, kind, alt, transcript, actions = [], ...surface }: MediaViewerProps): ReactNode {
  return (
    <SurfaceFrame {...surface} actions={actions.map((intent) => ({ ...intent, payload: { artifactId: artifact.id } }))}>
      {kind === "image" ? (
        <Image src={source} alt={alt} caption={artifact.title} accessibleLabel={alt} requires={{ feature: "imageDisplay" }} fallback={textFallback(artifact.title, alt)} />
      ) : (
        <Audio src={source} alt={alt} caption={artifact.title} accessibleLabel={alt} fallback={textFallback(artifact.title, transcript ?? alt)} {...(transcript === undefined ? {} : { transcript })} />
      )}
    </SurfaceFrame>
  );
}

export interface StructuredArtifactViewerProps extends SurfaceOptions {
  artifact: ArtifactEntry;
  value: unknown;
  expandedDepth?: number;
}

export function StructuredArtifactViewer({ artifact, value, expandedDepth = 3, ...surface }: StructuredArtifactViewerProps): ReactNode {
  return (
    <SurfaceFrame {...surface}>
      <ArtifactCard resourceId={artifact.id} title={artifact.title} status={artifact.status} data={toUiJson(artifact)}>
        <JsonTree value={toUiJson(value)} expandedDepth={expandedDepth} accessibleLabel={`${artifact.title} structured data`} />
      </ArtifactCard>
    </SurfaceFrame>
  );
}
