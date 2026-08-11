/** @jsxImportSource react */
import type { ReactNode } from "react";
import {
  Box,
  Button,
  CommandList,
  Link,
  Menu,
  Row,
  Split,
  Stack,
  Text,
  TextInput,
  Toolbar,
} from "../react/primitives.js";
import { IntentButton, StatusBadge, SurfaceFrame, VirtualizedCollection } from "./foundation.js";
import { toUiJson, type SemanticIntent, type SurfaceOptions } from "./types.js";

export interface NavigationItem {
  id: string;
  label: string;
  destination: string;
  status?: string;
  shortcut?: string;
}

export interface ApplicationShellProps extends SurfaceOptions {
  productName?: string;
  workspaceLabel: string;
  navigation: readonly NavigationItem[];
  activeDestination: string;
  navigateAction: string;
  globalActions?: readonly SemanticIntent[];
  status?: readonly StatusItem[];
  children: ReactNode;
}

export interface StatusItem {
  id: string;
  label: string;
  value: string;
  status?: string;
}

export function ApplicationShell({
  productName = "Codypendent",
  workspaceLabel,
  navigation,
  activeDestination,
  navigateAction,
  globalActions = [],
  status = [],
  children,
  ...surface
}: ApplicationShellProps): ReactNode {
  return (
    <SurfaceFrame {...surface} description={surface.description ?? `${workspaceLabel} workspace`} width={surface.width ?? "full"}>
      <Stack gap={surface.density === "compact" ? "xs" : "sm"}>
        <Row align="spaceBetween">
          <Stack gap="xs">
            <Text value={productName} role="heading" weight="bold" accessibleLabel={productName} />
            <Text value={workspaceLabel} role="caption" tone="muted" accessibleLabel={`Workspace: ${workspaceLabel}`} />
          </Stack>
          <Toolbar accessibleLabel="Global application actions" items={globalActions.map((intent) => toUiJson(intent))}>
            {globalActions.map((intent) => <IntentButton key={intent.action} intent={intent} />)}
          </Toolbar>
        </Row>
        <Split direction="horizontal" ratio={0.22} gap="sm">
          <Menu
            id={`${surface.id}-navigation`}
            accessibleLabel="Primary navigation"
            items={navigation.map((item) => toUiJson(item))}
            action={navigateAction}
            current={activeDestination}
          >
            <Stack gap="xs">
              {navigation.map((item) => (
                <Link
                  key={item.id}
                  id={`${surface.id}-nav-${item.id}`}
                  label={item.label}
                  href={item.destination}
                  action={navigateAction}
                  accessibleLabel={`${item.label}${item.status === undefined ? "" : `, ${item.status}`}`}
                  {...(item.shortcut === undefined ? {} : { description: item.shortcut })}
                  {...(item.destination === activeDestination ? { current: item.destination } : {})}
                />
              ))}
            </Stack>
          </Menu>
          <Box accessibleLabel={`${surface.title} content`}>{children}</Box>
        </Split>
        <ApplicationStatusLine items={status} />
      </Stack>
    </SurfaceFrame>
  );
}

export function ApplicationStatusLine({ items }: { items: readonly StatusItem[] }): ReactNode {
  return (
    <Row gap="sm" wrap accessibleLabel="Application status">
      {items.map((item) => (
        <Row key={item.id} gap="xs" accessibleLabel={`${item.label}: ${item.value}`}>
          <Text value={`${item.label}:`} role="label" tone="muted" />
          <Text value={item.value} role="status" />
          {item.status === undefined ? null : <StatusBadge status={item.status} />}
        </Row>
      ))}
    </Row>
  );
}

export interface CommandItem {
  id: string;
  label: string;
  category: string;
  description?: string;
  shortcut?: string;
  enabled: boolean;
  disabledReason?: string;
  payload?: unknown;
}

export interface CommandPaletteProps extends SurfaceOptions {
  query: string;
  queryAction: string;
  commands: readonly CommandItem[];
  selectedCommandId?: string;
  selectAction: string;
  invokeAction: string;
  closeAction: string;
}

export function CommandPalette({
  query,
  queryAction,
  commands,
  selectedCommandId,
  selectAction,
  invokeAction,
  closeAction,
  ...surface
}: CommandPaletteProps): ReactNode {
  return (
    <SurfaceFrame
      {...surface}
      width={surface.width ?? "narrow"}
      actions={[{ action: closeAction, label: "Close command palette", shortcut: "Esc" }]}
    >
      <Stack gap="sm">
        <TextInput
          id={`${surface.id}-query`}
          name="commandQuery"
          value={query}
          placeholder="Search commands"
          changeAction={queryAction}
          accessibleLabel="Search commands"
          description="Type to filter. Use arrow keys to move and Enter to run."
        />
        <CommandList
          id={`${surface.id}-commands`}
          items={commands.map((command) => toUiJson(command))}
          virtualized
          emptyMessage="No commands match this search"
          accessibleLabel={`${commands.length} available commands`}
          {...(selectedCommandId === undefined ? {} : { selectedKey: selectedCommandId })}
        >
          {commands.slice(0, 25).map((command) => (
            <Row key={command.id} align="spaceBetween" gap="sm">
              <Button
                id={`${surface.id}-select-${command.id}`}
                action={selectAction}
                label={command.label}
                payload={toUiJson({ commandId: command.id })}
                accessibleLabel={`Select ${command.label}`}
                disabled={!command.enabled}
                {...(command.description === undefined ? {} : { description: command.description })}
              />
              <Button
                id={`${surface.id}-invoke-${command.id}`}
                action={invokeAction}
                label="Run"
                payload={toUiJson({ commandId: command.id, payload: command.payload })}
                accessibleLabel={`Run ${command.label}`}
                disabled={!command.enabled}
                {...(command.shortcut === undefined ? {} : { shortcut: command.shortcut })}
                {...(command.disabledReason === undefined ? {} : { description: command.disabledReason })}
              />
            </Row>
          ))}
        </CommandList>
      </Stack>
    </SurfaceFrame>
  );
}

export interface NavigationRailProps {
  id: string;
  label: string;
  items: readonly NavigationItem[];
  activeDestination: string;
  navigateAction: string;
}

export function NavigationRail({ id, label, items, activeDestination, navigateAction }: NavigationRailProps): ReactNode {
  return (
    <VirtualizedCollection
      id={id}
      label={label}
      items={items}
      selectedKey={activeDestination}
      emptyMessage="No navigation destinations"
      itemKey={(item) => item.id}
    >
      {items.slice(0, 20).map((item) => (
        <Link
          key={item.id}
          label={item.label}
          href={item.destination}
          action={navigateAction}
          accessibleLabel={item.label}
          {...(item.destination === activeDestination ? { current: item.destination } : {})}
          {...(item.shortcut === undefined ? {} : { description: item.shortcut })}
        />
      ))}
    </VirtualizedCollection>
  );
}
