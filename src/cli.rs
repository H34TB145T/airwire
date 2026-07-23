use std::{net::SocketAddr, path::PathBuf};

use clap::{Parser, Subcommand};

pub const DEFAULT_RELAY: &str = "ws://127.0.0.1:8787/ws";

#[derive(Debug, Parser)]
#[command(
    name = "airwire",
    version,
    author = "H34TB145T",
    about = "Anonymous end-to-end encrypted terminal chat",
    long_about = None,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Start and host a new room.
    #[arg(long, conflicts_with_all = ["connect", "command"])]
    pub start: bool,

    /// Connect to an existing six-character room code.
    #[arg(long, value_name = "CODE", conflicts_with_all = ["start", "command"])]
    pub connect: Option<String>,

    /// Maximum number of guests accepted by a hosted room.
    #[arg(long, default_value_t = 16, requires = "start")]
    pub max_users: usize,

    /// Relay WebSocket URL. AIRWIRE_RELAY may also be used.
    #[arg(long, env = "AIRWIRE_RELAY", default_value = DEFAULT_RELAY)]
    pub relay: String,

    /// Route the relay TCP connection through this SOCKS5 proxy.
    #[arg(long, env = "AIRWIRE_TOR_PROXY", value_name = "HOST:PORT")]
    pub tor_proxy: Option<String>,

    /// Start an ephemeral Cloudflare Quick Tunnel for the embedded relay.
    #[arg(long, requires = "start")]
    pub cloudflared: bool,

    /// Ephemeral display name shown only inside this room.
    #[arg(long, default_value = "anonymous")]
    pub name: String,

    /// Directory for received files.
    #[arg(long, value_name = "DIR")]
    pub downloads: Option<PathBuf>,

    /// Disable microphone and speaker access.
    #[arg(long)]
    pub no_voice: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run a rendezvous/packet relay service.
    Relay {
        /// Address on which the HTTP/WebSocket relay listens.
        #[arg(long, default_value = "127.0.0.1:8787")]
        listen: SocketAddr,
    },
}
