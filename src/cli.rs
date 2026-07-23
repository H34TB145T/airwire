use std::{net::SocketAddr, path::PathBuf};

use clap::{Parser, Subcommand};

pub const DEFAULT_RELAY: &str = "ws://127.0.0.1:8787/ws";
pub const MANAGED_TOR: &str = "auto";

#[derive(Debug, Parser)]
#[command(
    name = "airwire",
    version,
    author = "H34TB145T",
    about = "Anonymous end-to-end encrypted terminal chat",
    long_about = None,
    disable_help_subcommand = true,
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    /// Start and host a new room.
    #[arg(long, conflicts_with = "connect")]
    pub start: bool,

    /// Connect to an existing six-character room code.
    #[arg(long, value_name = "CODE", conflicts_with = "start")]
    pub connect: Option<String>,

    /// Maximum number of guests accepted by a hosted room.
    #[arg(long, default_value_t = 16, requires = "start")]
    pub max_users: usize,

    /// Relay WebSocket URL. AIRWIRE_RELAY may also be used.
    #[arg(long, env = "AIRWIRE_RELAY", default_value = DEFAULT_RELAY)]
    pub relay: String,

    /// Automatically use Tor, or route through an explicit SOCKS5 HOST:PORT.
    #[arg(
        long,
        env = "AIRWIRE_TOR_PROXY",
        value_name = "HOST:PORT",
        num_args = 0..=1,
        default_missing_value = MANAGED_TOR,
        conflicts_with = "cloudflared"
    )]
    pub tor_proxy: Option<String>,

    /// Tor executable used by automatic Tor mode.
    #[arg(
        long,
        env = "AIRWIRE_TOR_BINARY",
        value_name = "PATH",
        requires = "tor_proxy"
    )]
    pub tor_binary: Option<PathBuf>,

    /// Start an ephemeral Cloudflare Quick Tunnel for the embedded relay.
    #[arg(long, requires = "start", conflicts_with = "tor_proxy")]
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

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;

    #[test]
    fn bare_tor_proxy_selects_managed_mode() {
        let cli = Cli::try_parse_from(["airwire", "--start", "--tor-proxy"]).unwrap();
        assert_eq!(cli.tor_proxy.as_deref(), Some(MANAGED_TOR));
    }

    #[test]
    fn explicit_tor_proxy_is_preserved() {
        let cli = Cli::try_parse_from([
            "airwire",
            "--connect",
            "aB3xY9",
            "--tor-proxy",
            "127.0.0.1:9150",
        ])
        .unwrap();
        assert_eq!(cli.tor_proxy.as_deref(), Some("127.0.0.1:9150"));
    }

    #[test]
    fn command_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
