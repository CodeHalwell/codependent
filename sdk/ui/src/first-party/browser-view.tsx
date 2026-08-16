/** @jsxImportSource react */
import type { ReactNode } from "react";
import {
  Badge,
  Box,
  Code,
  Image,
  LogViewer,
  Row,
  Stack,
  Text,
  TextInput,
} from "../react/primitives.js";
import { StatusBadge, SurfaceFrame, textFallback } from "./foundation.js";
import type { SemanticIntent, SurfaceOptions } from "./types.js";

export interface ConsoleLogEntry {
  id: string;
  level: "log" | "info" | "warn" | "error";
  message: string;
  timestamp: string;
}

export interface BrowserViewProps extends SurfaceOptions {
  url: string;
  status: "idle" | "loading" | "connected" | "error";
  screenshotBase64?: string;
  viewportWidth?: number;
  viewportHeight?: number;
  logs?: readonly ConsoleLogEntry[];
  domTreeSummary?: string;
  onNavigate?: string;
  onRefresh?: string;
  onCaptureScreenshot?: string;
}

export function BrowserView({
  url,
  status,
  screenshotBase64,
  viewportWidth = 1280,
  viewportHeight = 720,
  logs = [],
  domTreeSummary,
  onNavigate = "browser-navigate",
  onRefresh = "browser-refresh",
  onCaptureScreenshot = "browser-screenshot",
  ...surface
}: BrowserViewProps): ReactNode {
  const headerActions: readonly SemanticIntent[] = [
    {
      action: onRefresh,
      label: "Refresh",
      payload: { url },
    },
    {
      action: onCaptureScreenshot,
      label: "Screenshot",
      payload: { url },
    },
    ...(surface.actions ?? []),
  ];

  return (
    <SurfaceFrame
      {...surface}
      title={surface.title ?? "Browser Preview & CDP"}
      description={surface.description ?? "Headless browser session, visual validation, and DevTools console stream"}
      actions={headerActions}
    >
      <Stack gap="sm">
        {/* Navigation Bar */}
        <Row align="spaceBetween">
          <Row gap="xs">
            <StatusBadge status={status} />
            <Badge tone="muted" message={`${viewportWidth}x${viewportHeight}`} />
            <TextInput
              id={`${surface.id}-url-input`}
              name="browser-url"
              value={url}
              changeAction={onNavigate}
              accessibleLabel="Browser navigation URL"
            />
          </Row>
        </Row>

        {/* Viewport / Screenshot Surface */}
        {screenshotBase64 ? (
          <Box border="single" padding="xs">
            <Image
              id={`${surface.id}-viewport-image`}
              src={`data:image/png;base64,${screenshotBase64.slice(0, 500_000)}`}
              alt={`Browser viewport for ${url}`}
              caption={url}
              requires={{ feature: "imageDisplay" }}
              fallback={textFallback(
                "Browser Viewport",
                `Screenshot rendered for ${url} (${viewportWidth}x${viewportHeight})`
              )}
            />
          </Box>
        ) : (
          <Box border="single" padding="md">
            <Text value="No screenshot captured yet. Press 'Screenshot' to inspect." tone="muted" />
          </Box>
        )}

        {/* DOM Summary & Console Logs */}
        <Row gap="sm">
          {domTreeSummary && (
            <Box border="rounded" title="DOM Summary" grow={1}>
              <Code
                value={domTreeSummary.slice(0, 100_000)}
                language="html"
                lineNumbers={false}
                wrap
                accessibleLabel="DOM tree summary"
              />
            </Box>
          )}

          {logs.length > 0 && (
            <Box border="rounded" title="Console Logs" grow={1}>
              <LogViewer
                id={`${surface.id}-console-logs`}
                lines={logs.slice(0, 100).map((l) => `[${l.level.toUpperCase()}] ${l.message}`)}
              />
            </Box>
          )}
        </Row>
      </Stack>
    </SurfaceFrame>
  );
}
