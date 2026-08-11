/** @jsxImportSource react */
import type { ReactNode } from "react";
import {
  ArtifactCard,
  Audio,
  Badge,
  Button,
  Code,
  Form,
  Image,
  Markdown,
  MultiSelect,
  Row,
  Select,
  Stack,
  Text,
  TextArea,
} from "../react/primitives.js";
import { IntentButton, StatusBadge, SurfaceFrame, VirtualizedCollection, textFallback } from "./foundation.js";
import { toUiJson, type SemanticIntent, type SurfaceOptions } from "./types.js";

export type ConversationRole = "user" | "assistant" | "system" | "tool";

export interface ConversationAttachment {
  id: string;
  title: string;
  mediaType: string;
  source?: string;
  alt?: string;
  transcript?: string;
}

export interface ConversationMessage {
  id: string;
  role: ConversationRole;
  author: string;
  content: string;
  format?: "markdown" | "text" | "code";
  language?: string;
  createdAt: string;
  status?: "complete" | "streaming" | "failed";
  attachments?: readonly ConversationAttachment[];
}

export interface ConversationTranscriptProps extends SurfaceOptions {
  messages: readonly ConversationMessage[];
  selectedMessageId?: string;
  selectAction: string;
  messageActions?: readonly SemanticIntent<{ messageId: string }>[];
}

function AttachmentView({ attachment }: { attachment: ConversationAttachment }): ReactNode {
  if (attachment.mediaType.startsWith("image/") && attachment.source !== undefined) {
    const alt = attachment.alt ?? attachment.title;
    return (
      <Image
        id={`attachment-${attachment.id}`}
        src={attachment.source}
        alt={alt}
        caption={attachment.title}
        accessibleLabel={alt}
        requires={{ feature: "imageDisplay" }}
        fallback={textFallback(attachment.title, `Image attachment (${attachment.mediaType})`)}
      />
    );
  }
  if (attachment.mediaType.startsWith("audio/") && attachment.source !== undefined) {
    return (
      <Audio
        id={`attachment-${attachment.id}`}
        src={attachment.source}
        alt={attachment.title}
        caption={attachment.title}
        accessibleLabel={attachment.title}
        fallback={textFallback(attachment.title, attachment.transcript ?? `Audio attachment (${attachment.mediaType})`)}
        {...(attachment.transcript === undefined ? {} : { transcript: attachment.transcript })}
      />
    );
  }
  return (
    <ArtifactCard
      id={`attachment-${attachment.id}`}
      resourceId={attachment.id}
      title={attachment.title}
      status="available"
      data={toUiJson(attachment)}
      accessibleLabel={`${attachment.title}, ${attachment.mediaType}`}
    />
  );
}

function MessageContent({ message }: { message: ConversationMessage }): ReactNode {
  if (message.format === "code") {
    return <Code value={message.content} lineNumbers wrap accessibleLabel={`${message.author} code message`} {...(message.language === undefined ? {} : { language: message.language })} />;
  }
  if (message.format === "text") return <Text value={message.content} role="text" />;
  return <Markdown source={message.content} accessibleLabel={`${message.author} message`} />;
}

export function ConversationTranscript({
  messages,
  selectedMessageId,
  selectAction,
  messageActions = [],
  ...surface
}: ConversationTranscriptProps): ReactNode {
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "wide"}>
      <VirtualizedCollection
        id={`${surface.id}-messages`}
        label={`Conversation transcript with ${messages.length} messages`}
        items={messages}
        selectedKey={selectedMessageId}
        emptyMessage="No messages yet"
        itemKey={(message) => message.id}
      >
        {messages.slice(-50).map((message) => (
          <Stack key={message.id} id={`${surface.id}-message-${message.id}`} gap="xs" accessibleLabel={`${message.author} message at ${message.createdAt}`}>
            <Row align="spaceBetween">
              <Row gap="xs">
                <Text value={message.author} role="label" weight="bold" />
                <Badge title={message.role} message={message.role} tone={message.role === "system" ? "warning" : "neutral"} />
                {message.status === undefined ? null : <StatusBadge status={message.status} />}
              </Row>
              <Button
                action={selectAction}
                label="Select"
                payload={toUiJson({ messageId: message.id })}
                accessibleLabel={`Select message from ${message.author} at ${message.createdAt}`}
              />
            </Row>
            <MessageContent message={message} />
            {message.attachments?.map((attachment) => <AttachmentView key={attachment.id} attachment={attachment} />)}
            {messageActions.length === 0 ? null : (
              <Row gap="xs" accessibleLabel={`Actions for ${message.author} message`}>
                {messageActions.map((intent) => (
                  <IntentButton
                    key={intent.action}
                    intent={{ ...intent, payload: { messageId: message.id } }}
                  />
                ))}
              </Row>
            )}
          </Stack>
        ))}
      </VirtualizedCollection>
    </SurfaceFrame>
  );
}

export interface ComposerAttachment {
  id: string;
  label: string;
  mediaType: string;
  status: "ready" | "uploading" | "failed";
}

export interface ConversationComposerProps extends SurfaceOptions {
  draft: string;
  attachments: readonly ComposerAttachment[];
  submitIntent: SemanticIntent<{ draft: string; attachmentIds: string[] }>;
  draftChangeAction: string;
  attachIntent: SemanticIntent;
  removeAttachmentAction: string;
  mode: "compose" | "editing" | "steering";
  inputCapability?: "text" | "text-and-audio";
}

export function ConversationComposer({
  draft,
  attachments,
  submitIntent,
  draftChangeAction,
  attachIntent,
  removeAttachmentAction,
  mode,
  inputCapability = "text",
  ...surface
}: ConversationComposerProps): ReactNode {
  const submit = {
    ...submitIntent,
    payload: { draft, attachmentIds: attachments.map((attachment) => attachment.id) },
  };
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "wide"}>
      <Form submitAction={submit.action} accessibleLabel={`${mode} message form`}>
        <Stack gap="sm">
          <TextArea
            id={`${surface.id}-draft`}
            name="message"
            value={draft}
            rows={mode === "steering" ? 3 : 6}
            placeholder={mode === "steering" ? "Steer the active run…" : "Message Codypendent…"}
            changeAction={draftChangeAction}
            accessibleLabel={mode === "steering" ? "Steering instruction" : "Message draft"}
            description="Submit with Ctrl+Enter. Insert a newline with Enter."
          />
          {attachments.length === 0 ? null : (
            <Stack gap="xs" accessibleLabel={`${attachments.length} composer attachments`}>
              {attachments.map((attachment) => (
                <Row key={attachment.id} align="spaceBetween">
                  <Row gap="xs">
                    <Text value={attachment.label} />
                    <StatusBadge status={attachment.status} />
                  </Row>
                  <Button
                    action={removeAttachmentAction}
                    label="Remove"
                    payload={toUiJson({ attachmentId: attachment.id })}
                    accessibleLabel={`Remove ${attachment.label}`}
                  />
                </Row>
              ))}
            </Stack>
          )}
          <Row align="spaceBetween">
            <Row gap="xs">
              <IntentButton intent={attachIntent} />
              {inputCapability === "text-and-audio" ? (
                <Button action="core.audio.capture.request" label="Record" accessibleLabel="Record an audio message" shortcut="Ctrl+Shift+R" />
              ) : null}
            </Row>
            <IntentButton intent={submit} />
          </Row>
        </Stack>
      </Form>
    </SurfaceFrame>
  );
}

export interface ModelOption { id: string; label: string; provider: string; contextWindow?: number; status?: string }
export interface AgentOption { id: string; label: string; description?: string; status?: string }

export interface ModelAgentControlsProps extends SurfaceOptions {
  models: readonly ModelOption[];
  selectedModelId: string;
  modelChangeAction: string;
  agents: readonly AgentOption[];
  selectedAgentIds: readonly string[];
  agentsChangeAction: string;
  reasoningEffort: "low" | "medium" | "high" | "xhigh";
  reasoningChangeAction: string;
}

export function ModelAgentControls({
  models,
  selectedModelId,
  modelChangeAction,
  agents,
  selectedAgentIds,
  agentsChangeAction,
  reasoningEffort,
  reasoningChangeAction,
  ...surface
}: ModelAgentControlsProps): ReactNode {
  return (
    <SurfaceFrame {...surface} density={surface.density ?? "compact"} width={surface.width ?? "wide"}>
      <Row gap="sm" wrap>
        <Select
          name="model"
          value={selectedModelId}
          options={models.map((model) => ({ value: model.id, label: `${model.label} · ${model.provider}` }))}
          changeAction={modelChangeAction}
          accessibleLabel="Model"
        />
        <MultiSelect
          name="agents"
          value={[...selectedAgentIds]}
          options={agents.map((agent) => ({ value: agent.id, label: agent.label }))}
          changeAction={agentsChangeAction}
          accessibleLabel="Participating agents"
        />
        <Select
          name="reasoningEffort"
          value={reasoningEffort}
          options={["low", "medium", "high", "xhigh"].map((value) => ({ value, label: value }))}
          changeAction={reasoningChangeAction}
          accessibleLabel="Reasoning effort"
        />
      </Row>
    </SurfaceFrame>
  );
}
