import type { UiCapabilities, UiDocument, UiNode, UiRequirement } from "./protocol.js";

export function supportsRequirement(capabilities: UiCapabilities, requirement: UiRequirement): boolean {
  switch (requirement.feature) {
    case "richText": return capabilities.daemon.rich_text;
    case "imageDisplay": return capabilities.daemon.image_display && capabilities.media.includes("image");
    case "audioCapture": return capabilities.daemon.audio_capture;
    case "editorMutations": return capabilities.daemon.editor_mutations;
    case "diffView": return capabilities.daemon.diff_view;
    case "mouse": return capabilities.daemon.mouse;
    case "unicode": return capabilities.daemon.unicode;
    case "trueColor": return capabilities.daemon.true_color;
    case "keyboard": return capabilities.keyboard;
    case "screenReader": return capabilities.screenReader;
    case "clipboard": return capabilities.clipboard;
    case "terminal": return capabilities.client === "terminal";
    case "web": return capabilities.client === "web" || capabilities.client === "vscode" || capabilities.client === "desktop";
  }
}

function textFallback(node: Extract<UiNode, { kind: "element" }>): UiNode {
  const alt = typeof node.props.alt === "string" ? node.props.alt : undefined;
  const label = typeof node.props.accessibleLabel === "string" ? node.props.accessibleLabel : typeof node.props.label === "string" ? node.props.label : typeof node.props.title === "string" ? node.props.title : undefined;
  const value = typeof node.props.value === "string" ? node.props.value : undefined;
  return { kind: "text", ...(node.id === undefined ? {} : { id: node.id }), text: alt ?? label ?? value ?? `[${node.type} unavailable]` };
}

function supportedPrimitive(type: string, capabilities: UiCapabilities): boolean {
  return capabilities.primitives === "*" || capabilities.primitives.includes(type);
}

/** Resolves terminal/web and feature fallbacks before a client renderer sees the tree. */
export function resolveCapabilityFallbacks(node: UiNode, capabilities: UiCapabilities): UiNode {
  if (node.kind === "text" || node.kind !== "element") return node;
  const required = node.requires?.filter((requirement) => !requirement.optional) ?? [];
  const requirementsMet = required.every((requirement) => supportsRequirement(capabilities, requirement));
  const targetMet = node.type === "TerminalOnly"
    ? capabilities.client === "terminal"
    : node.type === "WebOnly"
      ? capabilities.client !== "terminal"
      : true;
  const primitiveMet = node.type === "TerminalOnly" || node.type === "WebOnly" || supportedPrimitive(node.type, capabilities);
  if (!requirementsMet || !targetMet || !primitiveMet) {
    return node.fallback === undefined
      ? textFallback(node)
      : resolveCapabilityFallbacks(node.fallback, capabilities);
  }
  if (node.type === "Image" && !supportsRequirement(capabilities, { feature: "imageDisplay" })) return node.fallback ?? textFallback(node);
  if (node.type === "Diff" && !capabilities.daemon.diff_view) return node.fallback ?? textFallback(node);
  if (node.type === "TerminalOnly" || node.type === "WebOnly") {
    return {
      kind: "element",
      ...(node.id === undefined ? {} : { id: node.id }),
      type: "Stack",
      props: node.props,
      children: (node.children ?? []).map((child) => resolveCapabilityFallbacks(child, capabilities)),
    };
  }
  return {
    ...node,
    children: (node.children ?? []).map((child) => resolveCapabilityFallbacks(child, capabilities)),
    ...(node.fallback === undefined ? {} : { fallback: resolveCapabilityFallbacks(node.fallback, capabilities) }),
  };
}

export function projectDocument(document: UiDocument, capabilities: UiCapabilities): UiDocument {
  return { ...document, capabilities, root: resolveCapabilityFallbacks(document.root, capabilities) };
}

export function negotiateCapabilities(local: UiCapabilities, remote: UiCapabilities): UiCapabilities {
  const localPrimitives = local.primitives === "*" ? remote.primitives : local.primitives;
  const primitives = remote.primitives === "*"
    ? localPrimitives
    : localPrimitives === "*"
      ? remote.primitives
      : localPrimitives.filter((primitive) => remote.primitives.includes(primitive));
  const protocolVersions = local.protocolVersions
    .flatMap((candidate) => remote.protocolVersions
      .filter((version) => version.major === candidate.major)
      .map((version) => ({ major: candidate.major, minor: Math.min(candidate.minor, version.minor) })))
    .sort((left, right) => right.major - left.major || right.minor - left.minor)
    .filter((version, index, versions) => index === 0 || version.major !== versions[index - 1]?.major || version.minor !== versions[index - 1]?.minor);
  if (protocolVersions.length === 0) throw new Error("No mutually supported remote UI protocol version");
  const colorDepths = ["monochrome", "ansi16", "ansi256", "trueColor"] as const;
  const colorDepth = colorDepths[Math.min(colorDepths.indexOf(local.colorDepth), colorDepths.indexOf(remote.colorDepth))];
  if (colorDepth === undefined) throw new Error("Unsupported color depth");
  const capabilities = local.capabilities?.filter((capability) => remote.capabilities?.includes(capability) ?? false);
  const contributionPoints = local.contributionPoints?.filter((point) => remote.contributionPoints?.includes(point) ?? false);
  const limitFields = local.limits === undefined || remote.limits === undefined ? undefined : {
    maxTreeDepth: Math.min(local.limits.maxTreeDepth, remote.limits.maxTreeDepth),
    maxNodes: Math.min(local.limits.maxNodes, remote.limits.maxNodes),
    maxTextBytes: Math.min(local.limits.maxTextBytes, remote.limits.maxTextBytes),
    maxPropertiesPerNode: Math.min(local.limits.maxPropertiesPerNode, remote.limits.maxPropertiesPerNode),
    maxActionsPerNode: Math.min(local.limits.maxActionsPerNode, remote.limits.maxActionsPerNode),
    maxJsonDepth: Math.min(local.limits.maxJsonDepth, remote.limits.maxJsonDepth),
    maxJsonValues: Math.min(local.limits.maxJsonValues, remote.limits.maxJsonValues),
    maxPatchesPerBatch: Math.min(local.limits.maxPatchesPerBatch, remote.limits.maxPatchesPerBatch),
    maxPatchBytes: Math.min(local.limits.maxPatchBytes, remote.limits.maxPatchBytes),
    maxContributions: Math.min(local.limits.maxContributions, remote.limits.maxContributions),
  };
  return {
    client: local.client,
    protocolVersions,
    daemon: {
      rich_text: local.daemon.rich_text && remote.daemon.rich_text,
      image_display: local.daemon.image_display && remote.daemon.image_display,
      audio_capture: local.daemon.audio_capture && remote.daemon.audio_capture,
      editor_mutations: local.daemon.editor_mutations && remote.daemon.editor_mutations,
      diff_view: local.daemon.diff_view && remote.daemon.diff_view,
      mouse: local.daemon.mouse && remote.daemon.mouse,
      unicode: local.daemon.unicode && remote.daemon.unicode,
      true_color: local.daemon.true_color && remote.daemon.true_color,
    },
    primitives,
    media: local.media.filter((media) => remote.media.includes(media)),
    colorDepth,
    keyboard: local.keyboard && remote.keyboard,
    screenReader: local.screenReader && remote.screenReader,
    reducedMotion: local.reducedMotion || remote.reducedMotion,
    clipboard: local.clipboard && remote.clipboard,
    viewport: local.viewport,
    ...(local.terminalGraphics === undefined ? {} : { terminalGraphics: local.terminalGraphics.filter((protocol) => remote.terminalGraphics?.includes(protocol) ?? false) }),
    ...(capabilities === undefined ? {} : { capabilities }),
    ...(contributionPoints === undefined ? {} : { contributionPoints }),
    ...(limitFields === undefined ? {} : { limits: limitFields }),
  };
}
