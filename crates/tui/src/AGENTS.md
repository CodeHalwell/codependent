# Codypendent TUI Invariants

1. Unidirectional data flow: `Event -> Action -> reduce(AppState) -> render(Frame)`.
2. Pure reduce: `reduce` is deterministic and strictly performs no I/O.
3. No I/O in widgets: widgets only read state and produce render buffers.
4. Colors only via `Theme`: widgets never hardcode RGB or ANSI palette constants.
5. Keyboard parity: every mouse-clickable affordance has a keyboard binding.
6. Bounds and clipping: UI views must never panic on zero or small terminal dimensions.
7. Outbox intents: effects and mutations are dispatched through `state.outbox`.
