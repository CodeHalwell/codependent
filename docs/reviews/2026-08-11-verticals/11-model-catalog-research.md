# Provider model-catalog research — 2026-08-11

Web research supporting the curated `[[model]]` prefill added to
`crates/providers/builtin_catalog.toml`. Every "live" row below was captured
from an open (or probed) endpoint on 2026-08-11; docs rows come from official
provider documentation. `(?)` marks uncertain/inferred data — those values
were **not** written into the catalog.

## Discovery endpoint support matrix (`GET {base}/models`, probed 2026-08-11)

| Provider | Unauth result | With key | Notes |
|---|---|---|---|
| **Nebius Token Factory** | 401 on `/v1/models` | ✅ (`?verbose=true` adds ctx/pricing) | **`https://tokenfactory.nebius.com/api/public/models_info` is OPEN (no key): full JSON with ctx/price/capabilities; human view at `/model-catalog.md`. Ideal live-discovery source — the prefill can be auto-regenerated from it.** |
| OpenRouter | ✅ 200 (405 models) | ✅ | Richest metadata: ctx, pricing, `supported_parameters`, `:batch` variants, `~…-latest` aliases |
| SambaNova | ✅ 200 | ✅ | Includes `context_length`, `max_completion_tokens`, pricing |
| DeepInfra | ✅ 200 (both `/v1/openai/models` and `/models/list`) | ✅ | `/models/list` very rich (pricing, `tools` tag, deprecation, `replaced_by`) |
| Novita | ✅ 200 | ✅ | ctx, pricing, `features` (function-calling/structured-outputs/reasoning) |
| Venice | ✅ 200 | ✅ | Rich `model_spec` (pricing, capabilities incl. vision/reasoning) |
| Featherless | ✅ 200 — **21,698 models** | ✅ | Any-HF-model host; never prefill, always live-discover |
| Chutes | ✅ 200 but partial (TEE subset) | ✅ full | Unauth shows only confidential-compute `-TEE` models |
| Inference.net | ✅ 200 (38 bare ids) | ✅ | Now a router serving frontier closed models too |
| OpenCode Zen | ✅ 200 (63 bare ids) | ✅ | Curated coding-model gateway |
| OpenAI / Anthropic / xAI / DeepSeek / Mistral / Moonshot / Together / Groq / Fireworks / DashScope / Cohere | 401 | ✅ | Standard key-gated listing. Anthropic also serves `/v1/models/{id}` with `max_input_tokens`/`capabilities`. xAI adds `/v1/language-models` with pricing |
| Gemini (OpenAI-compat) | 404 unauth | ✅ with Bearer key | 404-without-key is normal for this path |
| Cerebras | 403 | ✅ | |
| Z.ai / MiniMax / Parasail / Baseten | 401 | ✅(?) | Baseten's authed listing returns pricing/ctx/features per docs |
| Hyperbolic | 401 on `.xyz` | (?) | **Domain moved `api.hyperbolic.xyz` → `api.hyperbolic.ai`** (site + docs redirect); catalog base_url likely needs updating |
| Lambda | unreachable | — | **Lambda Inference API is winding down** per lambda.ai/inference — flag/remove |
| Perplexity | **404 — no listing endpoint** | ❌ | Static prefill is the only option |
| AI21 | **410 "This API has been retired"** | ❌ | Jamba/Maestro APIs sunset 2026-08-09 — remove or mark dead |
| GitHub Models | **410 retirement** | ❌ | **Fully retired 2026-07-30** (catalog, inference, BYOK) — remove |
| Ollama / LM Studio / vLLM | ✅ when running | — | Live discovery only |

## Catalog corrections (action items)

- **Drop or flag**: AI21 (retired), GitHub Models (retired), Lambda (winding down).
- **Update**: Hyperbolic base_url `.xyz` → `.ai` once verified with a key.
- **Nebius**: the prefill can be auto-generated from the open `models_info`
  endpoint — a natural "refresh catalog" command target.
- Naming-currency note: the current frontier is GPT-5.4/5.5/5.6(sol/terra/luna),
  Claude Fable/Opus/Sonnet 5 + Opus 4.6-4.8, Gemini 3.1/3.5/3.6, Grok
  4.3/4.5/4.20 + grok-build, DeepSeek V4, Kimi K2.5-K3, GLM-5.x, Qwen 3.5-3.8,
  MiniMax M2.5-M3. Llama 4 is effectively absent from serious hosts.

## Nebius Token Factory (authoritative, live 2026-08-11)

Base `https://api.tokenfactory.nebius.com/v1/`. 29 public models (text +
vision + 1 embedding; **no image-gen, STT, or TTS currently**). Capability
flags are Nebius's own metadata. Where `max_model_len` read `8000` that is the
deployed default completion window, not the model max (?). Regions
(us-central1 / eu-north1 / eu-west2 / uk-south1) are in the JSON.

| id | ctx | tools | vision | reasoning | $in/$out per Mtok |
|---|---|---|---|---|---|
| `deepseek-ai/DeepSeek-V4-Pro` | 1M | ✅ | — | ✅ | 1.75 / 3.50 |
| `deepseek-ai/DeepSeek-V4-Flash` | 131K | (?) | — | (?) | 0.14 / 0.28 |
| `zai-org/GLM-5.2` | 1M | ✅ | — | ✅ | 1.40 / 4.40 |
| `zai-org/GLM-5.1` | 200K | ✅ | — | ✅ | 1.40 / 4.40 |
| `moonshotai/Kimi-K3` | 1M | ✅ | ✅ | ✅ | 3.00 / 15.00 |
| `moonshotai/Kimi-K2.7-Code` | 256K | ✅ | ✅ | ✅ | 0.95 / 4.00 |
| `moonshotai/Kimi-K2.6` | 256K | ✅ | ✅ | ✅ | 0.95 / 4.00 |
| `MiniMaxAI/MiniMax-M3` | 1M | ✅ | — | ✅ | 0.30 / 1.20 |
| `MiniMaxAI/MiniMax-M2.5` | 196K | ✅ | — | ✅ | 0.30 / 1.20 |
| `Qwen/Qwen3.5-397B-A17B` | 262K | ✅ | — | ✅ | 0.60 / 3.60 |
| `Qwen/Qwen3-235B-A22B-Instruct-2507` | 262K | ✅ | — | — | 0.20 / 0.60 |
| `Qwen/Qwen3-30B-A3B-Instruct-2507` | 262K | ✅ | — | — | 0.10 / 0.30 |
| `Qwen/Qwen3-32B` | 41K | ✅ | — | ✅ | 0.10 / 0.30 |
| `Qwen/Qwen3-Next-80B-A3B-Thinking` | 128K | ✅ | — | ✅ | 0.15 / 1.20 |
| `Qwen/Qwen2.5-VL-72B-Instruct` | 32K | — | ✅ | — | 0.25 / 0.75 |
| `nvidia/Nemotron-3_5-Lightning` | 1M | ✅ | — | ✅ | 0.06 / 0.24 |
| `nvidia/Nemotron-3-Ultra-550b-a55b` | 1M | ✅ | — | ✅ | 1.00 / 3.00 |
| `nvidia/nemotron-3-super-120b-a12b` | 256K | ✅ | — | ✅ | 0.30 / 0.90 |
| `nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B` | 262K | ✅ | — | ✅ | 0.06 / 0.24 |
| `nvidia/Nemotron-3-Nano-Omni` | 262K | ✅ | ✅ | ✅ | 0.06 / 0.24 |
| `nvidia/Cosmos3-Super-Reasoner` | 256K | ✅ | ✅ | ✅ | 0.10 / 0.30 |
| `nvidia/Llama-3_1-Nemotron-Ultra-253B-v1` | 128K | — | — | ✅ | 0.60 / 1.80 |
| `openai/gpt-oss-120b` | 131K | ✅ | — | ✅ | 0.15 / 0.60 |
| `meta-llama/Llama-3.3-70B-Instruct` | 128K | ✅ | — | — | 0.13 / 0.40 |
| `google/gemma-3-27b-it` | 110K | ✅ | — | — | 0.10 / 0.30 |
| `NousResearch/Hermes-4-405B` | 128K | ✅ | — | ✅ | 1.00 / 3.00 |
| `NousResearch/Hermes-4-70B` | 128K | ✅ | — | ✅ | 0.13 / 0.40 |
| `openbmb/MiniCPM-V-4_5` | 32K | — | ✅ | — | 0.658 / 1.11 |
| `Qwen/Qwen3-Embedding-8B` | 32K | — | — | — | 0.01 / — (embedding) |

## Other providers (summary; full details in the catalog rows)

- **Groq** (docs): `llama-3.1-8b-instant`, `llama-3.3-70b-versatile`,
  `openai/gpt-oss-120b/20b`, `groq/compound(-mini)` (built-in web search +
  code exec), previews `qwen/qwen3.6-27b`, `minimaxai/minimax-m2.7`;
  moderation models. Catalog trimmed heavily vs 2025 (Llama-4, Kimi-K2,
  playai-tts all removed).
- **Cerebras** (docs): only `gpt-oss-120b` (prod, ~3000 tok/s),
  `gemma-4-31b` (preview), `zai-glm-4.7` (preview, deprecates 2026-08-17 —
  excluded from prefill).
- **Together** (docs): Inkling(+Small), MiniMax-M3, Qwen3.5-3.7, Kimi
  K2.6-K3, GLM-5.2, gpt-oss, DeepSeek V4, Nemotron 3 Ultra, Llama 3.3 Turbo,
  gemma-4; STT `openai/whisper-large-v3`, `nvidia/parakeet-tdt-0.6b-v3`; TTS
  `cartesia/sonic-3`, `hexgrad/Kokoro-82M`.
- **Fireworks** (docs pricing): Kimi K3 3.00/15.00 · K2.7-Code 0.95/4.00 ·
  K2.6 0.95/4.00 · DeepSeek V4 Pro 1.74/3.48 · V4 Flash 0.14/0.28 · GLM 5.2
  1.40/4.40 · GLM 5.1 1.40/4.40 · Qwen3.7 Plus 0.40/1.60 · MiniMax M3
  0.30/1.20 · gpt-oss-120b 0.15/0.60. **Slug format
  `accounts/fireworks/models/<slug>` with `p` for dots (?) — excluded from
  prefill; use live discovery.**
- **DeepInfra** (live): full table in catalog; also proxies closed frontier
  models (`anthropic/claude-*`, `google/gemini-*`) and serves a large
  STT/TTS roster.
- **SambaNova** (live): 6 models, exact ctx/prices in catalog.
- **Novita** (live): 146 models; the ~14 most catalog-worthy included with
  exact prices.
- **Venice** (live): native/open models included; also proxies closed
  frontier models at ~1.25× upstream price (venice-prefixed ids — excluded
  from prefill to avoid id confusion).
- **Chutes** (live, partial): TEE tier included; non-TEE ids drop the
  suffix (?).
- **Baseten** (docs): slugs + ctx included; authed `/models` returns pricing.
- **Hyperbolic / Parasail / Featherless / Lambda**: excluded (domain move /
  key-gated list / 21k models / winding down respectively).
- **OpenRouter** (live): flagship coding set included with exact prices;
  `openai/gpt-5.6-terra` and `-luna` prices conflict between OpenRouter
  (1/6, 0.10/0.60) and OpenAI's own page (2/12, 0.20/1.20) — OpenAI's page
  used for the `openai` provider rows; OpenRouter rows omit the two.
- **OpenCode Zen** (live): curated coding gateway; bare-id rows included,
  incl. a `-free` tier (`deepseek-v4-flash-free`, `nemotron-3.5-lightning-free`, …).
- **Frontier direct**: OpenAI (pricing page: gpt-5.6-sol/terra/luna 5/30 ·
  2/12 · 0.20/1.20; gpt-5.5(-pro), gpt-5.4(-mini/-nano), gpt-5.3-codex;
  realtime `gpt-realtime-2.1`), Anthropic (claude-fable-5 10/50,
  claude-opus-5 and opus-4.6-4.8 5/25, claude-sonnet-5 3/15 — intro 2/10
  through 2026-08-31, claude-haiku-4-5 1/5; 1M ctx / 128K out across the 5
  family, haiku 200K; `claude-mythos-5` exists but is access-gated —
  excluded), Gemini (3.6-flash 1.50/7.50, 3.1-pro-preview 2/12,
  3.5-flash(-lite), 3.1-flash-lite 0.25/1.50, maintained 2.5 family, TTS/live
  variants), xAI (grok-4.5 500K 2/6 with ≥200K tier 4/12; grok-4.3 1M
  1.25/2.50; grok-4.20 variants; `grok-build-0.1` 256K 1/2), DeepSeek
  (v4-flash 1M 0.14/0.28 cache-hit 0.0028; v4-pro 0.435/0.87; legacy
  `deepseek-chat`/`-reasoner` aliases deprecated 2026-07-24), Mistral
  (`-latest` aliases: medium 3.5 / large 3 / small 4 / codestral; Voxtral
  STT/TTS; hosted `glm-5.2`), Moonshot (kimi-k3 1M 3/15 with cache-hit 0.30,
  `reasoning_effort` low/high/max; k2.7-code, k2.6, k2.5; moonshot-v1 sunsets
  2026-08-31; Anthropic-compat endpoint available), Z.ai (glm-5.2/5.1/5,
  turbo/flash tiers, `glm-asr-2512` STT), MiniMax (M3 1M 0.30/1.20 ≤512K;
  M2.7/M2.5; Anthropic-compat `/anthropic`; id casing varies by surface (?)),
  Qwen/DashScope (qwen3.7-max/plus, qwen3.6-plus/flash 1M, qwen3.5-plus,
  qwen3-coder-plus (?); hybrid thinking default on 3.5+; pin dated snapshots
  for stability), Perplexity (sonar family; no listing endpoint; Agent API
  superseding chat completions), Cohere (command-a-plus-05-2026,
  command-a-03-2025, reasoning/vision/translate variants, `embed-v4.0`,
  `rerank-v4.0`, STT `cohere-transcribe-03-2026`).

## STT / TTS capable providers (for the voice objective)

| Provider | STT | TTS |
|---|---|---|
| Groq | `whisper-large-v3`, `whisper-large-v3-turbo` | `canopylabs/orpheus-v1-english` (preview; playai-tts removed) |
| OpenAI | `gpt-4o-transcribe` ($0.006/min), `gpt-4o-mini-transcribe` ($0.003/min), `whisper-1` | `gpt-4o-mini-tts` (?), `tts-1(-hd)` (?); realtime S2S `gpt-realtime-2.1(-mini)` |
| DeepInfra | whisper-large-v3(-turbo), Voxtral Small/Mini, Qwen3-ASR, Nemotron-3.5-ASR-Streaming | Kokoro-82M, Qwen3-TTS, Orpheus, Chatterbox, Inworld realtime, MiMo-tts, HiggsAudio, sesame/csm-1b |
| Together | whisper-large-v3, parakeet-tdt-0.6b-v3, nemotron-asr | cartesia/sonic-3/-2, orpheus, Kokoro-82M |
| Mistral | `voxtral-mini-transcribe-2` (+realtime), `voxtral-small-latest` (audio chat) | Voxtral TTS v26.03 (voice cloning) |
| Gemini | native audio-in on 3.x/2.5; live-translate previews | `gemini-3.1-flash-tts-preview`, `gemini-2.5-*-preview-tts` |
| xAI | STT $0.10/hr | TTS $15/1M chars; S2S `grok-voice-think-fast` |
| Z.ai | `glm-asr-2512` | — |
| Cohere | `cohere-transcribe-03-2026` | — |
| MiniMax | — | `speech-2.6-hd/-turbo` (?) |
| Deepgram (dedicated) | `nova-3`, `flux-general-en/multi` | `aura-2` |
| ElevenLabs (dedicated) | `scribe_v2(_realtime)` | `eleven_v3`, `eleven_flash_v2_5` |
| **Nebius** | none currently | none currently |

**Implication for the voice objective:** STT/TTS can ride the existing
OpenAI-compatible provider plumbing (`{base}/audio/transcriptions`,
`{base}/audio/speech`) for Groq/OpenAI/DeepInfra/Together — no new provider
protocol is needed for voice v1.

## Sources

Nebius `tokenfactory.nebius.com/api/public/models_info` + `/model-catalog.md`;
live `/models` dumps from OpenRouter, SambaNova, DeepInfra, Novita, Venice,
Chutes, Inference.net, OpenCode Zen, Featherless; official docs for Groq,
Cerebras, Together, Fireworks, Baseten, Parasail, Lambda, OpenAI, Anthropic,
Gemini, xAI, DeepSeek, Mistral, Moonshot (platform.kimi.ai), Z.ai, MiniMax,
Qwen/DashScope, Perplexity, AI21 (deprecation notice), Cohere, GitHub Models
(retirement changelog), Deepgram, ElevenLabs. All captured 2026-08-11.
