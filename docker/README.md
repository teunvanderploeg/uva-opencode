# UvA AI Chat bridge (Docker)

Run the UvA AI Chat bridge in a container, anywhere. OpenCode and T3 reach
`aichat.uva.nl` through it as an OpenAI-compatible provider. A one-time UvA login
(with MFA) happens in a browser window shown over VNC; afterward the same container
runs headless.

Nothing here depends on Kubernetes. The login profile lives in a mounted volume.

## Build

```sh
docker build -t uva-bridge docker/
```

## 1. Log in once (needs a screen)

The UvA login is a visible browser window with MFA. Run the VNC mode for that one step:

```sh
docker run --rm -it -p 6080:6080 -p 8765:8765 \
  -e UVA_BRIDGE_KEY="$(openssl rand -hex 32)" \
  -e UVA_VNC=1 -e VNC_PASSWORD='choose-8-chars' \
  -v uva-profile:/data \
  uva-bridge
```

Open http://localhost:6080/vnc.html, connect with the VNC password, sign in to
`aichat.uva.nl`, complete MFA, and leave the window open. Stop the container when the
bridge logs `UvA bridge on 0.0.0.0:8765`. The profile stays in the `uva-profile` volume.

## 2. Run headless

```sh
docker run -d --name uva-bridge -p 127.0.0.1:8765:8765 \
  -e UVA_BRIDGE_KEY="same-key-as-step-1" \
  -v uva-profile:/data \
  uva-bridge
```

Without `UVA_VNC=1` the container starts no display. It needs the logged-in profile in
`/data`. If UvA expires the session, run step 1 again.

## 3. Point OpenCode or T3 at it

Use an OpenAI-compatible provider with base URL `http://127.0.0.1:8765/v1` and the key
in `UVA_BRIDGE_KEY` as the API key. The full model list is in
[`opencode.example.json`](opencode.example.json) (`providers.uva.models`) and is served
by `/v1/models`.

Two providers cover the two modes the bridge offers:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "uva": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "UvA AI Chat",
      "options": {
        "baseURL": "http://127.0.0.1:8765/v1",
        "apiKey": "{env:UVA_BRIDGE_KEY}"
      },
      "models": {
        "gpt-6-luna": { "name": "UvA GPT 6 Luna", "tool_call": false, "limit": { "context": 32000, "output": 4096 } }
      }
    },
    "uva-tools": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "UvA AI Chat prompt tools (experimental)",
      "options": {
        "baseURL": "http://127.0.0.1:8765/v1",
        "apiKey": "{env:UVA_BRIDGE_KEY}"
      },
      "models": {
        "gpt-6.1-sol": { "name": "UvA GPT 6.1 Sol (tools)", "tool_call": true, "limit": { "context": 32000, "output": 4096 } }
      }
    }
  }
}
```

`uva` is plain chat: no tool envelope, so it is cheaper. `uva-tools` is the experimental
prompt-tools mode.

## How the prompt-tools mode works

UvA's chat has no tool API, so the bridge emulates tool calling with a prompt and asks
for one JSON envelope per reply. The prompt is engineered so a run settles instead of
looping:

- **One line per tool.** Each tool is `name(arg*: type, ...) - first sentence of its
  description`, not the full JSON schema (the schemas for a ~78-tool client were ~52k
  tokens, over the 32k window). Only a core set (`UVA_CORE_TOOLS`) is listed when the
  client offers one; validation still accepts every tool the client sent.
- **Client system text is dropped.** The base prompt and instruction files do not fit
  the 32k window with the task and tool results, so the transport instructions carry the
  protocol. The plain path keeps the client system text.
- **Conversation and tool results are capped.** The bridge keeps the task plus the newest
  turns, and trims each tool result to its head and tail with a marker naming the omitted
  count, so command output does not crowd out the reply.
- **One repair, then fail closed.** A reply that is not one valid envelope is retried once
  with the reason (and, if it was cut off, told to keep it shorter); a second failure is a
  422 and no tool ever runs.
- **A transient bare UvA error is retried once**, then returned.

Tools run in the client, never in the bridge. A request without tools is plain text
(no envelope).

## Environment

| Variable | Default | Purpose |
| --- | --- | --- |
| `UVA_BRIDGE_KEY` | required | Bearer token. Every request must send it. |
| `UVA_VNC` | unset | `1` starts the virtual screen and noVNC for login. |
| `VNC_PASSWORD` | required with `UVA_VNC=1` | VNC password. Only the first 8 characters count (classic VNC auth). |
| `UVA_BIND` | `127.0.0.1` | Interface the bridge listens on. |
| `UVA_ALLOWED_HOSTS` | `127.0.0.1:8765,localhost:8765` | Host headers the bridge accepts. |
| `UVA_STATE_DIR` | `/data` in the image | Browser profile location. Mount a volume here. |
| `UVA_CHROME_SANDBOX` | `0` in the image | Chromium sandbox off, because the non-root container lacks user namespaces. |
| `UVA_MAX_CONVERSATION_TOKENS` | `16000` | Prompt mode keeps at most this much of the conversation; `0` disables the cap. |
| `UVA_MAX_TOOL_RESULT_CHARS` | `8000` | Each tool result is trimmed to its head and tail; `0` disables the cap. |
| `UVA_UPSTREAM_ATTEMPTS` | `2` | Total attempts for a transient bare UvA error (`1` disables the retry). |
| `UVA_UPSTREAM_RETRY_SECONDS` | `5` | Pause before the one retry above. |
| `UVA_CORE_TOOLS` | `bash,read,write,edit,glob,grep,webfetch` | Core tool set listed in prompt mode. |
| `UVA_LIST_ALL_TOOLS` | unset | `1` lists every client tool instead of the core set. |

## Checks

```sh
cd docker && python3 -m unittest test_transport test_bridge
```

## Not verified here

No Docker in the sandbox that wrote this. The image build, Chromium inside the container,
the noVNC login and a full OpenCode tool loop are unverified.
