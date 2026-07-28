# Context-window protection and visibility

Codypendent can track how full a model's context window is getting during a
run, and can ask Ollama to use the model's real window instead of its small
default. Both are driven by one optional setting.

## Enable it: `context_tokens` in `models.toml`

Add `context_tokens` to a `[[model]]` entry in
`<config_dir>/codypendent/models.toml`:

```toml
[[model]]
id = "local-default"
provider = "openai-compatible"
base_url = "http://localhost:11434/v1"
model = "qwen2.5-coder:14b"
api_key_env = ""
context_tokens = 32768   # the model's real context window, in tokens
```

`context_tokens` is `Option<u64>` on `ModelConfig`
(`crates/runtime/src/models.rs`) and defaults to unset — existing
`models.toml` files parse unchanged.

Setting it does two things:

1. **Live `ctx N%` gauge in the TUI footer.** The agent loop estimates the
   transcript's token usage each step and, when the window is known, emits
   a budget event that the TUI turns into a percentage (`ctx 41%`, rising
   toward `100%` as the window fills). This is a cheap character-based
   estimate (~4 chars/token), not an exact token count — good enough to warn,
   not meant to bill.
2. **`num_ctx` sent with the request.** The client attaches
   `{"options":{"num_ctx":32768}}` to the chat-completions request body, so
   an Ollama server that honors it uses the real window instead of its
   default (often 2048–4096).

## If `context_tokens` is unset

The footer shows `—`, never a fabricated percentage, and no `num_ctx` hint is
sent. This is intentional: an unknown window must never be guessed at.

## The `num_ctx` caveat

Codypendent sends `num_ctx` as a nested `options` object on the request body.
Whether a given Ollama build actually **honors** a nested
`options.num_ctx` on `/v1/chat/completions` is version-dependent — it is not
guaranteed by this codebase. If the endpoint ignores it, the request is
unaffected; nothing breaks.

For a guaranteed context-window change, set it server-side instead:

- A Modelfile: `PARAMETER num_ctx <n>`, then `ollama create` a variant with
  that Modelfile, or
- The server environment variable `OLLAMA_CONTEXT_LENGTH=<n>` before starting
  `ollama serve`.

Either of these is authoritative regardless of whether the in-request hint is
honored.
