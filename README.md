<div align="center">

# airwire

### Anonymous Internet Relay With Integrated Routing & Encryption

Accountless, end-to-end encrypted rooms for your terminal.

[![Release](https://img.shields.io/github/v/release/H34TB145T/airwire?style=flat-square&color=78b8b4)](https://github.com/H34TB145T/airwire/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/H34TB145T/airwire/ci.yml?style=flat-square&label=build)](https://github.com/H34TB145T/airwire/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/H34TB145T/airwire?style=flat-square)](LICENSE-MIT)
[![Rust](https://img.shields.io/badge/built_with-Rust-202020?style=flat-square&logo=rust)](https://www.rust-lang.org/)

**Text · Groups · Files · Media · Voice · Tor**

Developed by **H34TB145T**

</div>

> [!WARNING]
> Airwire is early alpha and has not received an independent security audit.
> It improves privacy, but cannot guarantee anonymity against compromised
> devices, malware, traffic analysis, or a global network observer.

## Quick start

### 1. Install

**macOS / Linux**

```sh
curl -fsSL https://raw.githubusercontent.com/H34TB145T/airwire/main/install.sh | sh
```

**Windows PowerShell**

```powershell
irm https://raw.githubusercontent.com/H34TB145T/airwire/main/install.ps1 | iex
```

### 2. Open a room

| Route | Host command | Best for |
|---|---|---|
| Local | `airwire -s` | Same device or custom relay |
| Cloudflare | `airwire -s -f` | Fast temporary internet rooms |
| Tor | `airwire -s -t` | Onion-routed rooms |

Limit the room when needed:

```sh
airwire -s -t --max-users 3
```

### 3. Share the invitation

Airwire prints one compact command. The guest runs it exactly as shown:

```sh
# Cloudflare
airwire aB3xY9@paper-river

# Tor — Tor starts automatically
airwire aB3xY9@ONION_SERVICE_ID
```

One guest creates a private chat. More guests automatically create a group.

## What you get

| | |
|---|---|
| **No accounts** | Start with a random six-character room code |
| **Encrypted rooms** | Messages, files, media, and voice use end-to-end encryption |
| **Group chat** | Every admitted participant receives room traffic |
| **File drop** | Use `/send` or drag a file into the terminal |
| **Live voice** | Start and stop an encrypted call inside the TUI |
| **Flexible routing** | Use Tor, Cloudflare, Pinggy, or your own relay |
| **Cross-platform** | Windows x86-64/ARM64, macOS Intel/Apple Silicon, Linux x86-64/ARM64 |
| **No chat history** | Airwire and its relay do not save conversation logs |

## Inside the room

| Command | Action |
|---|---|
| `/send <path>` | Send a file or media |
| `/call` | Start voice |
| `/hangup` | Stop voice |
| `/clear` | Clear the screen |
| `/quit` | Leave the room |

Received files are saved to `Downloads/airwire` by default.

## Update

After the first installation, future upgrades are one command:

```sh
airwire --update
```

Airwire replaces the exact executable currently being used. On Windows, it
waits for `airwire.exe` to close before replacing it and keeps that installation
first in `PATH`.

## How it works

```text
host creates room
       │
       ▼
guest joins with CODE@HOST
       │
       ▼
SPAKE2 authenticated key exchange
       │
       ▼
XChaCha20-Poly1305 encrypted room traffic
       │
       ▼
local relay / Cloudflare / Tor / custom relay
```

The relay forwards opaque packets and keeps room state only in memory.

<details>
<summary><strong>Routing options</strong></summary>

### Cloudflare

Install
[cloudflared](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/),
then run:

```sh
airwire -s -f
```

Airwire starts a temporary Quick Tunnel and prints an invitation such as
`airwire aB3xY9@paper-river`.

### Tor

```sh
airwire -s -t
```

Airwire finds Tor, starts an isolated session, creates a temporary v3 onion
service, and removes its temporary state when the room closes. Windows installs
the Tor Expert Bundle automatically; supported macOS/Linux systems offer
interactive setup when Tor is missing.

Tor service IDs remain 56 characters because shortening them would require a
separate lookup service.

### Existing SOCKS5 proxy

```sh
airwire -c aB3xY9 -r ws://ADDRESS.onion/ws -t 127.0.0.1:9050
```

Tor Browser commonly uses port `9150`; the standalone Tor daemon commonly uses
`9050`.

### Shared relay

```sh
airwire relay --listen 127.0.0.1:8787
```

Expose `/ws` through HTTPS, then:

```sh
export AIRWIRE_RELAY=wss://relay.example/ws
airwire -s
airwire -c aB3xY9
```

Pinggy and similar tunnels can expose `http://127.0.0.1:8787`; use their public
address as `wss://HOST/ws`.

</details>

<details>
<summary><strong>Security notes</strong></summary>

- SPAKE2 authenticates the host-to-guest key exchange.
- XChaCha20-Poly1305 protects chat, files, media, and voice.
- Files stream in encrypted chunks and are verified with SHA-256.
- The relay sees connection metadata, timing, direction, approximate sizes,
  room membership, and a two-character rendezvous prefix.
- The remaining four room-code characters stay inside the SPAKE2 exchange.
- The bundled relay limits each active room to 60 join attempts per minute.
- Every room participant receives the group key and can read group traffic.
- Display names are temporary labels, not verified identities.

Six-character codes are convenient, not high-entropy passwords. Share
invitations privately, limit guests, and close rooms when finished.

</details>

<details>
<summary><strong>Installation details</strong></summary>

| Platform | Default executable |
|---|---|
| macOS / Linux | `~/.local/bin/airwire` |
| Windows | `%LOCALAPPDATA%\Airwire\bin\airwire.exe` |

Installers select the correct release, verify its SHA-256 checksum, and update
the current user's `PATH`.

Useful overrides:

| Variable | Purpose |
|---|---|
| `AIRWIRE_VERSION` | Install a specific tag, such as `v0.2.3` |
| `AIRWIRE_INSTALL_DIR` | Change the executable directory |
| `AIRWIRE_TOR_DIR` | Change the Windows Tor bundle directory |
| `AIRWIRE_TOR_BINARY` | Use a specific Tor executable |
| `AIRWIRE_RELAY` | Set the default WebSocket relay |

</details>

<details>
<summary><strong>Build and development</strong></summary>

Build with a current stable Rust toolchain:

```sh
cargo build --release
./target/release/airwire --help
```

Linux voice support normally requires the ALSA development package
(`libasound2-dev` on Debian/Ubuntu). Build without audio for a headless relay:

```sh
cargo build --release --no-default-features
```

Run the project checks:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

Pushing a `v*` tag builds checked archives for all supported platforms and
publishes them as a GitHub Release.

</details>

---

<div align="center">

**Open a room. Share one line. Leave no chat log.**

</div>
