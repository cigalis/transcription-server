# Transcription Server

Docker Compose deployment for an AMD Vulkan transcription and cleanup pipeline:

- Qwen3-ASR-1.7B Q8_0 runs through llama.cpp Vulkan.
- A small Rust proxy exposes an OpenAI-compatible multipart transcription endpoint.
- Gemma 4 E4B Q4 runs through llama.cpp Vulkan to clean up returned text.

## Requirements

- Docker Engine with Docker Compose.
- An AMD GPU exposed through Vulkan at `/dev/dri/renderD129`.
- Access to the `render` and `video` groups for the container user.

The defaults target a machine where the render and video group IDs are `991` and `44`. Override them when needed:

```bash
export RENDER_GID="$(getent group render | cut -d: -f3)"
export VIDEO_GID="$(getent group video | cut -d: -f3)"
```

## Start

Create the persistent Hugging Face cache volume once, then build and start the services:

```bash
docker volume create vllm-hf-cache
docker compose build
docker compose up -d
```

The first start downloads the Qwen and Gemma GGUF models into the persistent cache.

## Services

| Service | Host port | Model | Purpose |
| --- | ---: | --- | --- |
| `asr-proxy` | 8000 | Qwen3-ASR-1.7B Q8_0 | OpenAI-compatible audio transcription API |
| `gemma4` | 8001 | Gemma 4 E4B UD-Q4_K_XL | Text cleanup API |

Qwen remains internal to the Compose network. The proxy handles multipart uploads, Base64 conversion for llama.cpp, and removes Qwen's native `language ...<asr_text>` prefix.

## Transcription API

```bash
curl -X POST http://localhost:8000/v1/audio/transcriptions \
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
curl http://localhost:8001/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "gemma-4-e4b-it-ud-q4_k_xl",
    "messages": [{"role": "user", "content": "Clean up this transcription."}]
  }'
```

## Security

The services intentionally have no authentication and bind to all interfaces. Restrict ports `8000` and `8001` to trusted LAN clients with a firewall or reverse proxy before exposing this deployment beyond a trusted network.
