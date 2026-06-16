# penfreely-bridge

Run a language model on **your own machine** (via [Ollama](https://ollama.com))
and use it inside [PenFreely](https://penfreely.com) — for free. The inference
happens on your hardware, so no Ink is spent and your text never leaves your
computer.

The agent makes only an **outbound** secure connection to the service, so there
are **no ports to open** — it works behind routers and firewalls. It is a small,
self-contained binary; no Docker required.

> Step-by-step guide with download buttons per OS:
> **https://penfreely.com/local-models**

## Quick start

1. **Install Ollama** and pull a model:
   ```bash
   ollama pull llama3.2
   ```
2. **Get a bridge token** in PenFreely: *Studio → Connect your machine → Create a
   token*. It is shown once — copy it.
3. **Download** the binary for your system from the
   [latest release](https://github.com/fedorello/penfreely-bridge/releases/latest)
   and run it with your token.

### macOS / Linux

```bash
tar xzf penfreely-bridge-*.tar.gz
chmod +x penfreely-bridge
PENFREELY_BRIDGE_TOKEN="<your-token>" \
  PENFREELY_BACKEND_WS_URL="wss://app.penfreely.com/bridge/connect" \
  ./penfreely-bridge
```

On macOS the first run may be blocked as “from an unidentified developer”. Clear
the quarantine flag (`xattr -d com.apple.quarantine penfreely-bridge`) or
right-click the file → Open once.

### Windows (PowerShell)

```powershell
Expand-Archive penfreely-bridge-*.zip -DestinationPath .
$env:PENFREELY_BRIDGE_TOKEN="<your-token>"
$env:PENFREELY_BACKEND_WS_URL="wss://app.penfreely.com/bridge/connect"
.\penfreely-bridge.exe
```

Leave the window open — while it runs, your model is available in PenFreely under
**Your machine · local**, marked free.

## Configuration

All settings are environment variables; only the token is required.

| Variable | Required | Default | Purpose |
|----------|:--------:|---------|---------|
| `PENFREELY_BRIDGE_TOKEN` | yes | — | Bridge token from the service |
| `PENFREELY_BACKEND_WS_URL` | no | `ws://localhost:8080/bridge/connect` | Backend websocket (production: `wss://app.penfreely.com/bridge/connect`) |
| `PENFREELY_OLLAMA_URL` | no | `http://localhost:11434` | Local Ollama address |
| `PENFREELY_RECONNECT_INITIAL_MS` | no | `1000` | Initial reconnect backoff |
| `PENFREELY_RECONNECT_MAX_MS` | no | `30000` | Maximum reconnect backoff |
| `RUST_LOG` | no | `info` | Log level |

## How it works

1. The agent opens an outbound websocket to the backend and handshakes on a
   protocol version.
2. It reports your local Ollama models, which appear in the studio's model
   picker, marked free.
3. When you write a page with a local model, the backend sends the request down
   to the agent, which calls Ollama and **streams the tokens back**. No Ink is
   spent.
4. If the link drops, the agent reconnects with backoff.

The connection is outbound only; no cloud provider ever sees your text — the
inference runs on your hardware.

## Build from source

```bash
cargo build --release
# binary: target/release/penfreely-bridge
cargo test
```

## License

MIT — see [LICENSE](LICENSE).
