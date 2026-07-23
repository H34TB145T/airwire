# airwire

`airwire` is a cross-platform, terminal-only room for anonymous text, group
chat, files, media, and live voice. It is written in Rust and uses an
OpenCode-inspired black/gray TUI.

Developed by **H34TB145T**.

> Status: early alpha. The cryptographic design has not received an independent
> security audit. Do not treat anonymity as guaranteed against a global network
> observer, a compromised endpoint, traffic analysis, or malware.

## What works

- One host creates a random six-character room code.
- One guest makes a private conversation; additional guests automatically make
  it a group room.
- The host can cap guests with `--max-users`.
- Messages and attachment contents are end-to-end encrypted.
- `/send <path>` and terminal drag-and-drop send any file or media.
- `/call` and `/hangup` stream encrypted low-latency voice through the room.
- A bundled blind relay can run locally, behind TLS, behind Cloudflared/Pinggy,
  or as a Tor onion service.
- Linux, macOS, and Windows are supported by the Rust network/TUI/audio stack.

## Build

Install a current stable Rust toolchain, then:

```sh
cargo build --release
./target/release/airwire --help
```

Voice is enabled by default. On Linux, the CPAL backend normally requires the
ALSA development package (`libasound2-dev` on Debian/Ubuntu). A headless relay
can be built without audio:

```sh
cargo build --release --no-default-features
```

## Install

Release installers select the correct binary for macOS, Linux, or Windows,
verify its SHA-256 checksum, install it for the current user, and add Airwire to
`PATH` when necessary.

macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/H34TB145T/airwire/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/H34TB145T/airwire/main/install.ps1 | iex
```

The default locations are `~/.local/bin/airwire` on macOS/Linux and
`%LOCALAPPDATA%\Airwire\bin\airwire.exe` on Windows. Set
`AIRWIRE_INSTALL_DIR` before running an installer to override the location.
The Windows installer also downloads Tor Project's checksum-verified Expert
Bundle to `%LOCALAPPDATA%\Airwire\tor-expert`, detects an existing Tor
installation when possible, and saves its exact path in
`AIRWIRE_TOR_BINARY`. Set `AIRWIRE_TOR_DIR` to override the bundled Tor
location.

Installers use the latest GitHub release by default. Set `AIRWIRE_VERSION` to a
tag such as `v0.2.0` to install a specific release.

## Use

The host command automatically starts a local relay:

```sh
airwire --start
airwire --start --max-users 4
```

Another terminal on the same machine can join with:

```sh
airwire --connect aB3xY9
```

For an internet room with no account, install
[cloudflared](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/)
and run:

```sh
airwire --start --cloudflared
```

The footer prints the exact `AIRWIRE_RELAY=... airwire --connect CODE` command
to share. Cloudflare Quick Tunnel URLs are temporary. The room code alone is
enough only when everyone uses the same configured relay; code-to-endpoint
discovery necessarily requires shared infrastructure.

For an internet room carried entirely through Tor:

```sh
airwire --start --tor-proxy
```

Airwire finds Tor, offers to install it interactively on supported macOS/Linux
systems when it is missing, starts an isolated temporary Tor client, creates a
fresh v3 onion service, and prints the complete guest command. No `torrc`
editing or background Tor service is required.

Inside the UI:

```text
/send ./photo.png
/call
/hangup
/clear
/quit
```

Dropping a file from Finder, Explorer, or a desktop file manager into the input
line also sends it. Received files go to the user's Downloads/airwire directory
unless `--downloads` is supplied.

## Run a shared relay

```sh
airwire relay --listen 127.0.0.1:8787
```

Put `/ws` behind a normal HTTPS reverse proxy, then hosts and guests use the
same URL:

```sh
export AIRWIRE_RELAY=wss://relay.example/ws
airwire --start
airwire --connect aB3xY9
```

The relay stores no history and sees opaque application packets. It sees only
the first two characters of a room code for rendezvous, plus connection IPs
(unless hidden by a proxy), timing, membership, direction, and approximate
packet sizes. The remaining four characters stay inside the SPAKE2 exchange.

### Tor

Automatic mode is intended for quick private rooms:

```sh
airwire --start --tor-proxy
```

It launches a separate Tor process with temporary state and onion keys. Airwire
stops that process and removes its temporary data when the room closes. The
host receives a guest command shaped like:

```sh
airwire --connect aB3xY9 --relay ws://ADDRESS.onion/ws --tor-proxy
```

The guest's bare `--tor-proxy` also starts an isolated Tor client
automatically and retries while a fresh onion service propagates. Initial Tor
connections can take around a minute. If Tor is outside `PATH`, use
`--tor-binary /path/to/tor` or set `AIRWIRE_TOR_BINARY`. On Windows, Airwire
also discovers the Expert Bundle installed by `install.ps1` and common Tor
Browser locations.

Advanced users may keep routing through an already-running SOCKS5 proxy by
supplying its address explicitly:

```sh
airwire --connect aB3xY9 --relay ws://ADDRESS.onion/ws \
  --tor-proxy 127.0.0.1:9050
```

Using Tor generally improves source-address privacy but is slower, especially
for voice and large files. Tor Browser's SOCKS port is commonly `9150`; the
standalone Tor daemon commonly uses `9050`.

### Pinggy or another tunnel

Run the local relay, expose `http://127.0.0.1:8787` with the tunnel provider,
convert its public HTTPS address to `wss://.../ws`, and supply that value through
`AIRWIRE_RELAY`. The tunnel carries encrypted Airwire packets but remains able
to observe traffic metadata.

## Security design

```text
six-character code
        │
        ▼
SPAKE2 authenticated key exchange (host ↔ each guest)
        │
        ▼
high-entropy room key, delivered over each pairwise encrypted channel
        │
        ▼
XChaCha20-Poly1305 authenticated encryption for chat/files/voice
        │
        ▼
blind WebSocket relay / Cloudflared / Tor / Pinggy
```

SPAKE2 prevents a passive relay from using captured handshakes for an offline
guess of the four code characters that are not used for rendezvous. The random
code is still short, and the relay learns its two-character rendezvous prefix,
so an attacker can try active online guesses while the room exists. Use guest
limits, share codes privately, close rooms when finished, and operate a
rate-limited relay for hostile environments. The bundled relay limits each
active room to 60 join attempts per minute.

Each room participant receives the group key. Consequently, any admitted
participant can read group traffic and can technically impersonate another
display name. Display names are deliberately ephemeral and are not identities.
Files are streamed in encrypted chunks and verified with SHA-256 before the
`.airwire-part` file is renamed.

Airwire does not write chat logs. Received attachments are intentionally written
to disk. The relay is memory-only and drops room state when the host disconnects.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

Pushing a `v*` tag runs the release workflow. It builds and publishes checked
archives for Linux x86-64/ARM64, macOS Intel/Apple Silicon, and Windows
x86-64/ARM64. The installer scripts download those release assets.
