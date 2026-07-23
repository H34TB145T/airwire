mod audio;
mod cli;
mod client;
mod crypto;
mod protocol;
mod relay;
mod tui;
mod tunnel;

use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Result, bail};
use clap::{CommandFactory, Parser};
use directories::UserDirs;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use crate::{
    cli::{Cli, Command, DEFAULT_RELAY},
    client::{ClientConfig, NetCommand, Role, UiEvent},
    protocol::{generate_code, validate_code},
    tui::ViewConfig,
};

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    let cli = Cli::parse();

    if let Some(Command::Relay { listen }) = cli.command {
        println!("airwire relay listening on {listen}");
        println!("developed by H34TB145T");
        return relay::serve(listen).await;
    }

    if !cli.start && cli.connect.is_none() {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    }
    if cli.max_users == 0 {
        bail!("--max-users must be at least 1");
    }

    let code = match &cli.connect {
        Some(code) => {
            if !validate_code(code) {
                bail!("room codes must contain exactly six ASCII letters or digits");
            }
            code.clone()
        }
        None => generate_code(),
    };
    let is_host = cli.start;
    let role = if is_host {
        Role::Host {
            max_guests: cli.max_users,
        }
    } else {
        Role::Guest
    };

    let mut embedded_relay = None;
    if is_host && cli.relay == DEFAULT_RELAY {
        let listen: SocketAddr = "127.0.0.1:8787".parse().expect("static address");
        match relay::bind_embedded(listen).await {
            Ok(listener) => {
                embedded_relay = Some(tokio::spawn(async move {
                    if let Err(error) = relay::serve_listener(listener).await {
                        tracing::error!("embedded relay stopped: {error:#}");
                    }
                }));
            }
            Err(error) => {
                tracing::warn!("using an existing local relay: {error:#}");
            }
        }
    }

    let mut cloudflared = None;
    let relay_display = if cli.cloudflared {
        if cli.relay != DEFAULT_RELAY {
            bail!("--cloudflared manages the default embedded relay; omit --relay");
        }
        let tunnel = tunnel::CloudflaredTunnel::start("http://127.0.0.1:8787").await?;
        let share = format!(
            "share: AIRWIRE_RELAY={} airwire --connect {}",
            tunnel.public_relay_url, code
        );
        cloudflared = Some(tunnel);
        share
    } else if is_host && cli.relay == DEFAULT_RELAY {
        format!("local relay · airwire --connect {code}")
    } else {
        cli.relay.clone()
    };

    let downloads = cli.downloads.unwrap_or_else(default_downloads);
    let client_config = ClientConfig {
        role,
        code: code.clone(),
        relay_url: cli.relay.clone(),
        tor_proxy: cli.tor_proxy.clone(),
        alias: cli.name,
        downloads,
    };
    let (command_tx, command_rx) = mpsc::channel::<NetCommand>(128);
    let (event_tx, event_rx) = mpsc::channel::<UiEvent>(256);
    let network = tokio::spawn(async move {
        if let Err(error) = client::run(client_config, command_rx, event_tx.clone()).await {
            let _ = event_tx.try_send(UiEvent::Closed(format!("{error:#}")));
        }
    });

    let result = tui::run(
        ViewConfig {
            code,
            relay_display,
            is_host,
            voice_enabled: !cli.no_voice,
        },
        command_tx,
        event_rx,
    )
    .await;

    network.abort();
    if let Some(task) = embedded_relay {
        task.abort();
    }
    drop(cloudflared);
    result
}

fn default_downloads() -> PathBuf {
    UserDirs::new()
        .and_then(|directories| directories.download_dir().map(PathBuf::from))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join("airwire")
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
