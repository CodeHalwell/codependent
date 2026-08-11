# Migrating browser React to semantic Remote UI

A browser component cannot be embedded directly in a terminal. Keep stateful React composition and hooks, but replace browser rendering and ambient authority at the boundary.

| Browser pattern | Remote UI replacement |
| --- | --- |
| `div`, CSS grid/flex | `Stack`, `Row`, `Grid`, `Split`, explicit variants |
| DOM event object | revision-bound `UiEvent` handler |
| `fetch`, WebSocket | declared host projection or command |
| filesystem/process API | host command with permission review |
| arbitrary HTML/Markdown | semantic `Text`, `Markdown`, `Code`, `Table` |
| canvas/chart library | `Chart`/`Graph` plus terminal-safe fallback |
| CSS media query | `useViewport`, `useCapabilities`, `TerminalOnly`/`WebOnly` |
| local theme/CSS variables | `useTheme` semantic tokens |
| modal approval/secret form | host-owned core surface; components cannot replace it |

Start by extracting data from rendering. Convert the view to semantic primitives, give stable IDs to interactive/keyed nodes, attach accessible labels, and define a useful terminal fallback for every web-specific surface. Then replace data reads with the narrowest projection kind and writes with a declared command. Keep callbacks local; only their canonical `props.eventHandlers` presence declaration crosses the boundary. Use `onPress` rather than a DOM `onClick`, and do not combine a local handler with the same host-mediated `action` binding.

Browser-only packages should use separate `web` and terminal/shared entrypoints. A web-only contribution must declare `fallback_renderer` in `plugin.toml`. Avoid renderer-component callback props and giant context values: compose through children and subscribe to the smallest projection needed by each leaf.

Run `codypendent-ui validate`, the accessibility audit, schema fixtures, and `codypendent-ui inspect` before packaging. Migration is complete only when the component remains useful with monochrome color, keyboard-only input, reduced motion, no mouse, and an 80×24 viewport.
