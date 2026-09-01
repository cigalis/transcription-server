# Transcription Server

Docker Compose deployment for an AMD Vulkan transcription and cleanup pipeline:

- Qwen3-ASR-1.7B Q8_0 runs through llama.cpp Vulkan.
- A small Rust proxy exposes an OpenAI-compatible multipart transcription endpoint.
- Gemma 4 E4B Q4 runs through llama.cpp Vulkan to clean up returned text.

## Requirements

- Docker Engine with Docker Compose.
- An AMD GPU exposed through Vulkan at `/dev/dri/renderD129`.
- Access to the `render` and `video` groups for the container user.
- The DNS name `emplacement-n1-developpement.pro.dns-orange.fr` pointing to the public IP of this host.

The defaults target a machine where the render and video group IDs are `991` and `44`. Override them when needed:

```bash
export RENDER_GID="$(getent group render | cut -d: -f3)"
export VIDEO_GID="$(getent group video | cut -d: -f3)"
```

## Start

Create the persistent Hugging Face cache volume once, then build and start the services:

```bash
docker volume create vllm-hf-cache
cp .env.example .env
umask 077
openssl rand -hex 32 | { IFS= read -r key; printf 'LLAMA_API_KEY=%s\n' "$key"; } > .env
chmod 600 .env
docker compose build
docker compose up -d
```

The first start downloads the Qwen and Gemma GGUF models into the persistent cache.

The key is written directly to `.env`; it is never printed or included in the command history. Load it into the current shell before using the examples below:

```bash
set -a
. ./.env
set +a
```

## Services

| Service | Public URL | Model | Purpose |
| --- | ---: | --- | --- |
| `asr-proxy` | `https://emplacement-n1-developpement.pro.dns-orange.fr/v1/audio/transcriptions` | Qwen3-ASR-1.7B Q8_0 | OpenAI-compatible audio transcription API |
| `gemma4` | `https://emplacement-n1-developpement.pro.dns-orange.fr/gemma/v1` | Gemma 4 E4B UD-Q4_K_XL | Text cleanup API |

Qwen, the ASR proxy, and Gemma remain internal to the Compose network. Caddy is the only service that publishes ports on the host, obtains the TLS certificate, and forwards requests to the APIs.

## Transcription API

```bash
curl -X POST https://emplacement-n1-developpement.pro.dns-orange.fr/v1/audio/transcriptions \
  -H "Authorization: Bearer $LLAMA_API_KEY" \
  -F "file=@recording.wav" \
  -F "model=qwen3-asr-1.7b"
```

The response follows the OpenAI transcription shape:

```json
{"text":"The transcribed text"}
```

Audio uploads are limited to 256 MiB by default. Change `MAX_AUDIO_BYTES` in `compose.yaml` if required.

## Gemma API

```bash
curl https://emplacement-n1-developpement.pro.dns-orange.fr/gemma/v1/chat/completions \
  -H "Authorization: Bearer $LLAMA_API_KEY" \
  -H "X-Text-Language: fr" \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "gemma-4-e4b-it-ud-q4_k_xl",
    "messages": [{"role": "user", "content": "Clean up this transcription."}]
  }'
```

## Security

All public APIs require `Authorization: Bearer $LLAMA_API_KEY`. The local `.env` file is ignored by Git, has permissions `0600`, and must never be committed.

Gemma uses deterministic sampling and a single generation slot by default, favoring reproducible cleanup over concurrent requests. Send `X-Text-Language: fr` for French cleanup: the proxy then applies French nonbreaking spaces before `:`, `;`, `!`, and `?`, without changing text inside quotation marks. Requests without that header are forwarded unchanged.

Configure the router with these port forwards to `192.168.1.57` before starting Caddy. Port 80 is required for Let's Encrypt validation and redirects to HTTPS. Port 443 serves the API; its UDP mapping enables HTTP/3 but is optional.

| Protocol | Public port | Destination |
| --- | ---: | --- |
| TCP | 80 | `192.168.1.57:80` |
| TCP | 443 | `192.168.1.57:443` |
| UDP | 443 | `192.168.1.57:443` (optional) |
