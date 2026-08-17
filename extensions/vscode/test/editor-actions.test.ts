import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it, vi } from "vitest";
import * as vscode from "vscode";

import {
  executeExplainSelection,
  executeFixDiagnostic,
  executeFixSelection,
  executeGenerateTestsForSelection,
  executeReviewCurrentFile,
  extractIdeContext,
  findTargetDiagnostic,
  toProtocolDiagnostic,
  toProtocolDiagnosticSeverity,
} from "../src/editor-actions.js";
import { DaemonClient } from "../src/client.js";
import type {
  EditorActionContext,
  EditorNativeAction,
} from "../src/protocol/types.js";

// Mock vscode module
vi.mock("vscode", () => {
  const DiagnosticSeverity = {
    Error: 0,
    Warning: 1,
    Information: 2,
    Hint: 3,
  };

  class Position {
    constructor(public readonly line: number, public readonly character: number) {}
    isBefore(other: Position): boolean {
      return this.line < other.line || (this.line === other.line && this.character < other.character);
    }
    isBeforeOrEqual(other: Position): boolean {
      return this.line < other.line || (this.line === other.line && this.character <= other.character);
    }
    isAfter(other: Position): boolean {
      return this.line > other.line || (this.line === other.line && this.character > other.character);
    }
    isAfterOrEqual(other: Position): boolean {
      return this.line > other.line || (this.line === other.line && this.character >= other.character);
    }
    isEqual(other: Position): boolean {
      return this.line === other.line && this.character === other.character;
    }
    compareTo(other: Position): number {
      if (this.line !== other.line) return this.line - other.line;
      return this.character - other.character;
    }
    translate(_lineDelta?: number, _characterDelta?: number): Position {
      return this;
    }
    with(_change?: { line?: number; character?: number }): Position {
      return this;
    }
  }

  class Range {
    readonly start: Position;
    readonly end: Position;
    readonly isEmpty: boolean;
    readonly isSingleLine: boolean;

    constructor(start: Position, end: Position) {
      this.start = start;
      this.end = end;
      this.isEmpty = start.isEqual(end);
      this.isSingleLine = start.line === end.line;
    }

    contains(positionOrRange: Position | Range): boolean {
      if ("start" in positionOrRange) {
        return this.contains(positionOrRange.start) && this.contains(positionOrRange.end);
      }
      const position = positionOrRange;
      if (position.line < this.start.line || position.line > this.end.line) return false;
      if (position.line === this.start.line && position.character < this.start.character) return false;
      if (position.line === this.end.line && position.character > this.end.character) return false;
      return true;
    }

    isEqual(other: Range): boolean {
      return this.start.isEqual(other.start) && this.end.isEqual(other.end);
    }

    intersection(other: Range): Range | undefined {
      const start = this.start.isAfter(other.start) ? this.start : other.start;
      const end = this.end.isBefore(other.end) ? this.end : other.end;
      return start.isBeforeOrEqual(end) ? new Range(start, end) : undefined;
    }

    union(other: Range): Range {
      const start = this.start.isBefore(other.start) ? this.start : other.start;
      const end = this.end.isAfter(other.end) ? this.end : other.end;
      return new Range(start, end);
    }

    with(_change?: { start?: Position; end?: Position }): Range {
      return this;
    }
  }

  class Selection extends Range {
    readonly anchor: Position;
    readonly active: Position;
    readonly isReversed: boolean;

    constructor(anchor: Position, active: Position) {
      const [start, end] = anchor.isBefore(active) ? [anchor, active] : [active, anchor];
      super(start, end);
      this.anchor = anchor;
      this.active = active;
      this.isReversed = anchor.isAfter(active);
    }
  }

  const Uri = {
    file: (path: string) => ({
      scheme: "file",
      fsPath: path,
      path,
      toString: () => `file://${path}`,
    }),
    parse: (str: string) => ({
      scheme: "file",
      fsPath: str.replace(/^file:\/\//, ""),
      path: str.replace(/^file:\/\//, ""),
      toString: () => str,
    }),
  };

  const window = {
    activeTextEditor: undefined as unknown,
    showWarningMessage: vi.fn(),
    showInformationMessage: vi.fn(),
  };

  const workspace = {
    workspaceFolders: [
      {
        uri: Uri.file("/path/to/repo"),
        name: "repo",
        index: 0,
      },
    ],
    textDocuments: [] as unknown[],
  };

  const languages = {
    getDiagnostics: vi.fn(() => [] as vscode.Diagnostic[]),
  };

  const commands = {
    registerCommand: vi.fn((_id: string, callback: (...args: unknown[]) => unknown) => ({
      dispose: vi.fn(),
      callback,
    })),
    executeCommand: vi.fn(),
  };

  return {
    DiagnosticSeverity,
    Position,
    Range,
    Selection,
    Uri,
    window,
    workspace,
    languages,
    commands,
  };
});

describe("editor-actions context extraction", () => {
  it("converts VS Code diagnostic severities to protocol severities", () => {
    expect(toProtocolDiagnosticSeverity(vscode.DiagnosticSeverity.Error)).toEqual({ type: "Error" });
    expect(toProtocolDiagnosticSeverity(vscode.DiagnosticSeverity.Warning)).toEqual({ type: "Warning" });
    expect(toProtocolDiagnosticSeverity(vscode.DiagnosticSeverity.Information)).toEqual({ type: "Information" });
    expect(toProtocolDiagnosticSeverity(vscode.DiagnosticSeverity.Hint)).toEqual({ type: "Hint" });
  });

  it("converts VS Code Diagnostic to protocol Diagnostic with 0-based range and path", () => {
    const uri = vscode.Uri.file("/path/to/file.ts");
    const diag = {
      range: new vscode.Range(new vscode.Position(10, 2), new vscode.Position(10, 8)),
      severity: vscode.DiagnosticSeverity.Error,
      message: "Type mismatch: expected number, got string",
      source: "typescript",
    } as vscode.Diagnostic;

    const protocolDiag = toProtocolDiagnostic(uri, diag);
    expect(protocolDiag).toEqual({
      path: "/path/to/file.ts",
      range: {
        start: { line: 10, character: 2 },
        end: { line: 10, character: 8 },
      },
      severity: { type: "Error" },
      message: "Type mismatch: expected number, got string",
      source: "typescript",
    });
  });

  it("extracts IdeContextUpdate containing active file, selection, open files, and dirty buffers", () => {
    const activeDoc = {
      uri: vscode.Uri.file("/path/to/repo/src/index.ts"),
      isDirty: true,
      getText: () => "const x = 1;\n",
    } as vscode.TextDocument;

    const otherDoc = {
      uri: vscode.Uri.file("/path/to/repo/src/utils.ts"),
      isDirty: false,
      getText: () => "export function foo() {}\n",
    } as vscode.TextDocument;

    const editor = {
      document: activeDoc,
      selection: new vscode.Selection(new vscode.Position(0, 6), new vscode.Position(0, 7)),
    } as vscode.TextEditor;

    const ideContext = extractIdeContext(editor, [activeDoc, otherDoc], 3);

    expect(ideContext.active_file).toBe("/path/to/repo/src/index.ts");
    expect(ideContext.selection).toEqual({
      path: "/path/to/repo/src/index.ts",
      range: {
        start: { line: 0, character: 6 },
        end: { line: 0, character: 7 },
      },
    });
    expect(ideContext.open_files).toEqual([
      "/path/to/repo/src/index.ts",
      "/path/to/repo/src/utils.ts",
    ]);
    expect(ideContext.dirty_buffers).toHaveLength(1);
    expect(ideContext.dirty_buffers![0].path).toBe("/path/to/repo/src/index.ts");
    expect(ideContext.dirty_buffers![0].byte_length).toBe(Buffer.byteLength("const x = 1;\n", "utf8"));
    expect(ideContext.diagnostics_revision).toBe(3);
  });

  it("finds target diagnostic overlapping cursor position or first error/warning", () => {
    const uri = vscode.Uri.file("/path/to/file.ts");
    const diag1 = {
      range: new vscode.Range(new vscode.Position(5, 0), new vscode.Position(5, 10)),
      severity: vscode.DiagnosticSeverity.Warning,
      message: "Unused variable",
    } as vscode.Diagnostic;

    const diag2 = {
      range: new vscode.Range(new vscode.Position(12, 4), new vscode.Position(12, 15)),
      severity: vscode.DiagnosticSeverity.Error,
      message: "Undefined variable foo",
    } as vscode.Diagnostic;

    vi.mocked(vscode.languages.getDiagnostics).mockImplementation((_arg?: unknown) => [diag1, diag2] as unknown as [vscode.Uri, vscode.Diagnostic[]][]);

    // Cursor at line 12 inside diag2
    const editor = {
      document: { uri },
      selection: new vscode.Selection(new vscode.Position(12, 6), new vscode.Position(12, 6)),
    } as unknown as vscode.TextEditor;

    const found = findTargetDiagnostic(editor);
    expect(found).toBeDefined();
    expect(found?.message).toBe("Undefined variable foo");
    expect(found?.severity).toEqual({ type: "Error" });

    // Cursor not overlapping any diagnostic: selects first error
    const editorFarAway = {
      document: { uri },
      selection: new vscode.Selection(new vscode.Position(100, 0), new vscode.Position(100, 0)),
    } as unknown as vscode.TextEditor;

    const fallbackFound = findTargetDiagnostic(editorFarAway);
    expect(fallbackFound).toBeDefined();
    expect(fallbackFound?.message).toBe("Undefined variable foo");
  });
});

describe("editor_actions_start_attributable_daemon_runs (Criterion 16)", () => {
  it("every editor action produces an ordinary StartRun/RunEditorAction daemon run carrying IdeContextUpdate and idempotency key", () => {
    const dispatchedActions: Array<{
      action: EditorNativeAction;
      context: EditorActionContext;
      model?: string;
    }> = [];

    const mockClient = {
      sessionId: "11111111-1111-4111-8111-111111111111",
      runEditorAction: vi.fn((action: EditorNativeAction, context: EditorActionContext, model?: string) => {
        dispatchedActions.push({ action, context, model });
      }),
    } as unknown as DaemonClient;

    const doc = {
      uri: vscode.Uri.file("/path/to/repo/src/app.ts"),
      isDirty: false,
      getText: () => "console.log('hello');\n",
    } as vscode.TextDocument;

    const editor = {
      document: doc,
      selection: new vscode.Selection(new vscode.Position(0, 0), new vscode.Position(0, 11)),
    } as vscode.TextEditor;

    const folders = [{ uri: vscode.Uri.file("/path/to/repo"), name: "repo", index: 0 }];
    const options = {
      workspaceFolders: folders,
      workspaceDocuments: [doc],
      diagnosticsRevision: 1,
      model: "test-model-4",
    };

    // 1. Explain Selection
    executeExplainSelection(mockClient, editor, options);
    expect(dispatchedActions).toHaveLength(1);
    expect(dispatchedActions[0].action).toEqual({ type: "ExplainSelection" });
    expect(dispatchedActions[0].context.ide.active_file).toBe("/path/to/repo/src/app.ts");
    expect(dispatchedActions[0].context.ide.selection?.range.end.character).toBe(11);
    expect(dispatchedActions[0].context.repository_id).toBe("/path/to/repo");
    expect(dispatchedActions[0].model).toBe("test-model-4");

    // 2. Fix / Refactor Selection
    executeFixSelection(mockClient, editor, options);
    expect(dispatchedActions).toHaveLength(2);
    expect(dispatchedActions[1].action).toEqual({ type: "FixSelection" });
    expect(dispatchedActions[1].context.ide.active_file).toBe("/path/to/repo/src/app.ts");

    // 3. Fix Diagnostic
    const diag = {
      range: new vscode.Range(new vscode.Position(0, 0), new vscode.Position(0, 7)),
      severity: vscode.DiagnosticSeverity.Error,
      message: "Cannot find name 'console'",
    } as vscode.Diagnostic;
    vi.mocked(vscode.languages.getDiagnostics).mockImplementation((_arg?: unknown) => [diag] as unknown as [vscode.Uri, vscode.Diagnostic[]][]);

    const fixResult = executeFixDiagnostic(mockClient, editor, undefined, options);
    expect(fixResult).toBe(true);
    expect(dispatchedActions).toHaveLength(3);
    expect(dispatchedActions[2].action).toEqual({
      type: "FixDiagnostic",
      diagnostic: {
        path: "/path/to/repo/src/app.ts",
        range: {
          start: { line: 0, character: 0 },
          end: { line: 0, character: 7 },
        },
        severity: { type: "Error" },
        message: "Cannot find name 'console'",
      },
    });

    // 4. Generate Tests for Selection
    executeGenerateTestsForSelection(mockClient, editor, options);
    expect(dispatchedActions).toHaveLength(4);
    expect(dispatchedActions[3].action).toEqual({ type: "GenerateTestsForSelection" });
    expect(dispatchedActions[3].context.ide.selection).toBeDefined();

    // 5. Review Current File
    executeReviewCurrentFile(mockClient, editor, options);
    expect(dispatchedActions).toHaveLength(5);
    expect(dispatchedActions[4].action).toEqual({ type: "ReviewCurrentFile" });
    expect(dispatchedActions[4].context.ide.active_file).toBe("/path/to/repo/src/app.ts");
  });
});

describe("package.json contributions and when-clause enablement (Criterion 15)", () => {
  const packageJsonPath = join(__dirname, "..", "package.json");
  const packageJson = JSON.parse(readFileSync(packageJsonPath, "utf8")) as {
    contributes: {
      commands: Array<{ command: string; title: string; enablement?: string }>;
      menus: {
        "editor/context"?: Array<{ command: string; when?: string }>;
        commandPalette?: Array<{ command: string; when?: string }>;
      };
    };
  };

  const expectedActions = [
    {
      id: "codypendent.explain",
      name: "Explain Selection",
      enablement: "editorHasSelection",
      menuWhen: "editorHasSelection",
    },
    {
      id: "codypendent.refactor",
      name: "Fix Selection",
      enablement: "editorHasSelection",
      menuWhen: "editorHasSelection",
    },
    {
      id: "codypendent.fixDiagnostic",
      name: "Fix Diagnostic",
      enablement: "editorTextFocus",
      menuWhen: "editorTextFocus",
    },
    {
      id: "codypendent.generateTests",
      name: "Generate Tests",
      enablement: "editorHasSelection",
      menuWhen: "editorHasSelection",
    },
    {
      id: "codypendent.reviewFile",
      name: "Review Current File",
      enablement: "editorTextFocus",
      menuWhen: "editorTextFocus",
    },
  ];

  it.each(expectedActions)(
    "contributes command $id with enablement $enablement and editor context menu",
    ({ id, enablement, menuWhen }) => {
      const cmd = packageJson.contributes.commands.find((c) => c.command === id);
      expect(cmd, `command ${id} must be in contributes.commands`).toBeDefined();
      expect(cmd?.enablement).toBe(enablement);

      const editorMenu = packageJson.contributes.menus["editor/context"]?.find((m) => m.command === id);
      expect(editorMenu, `command ${id} must be in contributes.menus["editor/context"]`).toBeDefined();
      expect(editorMenu?.when).toBe(menuWhen);

      const paletteMenu = packageJson.contributes.menus.commandPalette?.find((m) => m.command === id);
      expect(paletteMenu, `command ${id} must be in contributes.menus.commandPalette`).toBeDefined();
      expect(paletteMenu?.when).toBe(menuWhen);
    },
  );
});

describe("no extension-only model/tool calls (Criterion 17)", () => {
  function getAllSourceFiles(dir: string): string[] {
    const results: string[] = [];
    for (const entry of readdirSync(dir)) {
      const fullPath = join(dir, entry);
      const stat = statSync(fullPath);
      if (stat.isDirectory()) {
        results.push(...getAllSourceFiles(fullPath));
      } else if (fullPath.endsWith(".ts") || fullPath.endsWith(".tsx")) {
        results.push(fullPath);
      }
    }
    return results;
  }

  it("ensures no direct model/tool HTTP fetch calls exist in extension source code", () => {
    const srcDir = join(__dirname, "..", "src");
    const sourceFiles = getAllSourceFiles(srcDir);

    for (const file of sourceFiles) {
      const content = readFileSync(file, "utf8");
      // Check for fetch( or direct HTTP requests to LLM / model / tool endpoints
      expect(content).not.toMatch(/fetch\s*\(/);
      expect(content).not.toMatch(/https?:\/\/(?!localhost|127\.0\.0\.1)[^\s"']*\/v1\/(chat|completions|embeddings)/i);
    }
  });
});
