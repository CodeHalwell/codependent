/**
 * DaemonClient: a thin, reconnecting client for the Codypendent daemon over a
 * Unix domain socket.
 *
 * Lifecycle of one connection:
 *   connect -> send `ClientHello` -> receive `ServerHello`
 *           -> send `Command(AttachSession { requested_role: Approver })`
 *           -> receive `Catchup` and a live stream of `Event`s.
 *
 * This module imports its host-neutral protocol transport from
 * `@codypendent/protocol` and provides the default Node.js Unix socket connection
 * factory (`net.createConnection`).
 */
import * as net from "node:net";

import {
  DaemonClient as BaseDaemonClient,
  type DaemonClientOptions as BaseDaemonClientOptions,
  type SocketLike,
} from "@codypendent/protocol";

export {
  computeBackoff,
  DEFAULT_BACKOFF,
  MAX_QUEUED_COMMANDS,
  encodeEnvelope,
  FrameDecoder,
  FrameError,
  MAX_FRAME_BYTES,
  listInbox,
  listInboxCommand,
  mutateInbox,
  mutateInboxCommand,
  queryAnalytics,
  queryAnalyticsCommand,
  exportAnalytics,
  exportAnalyticsCommand,
  searchSessions,
  searchSessionsCommand,
  SessionSearchPager,
  type BackoffConfig,
  type ConnectionFactory,
  type ConnectionStatus,
  type DaemonClientEvents,
  type SocketLike,
} from "@codypendent/protocol";

export interface DaemonClientOptions extends BaseDaemonClientOptions {
  socketPath: string;
}

export class DaemonClient extends BaseDaemonClient {
  constructor(options: DaemonClientOptions) {
    super({
      clientName: "codypendent-vscode",
      ...options,
      createConnection:
        options.createConnection ??
        ((socketPath: string) => net.createConnection({ path: socketPath }) as unknown as SocketLike),
    });
  }
}
