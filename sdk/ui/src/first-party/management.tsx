/** @jsxImportSource react */
import type { ReactNode } from "react";
import {
  AgentCard,
  Badge,
  Button,
  Details,
  KeyValue,
  Row,
  Stack,
  Text,
} from "../react/primitives.js";
import { IntentButton, StatusBadge, SurfaceFrame, VirtualizedCollection } from "./foundation.js";
import { toUiJson, type SemanticIntent, type SurfaceOptions } from "./types.js";

export interface AgentDefinition {
  id: string;
  name: string;
  description: string;
  model?: string;
  capabilities: readonly string[];
  status: "available" | "busy" | "offline" | "error";
  source: "built-in" | "workspace" | "plugin";
}

export interface AgentManagementProps extends SurfaceOptions {
  agents: readonly AgentDefinition[];
  selectedAgentId?: string;
  selectAction: string;
  createIntent?: SemanticIntent;
  configureIntent: SemanticIntent<{ agentId: string }>;
  invokeIntent: SemanticIntent<{ agentId: string }>;
}

export function AgentManagement({ agents, selectedAgentId, selectAction, createIntent, configureIntent, invokeIntent, ...surface }: AgentManagementProps): ReactNode {
  return (
    <SurfaceFrame {...surface} actions={createIntent === undefined ? [] : [createIntent]}>
      <VirtualizedCollection id={`${surface.id}-agents`} label={`${agents.length} agents`} items={agents} selectedKey={selectedAgentId} emptyMessage="No agents are available" itemKey={(agent) => agent.id}>
        {agents.slice(0, 40).map((agent) => (
          <AgentCard key={agent.id} resourceId={agent.id} title={agent.name} status={agent.status} actions={[configureIntent.action, invokeIntent.action]} data={toUiJson(agent)} accessibleLabel={`${agent.name}, ${agent.status}, from ${agent.source}`}>
            <Stack gap="xs">
              <Row align="spaceBetween">
                <Text value={agent.description} />
                <StatusBadge status={agent.status} />
              </Row>
              <Row gap="xs" wrap>{agent.capabilities.map((capability) => <Badge key={capability} title={capability} message={capability} />)}</Row>
              <Row align="end" gap="xs">
                <Button action={selectAction} label="Inspect" payload={toUiJson({ agentId: agent.id })} accessibleLabel={`Inspect ${agent.name}`} />
                <IntentButton intent={{ ...configureIntent, payload: { agentId: agent.id } }} />
                <IntentButton intent={{ ...invokeIntent, payload: { agentId: agent.id } }} />
              </Row>
            </Stack>
          </AgentCard>
        ))}
      </VirtualizedCollection>
    </SurfaceFrame>
  );
}

export interface ExtensionDefinition {
  id: string;
  name: string;
  description: string;
  version: string;
  publisher: string;
  status: "enabled" | "disabled" | "update-available" | "error";
  trust: "built-in" | "verified" | "unverified";
  capabilities: readonly string[];
}

interface ExtensionManagementProps extends SurfaceOptions {
  kind: "skill" | "plugin";
  extensions: readonly ExtensionDefinition[];
  selectedId?: string;
  selectAction: string;
  installIntent?: SemanticIntent;
  enableIntent: SemanticIntent<{ id: string }>;
  disableIntent: SemanticIntent<{ id: string }>;
  updateIntent: SemanticIntent<{ id: string }>;
  removeIntent: SemanticIntent<{ id: string }>;
}

function ExtensionManagement({ kind, extensions, selectedId, selectAction, installIntent, enableIntent, disableIntent, updateIntent, removeIntent, ...surface }: ExtensionManagementProps): ReactNode {
  return (
    <SurfaceFrame {...surface} actions={installIntent === undefined ? [] : [installIntent]}>
      <VirtualizedCollection id={`${surface.id}-${kind}s`} label={`${extensions.length} ${kind}s`} items={extensions} selectedKey={selectedId} emptyMessage={`No ${kind}s installed`} itemKey={(extension) => extension.id}>
        {extensions.slice(0, 50).map((extension) => {
          const toggle = extension.status === "disabled" ? enableIntent : disableIntent;
          return (
            <Details key={extension.id} title={`${extension.name} ${extension.version}`} accessibleLabel={`${extension.name} ${kind}, ${extension.status}, ${extension.trust}`}>
              <Stack gap="xs">
                <Row align="spaceBetween">
                  <Text value={extension.description} />
                  <Row gap="xs"><StatusBadge status={extension.status} /><Badge title={extension.trust} message={extension.trust} tone={extension.trust === "unverified" ? "warning" : "neutral"} /></Row>
                </Row>
                <KeyValue entries={{ publisher: extension.publisher, capabilities: toUiJson(extension.capabilities) }} />
                <Row align="end" gap="xs">
                  <Button action={selectAction} label="Inspect" payload={toUiJson({ id: extension.id, kind })} accessibleLabel={`Inspect ${extension.name}`} />
                  <IntentButton intent={{ ...toggle, payload: { id: extension.id } }} />
                  {extension.status === "update-available" ? <IntentButton intent={{ ...updateIntent, payload: { id: extension.id } }} /> : null}
                  <IntentButton intent={{ ...removeIntent, payload: { id: extension.id } }} />
                </Row>
              </Stack>
            </Details>
          );
        })}
      </VirtualizedCollection>
    </SurfaceFrame>
  );
}

export type SkillManagementProps = Omit<ExtensionManagementProps, "kind">;
export function SkillManagement(props: SkillManagementProps): ReactNode { return <ExtensionManagement {...props} kind="skill" />; }

export type PluginManagementProps = Omit<ExtensionManagementProps, "kind">;
export function PluginManagement(props: PluginManagementProps): ReactNode { return <ExtensionManagement {...props} kind="plugin" />; }

export interface IntegrationDefinition {
  id: string;
  name: string;
  description: string;
  status: "connected" | "disconnected" | "degraded" | "error";
  account?: string;
  scopes: readonly string[];
  lastSyncAt?: string;
}

export interface IntegrationManagementProps extends SurfaceOptions {
  integrations: readonly IntegrationDefinition[];
  connectIntent: SemanticIntent<{ integrationId: string }>;
  disconnectIntent: SemanticIntent<{ integrationId: string }>;
  configureIntent: SemanticIntent<{ integrationId: string }>;
  syncIntent: SemanticIntent<{ integrationId: string }>;
}

export function IntegrationManagement({ integrations, connectIntent, disconnectIntent, configureIntent, syncIntent, ...surface }: IntegrationManagementProps): ReactNode {
  return (
    <SurfaceFrame {...surface}>
      <VirtualizedCollection id={`${surface.id}-integrations`} label={`${integrations.length} integrations`} items={integrations} emptyMessage="No integrations configured" itemKey={(integration) => integration.id}>
        {integrations.slice(0, 50).map((integration) => (
          <Details key={integration.id} title={integration.name} accessibleLabel={`${integration.name}, ${integration.status}`}>
            <Stack gap="xs">
              <Text value={integration.description} />
              <Row gap="xs"><StatusBadge status={integration.status} />{integration.account === undefined ? null : <Text value={integration.account} role="caption" />}</Row>
              <KeyValue entries={{ scopes: toUiJson(integration.scopes), ...(integration.lastSyncAt === undefined ? {} : { lastSyncAt: integration.lastSyncAt }) }} />
              <Row align="end" gap="xs">
                {integration.status === "disconnected"
                  ? <IntentButton intent={{ ...connectIntent, payload: { integrationId: integration.id } }} />
                  : <IntentButton intent={{ ...disconnectIntent, payload: { integrationId: integration.id } }} />}
                <IntentButton intent={{ ...configureIntent, payload: { integrationId: integration.id } }} />
                <IntentButton intent={{ ...syncIntent, payload: { integrationId: integration.id } }} />
              </Row>
            </Stack>
          </Details>
        ))}
      </VirtualizedCollection>
    </SurfaceFrame>
  );
}
