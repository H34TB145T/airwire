use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use crate::{protocol::validate_code, tor::is_valid_v3_onion_hostname};

pub const DEFAULT_RELAY: &str = "ws://127.0.0.1:8787/ws";
pub const MANAGED_TOR: &str = "auto";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactInvite {
    pub code: String,
    pub relay_url: String,
    pub managed_tor: bool,
}

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
    /// Compact invitation in CODE@HOST form.
    #[arg(
        value_name = "CODE@HOST",
        conflicts_with_all = ["start", "connect", "cloudflared"]
    )]
    pub invite: Option<String>,

    /// Start and host a new room.
    #[arg(short = 's', long, conflicts_with = "connect")]
    pub start: bool,

    /// Connect to an existing six-character room code.
    #[arg(short = 'c', long, value_name = "CODE", conflicts_with = "start")]
    pub connect: Option<String>,

    /// Maximum number of guests accepted by a hosted room.
    #[arg(long, default_value_t = 16, requires = "start")]
    pub max_users: usize,

    /// Relay WebSocket URL. AIRWIRE_RELAY may also be used.
    #[arg(short = 'r', long, env = "AIRWIRE_RELAY", default_value = DEFAULT_RELAY)]
    pub relay: String,

    /// Automatically use Tor, or route through an explicit SOCKS5 HOST:PORT.
    #[arg(
        short = 't',
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
    #[arg(short = 'f', long, requires = "start", conflicts_with = "tor_proxy")]
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

pub fn parse_compact_invite(value: &str) -> Result<CompactInvite> {
    let (code, endpoint) = value
        .trim()
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("compact invitations must use CODE@HOST"))?;
    if !validate_code(code) {
        bail!("room codes must contain exactly six ASCII letters or digits");
    }
    if endpoint.is_empty() || endpoint.chars().any(char::is_whitespace) {
        bail!("compact invitation host is missing or invalid");
    }

    let relay_url = if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
        endpoint.to_owned()
    } else if endpoint.ends_with(".onion") {
        format!("ws://{endpoint}/ws")
    } else if is_valid_v3_onion_hostname(&format!("{endpoint}.onion")) {
        format!("ws://{endpoint}.onion/ws")
    } else if !endpoint.contains('.') && !endpoint.contains(':') {
        format!("wss://{endpoint}.trycloudflare.com/ws")
    } else {
        format!("wss://{endpoint}/ws")
    };
    let mut parsed = url::Url::parse(&relay_url)
        .map_err(|error| anyhow::anyhow!("invalid compact invitation host: {error}"))?;
    if !matches!(parsed.scheme(), "ws" | "wss")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("compact invitations require a plain WebSocket relay host");
    }
    if parsed.path() == "/" {
        parsed.set_path("/ws");
    } else if parsed.path() != "/ws" {
        bail!("compact invitation relay paths must be /ws");
    }

    let hostname = parsed.host_str().expect("host was checked above");
    let managed_tor = hostname.ends_with(".onion");
    if managed_tor && (parsed.scheme() != "ws" || !is_valid_v3_onion_hostname(hostname)) {
        bail!("compact Tor invitations require a valid v3 onion hostname");
    }

    Ok(CompactInvite {
        code: code.to_owned(),
        relay_url: parsed.to_string(),
        managed_tor,
    })
}

pub fn compact_invitation_command(code: &str, relay_url: &str) -> Result<String> {
    let parsed = url::Url::parse(relay_url)
        .map_err(|error| anyhow::anyhow!("cannot shorten relay URL: {error}"))?;
    let hostname = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("cannot shorten a relay URL without a hostname"))?;
    let is_onion = hostname.ends_with(".onion");
    if (is_onion && parsed.scheme() != "ws")
        || (!is_onion && parsed.scheme() != "wss")
        || parsed.path() != "/ws"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("relay URL cannot be represented as a compact invitation");
    }
    let compact_hostname = hostname
        .strip_suffix(".onion")
        .or_else(|| hostname.strip_suffix(".trycloudflare.com"))
        .unwrap_or(hostname);
    let endpoint = match parsed.port() {
        Some(port) => format!("{compact_hostname}:{port}"),
        None => compact_hostname.to_owned(),
    };
    Ok(format!("airwire {code}@{endpoint}"))
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
    fn parses_compact_cloudflare_and_tor_invitations() {
        let cli = Cli::try_parse_from(["airwire", "aB3xY9@paper-river"]).unwrap();
        assert_eq!(cli.invite.as_deref(), Some("aB3xY9@paper-river"));

        let cloudflare = parse_compact_invite("aB3xY9@paper-river").unwrap();
        assert_eq!(
            cloudflare,
            CompactInvite {
                code: "aB3xY9".into(),
                relay_url: "wss://paper-river.trycloudflare.com/ws".into(),
                managed_tor: false,
            }
        );

        let onion = format!("{}.onion", "a".repeat(56));
        assert_eq!(
            compact_invitation_command("aB3xY9", &cloudflare.relay_url).unwrap(),
            "airwire aB3xY9@paper-river"
        );

        let service_id = onion.trim_end_matches(".onion");
        let tor = parse_compact_invite(&format!("aB3xY9@{service_id}")).unwrap();
        assert_eq!(tor.relay_url, format!("ws://{onion}/ws"));
        assert!(tor.managed_tor);
        assert_eq!(
            compact_invitation_command("aB3xY9", &tor.relay_url).unwrap(),
            format!("airwire aB3xY9@{service_id}")
        );
    }

    #[test]
    fn rejects_invalid_compact_invitations() {
        assert!(parse_compact_invite("short@relay.example").is_err());
        assert!(parse_compact_invite("aB3xY9@short.onion").is_err());
        assert!(parse_compact_invite("aB3xY9@relay.example/other").is_err());
    }

    #[test]
    fn short_flags_are_available() {
        let cli = Cli::try_parse_from(["airwire", "-s", "-t"]).unwrap();
        assert!(cli.start);
        assert_eq!(cli.tor_proxy.as_deref(), Some(MANAGED_TOR));

        let cli = Cli::try_parse_from(["airwire", "-c", "aB3xY9", "-r", "wss://relay/ws"]).unwrap();
        assert_eq!(cli.connect.as_deref(), Some("aB3xY9"));
        assert_eq!(cli.relay, "wss://relay/ws");
    }

    #[test]
    fn command_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
