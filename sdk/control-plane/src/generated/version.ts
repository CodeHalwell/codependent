/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

export interface VersionCatalog {
  error: ControlPlaneError;
  handshake_request: ProtocolHandshakeRequest;
  handshake_response: ProtocolHandshakeResponse;
  version: ProtocolVersion;
}
/**
 * Standard wire error response returned by all Control Plane REST APIs.
 */
export interface ControlPlaneError {
  /**
   * Optional machine-readable sub-code.
   */
  code?: string | null;
  detail?: JsonValue;
  /**
   * Human-readable error message.
   */
  message: string;
  /**
   * Resource name if applicable (e.g. "repository", "session", "organization").
   */
  resource?: string | null;
  /**
   * Categorical error type (e.g. "not_found", "unauthorized", "validation_error", "conflict").
   */
  type: string;
}
/**
 * Initial protocol handshake request from a client or daemon.
 */
export interface ProtocolHandshakeRequest {
  /**
   * Capabilities requested by the client.
   */
  capabilities?: string[];
  /**
   * Client build identifier if available.
   */
  client_build_id?: string | null;
  /**
   * Client identifier or kind (e.g. "daemon", "web-ui", "cli").
   */
  client_kind: string;
  /**
   * The client's supported protocol version.
   */
  client_version: ProtocolVersion;
}
/**
 * Control plane wire protocol version.
 */
export interface ProtocolVersion {
  major: number;
  minor: number;
}
/**
 * Server response to a protocol handshake request.
 */
export interface ProtocolHandshakeResponse {
  /**
   * The oldest supported client version.
   */
  min_compatible_version: ProtocolVersion;
  /**
   * The negotiated protocol version to use for subsequent interactions.
   */
  negotiated_version: ProtocolVersion;
  /**
   * The server's full supported protocol version.
   */
  server_version: ProtocolVersion;
  /**
   * Active capabilities negotiated for this connection.
   */
  supported_capabilities?: string[];
}

type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
