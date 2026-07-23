use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
};
use tokio_socks::tcp::Socks5Stream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, client_async_tls_with_config, connect_async,
    tungstenite::Message,
};
use zeroize::Zeroize;

use crate::{
    crypto,
    protocol::{
        AppFrame, FILE_CHUNK_SIZE, MAX_FILE_SIZE, PROTOCOL_VERSION, PeerId, RelayCommand,
        RelayEvent, Welcome, WirePacket, decode, encode, rendezvous_id,
    },
};

const MAX_CHAT_CHARS: usize = 8_000;
const MAX_ALIAS_CHARS: usize = 32;
const TOR_CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(20);
const TOR_CONNECT_RETRY_WINDOW: Duration = Duration::from_secs(90);

#[derive(Debug, Clone)]
pub enum Role {
    Host { max_guests: usize },
    Guest,
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub role: Role,
    pub code: String,
    pub relay_url: String,
    pub tor_proxy: Option<String>,
    pub retry_connect: bool,
    pub alias: String,
    pub downloads: PathBuf,
}

#[derive(Debug)]
#[cfg_attr(not(feature = "voice"), allow(dead_code))]
pub enum NetCommand {
    Chat(String),
    SendFile(PathBuf),
    StartCall,
    StopCall,
    VoicePcm { sample_rate: u32, samples: Vec<i16> },
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum UiEvent {
    Ready {
        secure: bool,
        detail: String,
    },
    Chat {
        alias: String,
        text: String,
        mine: bool,
    },
    Status(String),
    Error(String),
    PeerCount(usize),
    FileProgress {
        label: String,
        transferred: u64,
        total: u64,
    },
    FileReceived {
        from: String,
        path: PathBuf,
    },
    VoiceStarted(String),
    VoiceStopped(String),
    VoicePcm {
        sample_rate: u32,
        samples: Vec<i16>,
    },
    Closed(String),
}

struct ReceivingFile {
    from: String,
    final_path: PathBuf,
    partial_path: PathBuf,
    file: File,
    hasher: Sha256,
    expected_size: u64,
    received: u64,
    next_sequence: u32,
}

enum SecurityState {
    Host {
        room_key: [u8; 32],
        handshakes: HashMap<PeerId, Spake2<Ed25519Group>>,
        aliases: HashMap<PeerId, String>,
    },
    Guest {
        room_key: Option<[u8; 32]>,
        host: Option<PeerId>,
        pair_key: Option<[u8; 32]>,
    },
}

impl Drop for SecurityState {
    fn drop(&mut self) {
        match self {
            Self::Host { room_key, .. } => room_key.zeroize(),
            Self::Guest {
                room_key, pair_key, ..
            } => {
                room_key.zeroize();
                pair_key.zeroize();
            }
        }
    }
}

pub async fn run(
    config: ClientConfig,
    mut commands: mpsc::Receiver<NetCommand>,
    events: mpsc::Sender<UiEvent>,
) -> Result<()> {
    fs::create_dir_all(&config.downloads)
        .await
        .with_context(|| format!("cannot create {}", config.downloads.display()))?;

    let mut socket = connect_with_retry(
        &config.relay_url,
        config.tor_proxy.as_deref(),
        config.retry_connect,
        &events,
    )
    .await?;
    let registration = match config.role {
        Role::Host { max_guests } => RelayCommand::Host {
            version: PROTOCOL_VERSION,
            rendezvous: rendezvous_id(&config.code),
            max_guests,
        },
        Role::Guest => RelayCommand::Join {
            version: PROTOCOL_VERSION,
            rendezvous: rendezvous_id(&config.code),
        },
    };
    socket
        .send(Message::Binary(encode(&registration)?.into()))
        .await
        .context("relay registration failed")?;

    let first = next_relay_event(&mut socket).await?;
    let (self_id, is_host) = match first {
        RelayEvent::Registered { id, is_host } => (id, is_host),
        RelayEvent::Error { message } => bail!("{message}"),
        _ => bail!("relay sent an invalid registration response"),
    };
    if is_host != matches!(config.role, Role::Host { .. }) {
        bail!("relay returned the wrong room role");
    }

    let alias: String = config.alias.chars().take(MAX_ALIAS_CHARS).collect();
    let alias = if alias.trim().is_empty() {
        "anonymous".to_owned()
    } else {
        alias
    };
    let mut security = if is_host {
        SecurityState::Host {
            room_key: crypto::random_key(),
            handshakes: HashMap::new(),
            aliases: HashMap::new(),
        }
    } else {
        SecurityState::Guest {
            room_key: None,
            host: None,
            pair_key: None,
        }
    };
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (relay_tx, mut relay_rx) = mpsc::channel::<RelayCommand>(64);
    let writer = tokio::spawn(async move {
        while let Some(command) = relay_rx.recv().await {
            let bytes = encode(&command)?;
            ws_tx
                .send(Message::Binary(bytes.into()))
                .await
                .context("relay connection closed")?;
        }
        Result::<()>::Ok(())
    });

    let _ = events.try_send(UiEvent::Ready {
        secure: is_host,
        detail: if is_host {
            "room registered; waiting for encrypted peers".into()
        } else {
            "relay connected; authenticating the room code".into()
        },
    });
    let _ = events.try_send(UiEvent::PeerCount(1));

    let (frame_tx, mut frame_rx) = mpsc::channel::<AppFrame>(16);
    let mut receiving = HashMap::<[u8; 16], ReceivingFile>::new();
    let mut peer_count = 1_usize;

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                match command {
                    NetCommand::Chat(text) => {
                        let text: String = text.chars().take(MAX_CHAT_CHARS).collect();
                        if !text.trim().is_empty() {
                            let frame = AppFrame::Chat {
                                alias: alias.clone(),
                                text: text.clone(),
                                sent_at_ms: unix_time_ms(),
                            };
                            if broadcast_frame(&security, self_id, frame, &relay_tx).await? {
                                let _ = events.try_send(UiEvent::Chat {
                                    alias: alias.clone(),
                                    text,
                                    mine: true,
                                });
                            } else {
                                let _ = events.try_send(UiEvent::Error("secure room handshake is not complete".into()));
                            }
                        }
                    }
                    NetCommand::SendFile(path) => {
                        if room_key(&security).is_none() {
                            let _ = events.try_send(UiEvent::Error("secure room handshake is not complete".into()));
                            continue;
                        }
                        let frames = frame_tx.clone();
                        let updates = events.clone();
                        let sender_alias = alias.clone();
                        tokio::spawn(async move {
                            if let Err(error) = stream_file(path, sender_alias, frames, updates.clone()).await {
                                let _ = updates.try_send(UiEvent::Error(format!("file send failed: {error:#}")));
                            }
                        });
                    }
                    NetCommand::StartCall => {
                        if broadcast_frame(
                            &security,
                            self_id,
                            AppFrame::VoiceStart { alias: alias.clone() },
                            &relay_tx,
                        ).await? {
                            let _ = events.try_send(UiEvent::VoiceStarted(format!("you ({alias})")));
                        } else {
                            let _ = events.try_send(UiEvent::Error("secure room handshake is not complete".into()));
                        }
                    }
                    NetCommand::StopCall => {
                        if broadcast_frame(
                            &security,
                            self_id,
                            AppFrame::VoiceStop { alias: alias.clone() },
                            &relay_tx,
                        ).await? {
                            let _ = events.try_send(UiEvent::VoiceStopped(format!("you ({alias})")));
                        } else {
                            let _ = events.try_send(UiEvent::Error("secure room handshake is not complete".into()));
                        }
                    }
                    NetCommand::VoicePcm { sample_rate, samples } => {
                        let _ = broadcast_frame(
                            &security,
                            self_id,
                            AppFrame::VoicePcm { sample_rate, samples },
                            &relay_tx,
                        ).await?;
                    }
                    NetCommand::Shutdown => break,
                }
            }
            Some(frame) = frame_rx.recv() => {
                let _ = broadcast_frame(&security, self_id, frame, &relay_tx).await?;
            }
            message = ws_rx.next() => {
                let Some(message) = message else {
                    let _ = events.try_send(UiEvent::Closed("relay connection ended".into()));
                    break;
                };
                let bytes = match message {
                    Ok(Message::Binary(bytes)) => bytes,
                    Ok(Message::Close(_)) => {
                        let _ = events.try_send(UiEvent::Closed("relay closed the connection".into()));
                        break;
                    }
                    Ok(_) => continue,
                    Err(error) => return Err(error).context("relay read failed"),
                };
                let relay_event: RelayEvent = decode(&bytes).context("invalid relay event")?;
                match relay_event {
                    RelayEvent::PeerJoined { id } => {
                        if let SecurityState::Host { handshakes, .. } = &mut security {
                            let (state, message) = Spake2::<Ed25519Group>::start_symmetric(
                                &Password::new(config.code.as_bytes()),
                                &Identity::new(b"airwire-room-v1"),
                            );
                            handshakes.insert(id, state);
                            send_packet(
                                &relay_tx,
                                Some(id),
                                WirePacket::Spake { response: false, message },
                            ).await?;
                            peer_count += 1;
                            let _ = events.try_send(UiEvent::PeerCount(peer_count));
                        }
                    }
                    RelayEvent::PeerLeft { id } => {
                        if let SecurityState::Host { aliases, handshakes, .. } = &mut security {
                            peer_count = peer_count.saturating_sub(1).max(1);
                            let _ = events.try_send(UiEvent::PeerCount(peer_count));
                            handshakes.remove(&id);
                            if let Some(departed) = aliases.remove(&id) {
                                let frame = AppFrame::Left { alias: departed.clone() };
                                let _ = broadcast_frame(&security, self_id, frame, &relay_tx).await?;
                                let _ = events.try_send(UiEvent::Status(format!("{departed} left the room")));
                            }
                        }
                    }
                    RelayEvent::Data { from, payload } => {
                        let packet: WirePacket = match decode(&payload) {
                            Ok(packet) => packet,
                            Err(_) => continue,
                        };
                        handle_packet(
                            packet,
                            from,
                            self_id,
                            &config.code,
                            &alias,
                            &mut security,
                            &relay_tx,
                            &events,
                            &config.downloads,
                            &mut receiving,
                            &mut peer_count,
                        ).await?;
                    }
                    RelayEvent::RoomClosed => {
                        let _ = events.try_send(UiEvent::Closed("the host closed this room".into()));
                        break;
                    }
                    RelayEvent::Error { message } => {
                        let _ = events.try_send(UiEvent::Error(message));
                    }
                    RelayEvent::Registered { .. } => {}
                }
            }
        }
    }

    drop(relay_tx);
    writer.abort();
    for (_, receive) in receiving {
        drop(receive.file);
        let _ = fs::remove_file(receive.partial_path).await;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_packet(
    packet: WirePacket,
    from: PeerId,
    self_id: PeerId,
    code: &str,
    alias: &str,
    security: &mut SecurityState,
    relay_tx: &mpsc::Sender<RelayCommand>,
    events: &mpsc::Sender<UiEvent>,
    downloads: &Path,
    receiving: &mut HashMap<[u8; 16], ReceivingFile>,
    peer_count: &mut usize,
) -> Result<()> {
    match packet {
        WirePacket::Spake {
            response: false,
            message,
        } => {
            if let SecurityState::Guest {
                room_key,
                host,
                pair_key,
            } = security
            {
                if room_key.is_some() || host.is_some() {
                    return Ok(());
                }
                let (state, reply) = Spake2::<Ed25519Group>::start_symmetric(
                    &Password::new(code.as_bytes()),
                    &Identity::new(b"airwire-room-v1"),
                );
                let shared = state
                    .finish(&message)
                    .map_err(|_| anyhow!("room authentication failed"))?;
                let normalized = crypto::normalize_spake_key(&shared);
                *pair_key = Some(*normalized);
                *host = Some(from);
                send_packet(
                    relay_tx,
                    Some(from),
                    WirePacket::Spake {
                        response: true,
                        message: reply,
                    },
                )
                .await?;
            }
        }
        WirePacket::Spake {
            response: true,
            message,
        } => {
            if let SecurityState::Host {
                room_key,
                handshakes,
                aliases,
            } = security
            {
                let Some(state) = handshakes.remove(&from) else {
                    return Ok(());
                };
                let shared = state
                    .finish(&message)
                    .map_err(|_| anyhow!("peer room authentication failed"))?;
                let pair_key = crypto::normalize_spake_key(&shared);
                let welcome = Welcome {
                    room_key: *room_key,
                    host_alias: alias.to_owned(),
                    peer_count: aliases.len() + 2,
                };
                let sealed = crypto::seal_pair(&pair_key, &welcome)?;
                send_packet(relay_tx, Some(from), WirePacket::PairSealed(sealed)).await?;
            }
        }
        WirePacket::PairSealed(sealed) => {
            if let SecurityState::Guest {
                room_key,
                host,
                pair_key,
            } = security
            {
                if *host != Some(from) || room_key.is_some() {
                    return Ok(());
                }
                let mut key = pair_key
                    .take()
                    .ok_or_else(|| anyhow!("missing authenticated pair key"))?;
                let mut welcome: Welcome = crypto::open_pair(&key, &sealed)?;
                key.zeroize();
                *room_key = Some(welcome.room_key);
                let _ = events.try_send(UiEvent::Ready {
                    secure: true,
                    detail: format!("end-to-end encrypted with host {}", welcome.host_alias),
                });
                *peer_count = welcome.peer_count;
                let _ = events.try_send(UiEvent::PeerCount(*peer_count));
                welcome.room_key.zeroize();
                let _ = broadcast_frame(
                    security,
                    self_id,
                    AppFrame::Joined {
                        alias: alias.to_owned(),
                    },
                    relay_tx,
                )
                .await?;
            }
        }
        WirePacket::GroupSealed(sealed) => {
            let Some(key) = room_key(security) else {
                return Ok(());
            };
            let frame: AppFrame = match crypto::open_group(key, &from.0, &sealed) {
                Ok(frame) => frame,
                Err(_) => {
                    let _ = events.try_send(UiEvent::Error(format!(
                        "discarded an unauthenticated packet from {}",
                        from.short()
                    )));
                    return Ok(());
                }
            };
            if let SecurityState::Host { aliases, .. } = security
                && let AppFrame::Joined { alias } = &frame
            {
                aliases.insert(from, clean_alias(alias.clone()));
            }
            if matches!(security, SecurityState::Guest { .. }) {
                match &frame {
                    AppFrame::Joined { .. } => {
                        *peer_count += 1;
                        let _ = events.try_send(UiEvent::PeerCount(*peer_count));
                    }
                    AppFrame::Left { .. } => {
                        *peer_count = peer_count.saturating_sub(1).max(1);
                        let _ = events.try_send(UiEvent::PeerCount(*peer_count));
                    }
                    _ => {}
                }
            }
            handle_app_frame(frame, events, downloads, receiving).await?;
        }
    }
    Ok(())
}

async fn handle_app_frame(
    frame: AppFrame,
    events: &mpsc::Sender<UiEvent>,
    downloads: &Path,
    receiving: &mut HashMap<[u8; 16], ReceivingFile>,
) -> Result<()> {
    match frame {
        AppFrame::Chat { alias, text, .. } => {
            let alias = clean_alias(alias);
            let text: String = text.chars().take(MAX_CHAT_CHARS).collect();
            let _ = events.try_send(UiEvent::Chat {
                alias,
                text,
                mine: false,
            });
        }
        AppFrame::Joined { alias } => {
            let alias = clean_alias(alias);
            let _ = events.try_send(UiEvent::Status(format!("{alias} joined securely")));
        }
        AppFrame::Left { alias } => {
            let alias = clean_alias(alias);
            let _ = events.try_send(UiEvent::Status(format!("{alias} left the room")));
        }
        AppFrame::FileOffer {
            id,
            alias,
            name,
            size,
            ..
        } => {
            if receiving.len() >= 16 || receiving.contains_key(&id) {
                let _ = events.try_send(UiEvent::Error(
                    "rejected attachment: too many active or duplicate transfers".into(),
                ));
                return Ok(());
            }
            if size > MAX_FILE_SIZE {
                let _ = events.try_send(UiEvent::Error(format!(
                    "rejected {name}: file exceeds the {} MiB limit",
                    MAX_FILE_SIZE / 1024 / 1024
                )));
                return Ok(());
            }
            let alias = clean_alias(alias);
            let final_path =
                unique_download_path(downloads, &sanitize_filename(&name), receiving).await;
            let partial_path = downloads.join(format!(
                ".{}.{}.airwire-part",
                sanitize_filename(&name),
                hex::encode(id)
            ));
            let file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&partial_path)
                .await
            {
                Ok(file) => file,
                Err(error) => {
                    let _ = events.try_send(UiEvent::Error(format!(
                        "cannot receive {}: {error}",
                        final_path.display()
                    )));
                    return Ok(());
                }
            };
            receiving.insert(
                id,
                ReceivingFile {
                    from: alias.clone(),
                    final_path,
                    partial_path,
                    file,
                    hasher: Sha256::new(),
                    expected_size: size,
                    received: 0,
                    next_sequence: 0,
                },
            );
            let _ = events.try_send(UiEvent::Status(format!(
                "receiving {name} ({}) from {alias}",
                human_bytes(size)
            )));
        }
        AppFrame::FileChunk {
            id,
            sequence,
            bytes,
        } => {
            let Some(receive) = receiving.get_mut(&id) else {
                return Ok(());
            };
            if bytes.len() > FILE_CHUNK_SIZE
                || sequence != receive.next_sequence
                || receive.received + bytes.len() as u64 > receive.expected_size
            {
                let broken = receiving.remove(&id).expect("entry exists");
                let _ = fs::remove_file(broken.partial_path).await;
                let _ = events.try_send(UiEvent::Error("discarded an invalid file stream".into()));
                return Ok(());
            }
            receive.file.write_all(&bytes).await?;
            receive.hasher.update(&bytes);
            receive.received += bytes.len() as u64;
            receive.next_sequence += 1;
            let _ = events.try_send(UiEvent::FileProgress {
                label: format!("receiving {}", receive.final_path.display()),
                transferred: receive.received,
                total: receive.expected_size,
            });
        }
        AppFrame::FileComplete { id, sha256 } => {
            let Some(mut receive) = receiving.remove(&id) else {
                return Ok(());
            };
            receive.file.flush().await?;
            drop(receive.file);
            let digest: [u8; 32] = receive.hasher.finalize().into();
            if receive.received != receive.expected_size || digest != sha256 {
                let _ = fs::remove_file(&receive.partial_path).await;
                let _ = events.try_send(UiEvent::Error(
                    "file integrity check failed; partial file removed".into(),
                ));
                return Ok(());
            }
            fs::rename(&receive.partial_path, &receive.final_path).await?;
            let _ = events.try_send(UiEvent::FileReceived {
                from: receive.from,
                path: receive.final_path,
            });
        }
        AppFrame::VoiceStart { alias } => {
            let _ = events.try_send(UiEvent::VoiceStarted(clean_alias(alias)));
        }
        AppFrame::VoicePcm {
            sample_rate,
            samples,
        } => {
            if samples.len() > 4_096 || sample_rate == 0 || sample_rate > 384_000 {
                return Ok(());
            }
            let _ = events.try_send(UiEvent::VoicePcm {
                sample_rate,
                samples,
            });
        }
        AppFrame::VoiceStop { alias } => {
            let _ = events.try_send(UiEvent::VoiceStopped(clean_alias(alias)));
        }
    }
    Ok(())
}

async fn stream_file(
    path: PathBuf,
    alias: String,
    frames: mpsc::Sender<AppFrame>,
    events: mpsc::Sender<UiEvent>,
) -> Result<()> {
    let metadata = fs::metadata(&path)
        .await
        .with_context(|| format!("cannot read {}", path.display()))?;
    if !metadata.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    if metadata.len() > MAX_FILE_SIZE {
        bail!(
            "{} exceeds the {} MiB limit",
            path.display(),
            MAX_FILE_SIZE / 1024 / 1024
        );
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("file name is not valid UTF-8"))?
        .to_owned();
    let id = rand::random();
    let mime = mime_guess::from_path(&path)
        .first_or_octet_stream()
        .to_string();
    frames
        .send(AppFrame::FileOffer {
            id,
            alias,
            name: name.clone(),
            size: metadata.len(),
            mime,
        })
        .await
        .map_err(|_| anyhow!("network task ended"))?;

    let mut file = File::open(&path).await?;
    let mut hasher = Sha256::new();
    let mut sequence = 0_u32;
    let mut transferred = 0_u64;
    let mut buffer = vec![0_u8; FILE_CHUNK_SIZE];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        frames
            .send(AppFrame::FileChunk {
                id,
                sequence,
                bytes: buffer[..read].to_vec(),
            })
            .await
            .map_err(|_| anyhow!("network task ended"))?;
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| anyhow!("file has too many chunks"))?;
        transferred += read as u64;
        let _ = events.try_send(UiEvent::FileProgress {
            label: format!("sending {name}"),
            transferred,
            total: metadata.len(),
        });
        tokio::task::yield_now().await;
    }
    frames
        .send(AppFrame::FileComplete {
            id,
            sha256: hasher.finalize().into(),
        })
        .await
        .map_err(|_| anyhow!("network task ended"))?;
    let _ = events.try_send(UiEvent::Status(format!("sent {name}")));
    Ok(())
}

async fn broadcast_frame(
    security: &SecurityState,
    self_id: PeerId,
    frame: AppFrame,
    relay_tx: &mpsc::Sender<RelayCommand>,
) -> Result<bool> {
    let Some(key) = room_key(security) else {
        return Ok(false);
    };
    let sealed = crypto::seal_group(key, &self_id.0, &frame)?;
    send_packet(relay_tx, None, WirePacket::GroupSealed(sealed)).await?;
    Ok(true)
}

async fn send_packet(
    relay_tx: &mpsc::Sender<RelayCommand>,
    to: Option<PeerId>,
    packet: WirePacket,
) -> Result<()> {
    relay_tx
        .send(RelayCommand::Data {
            to,
            payload: encode(&packet)?,
        })
        .await
        .map_err(|_| anyhow!("relay writer stopped"))
}

fn room_key(security: &SecurityState) -> Option<&[u8; 32]> {
    match security {
        SecurityState::Host { room_key, .. } => Some(room_key),
        SecurityState::Guest { room_key, .. } => room_key.as_ref(),
    }
}

async fn next_relay_event(
    socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
) -> Result<RelayEvent> {
    while let Some(message) = socket.next().await {
        match message? {
            Message::Binary(bytes) => return decode(&bytes),
            Message::Close(_) => bail!("relay closed during registration"),
            _ => {}
        }
    }
    bail!("relay closed during registration")
}

async fn connect(
    relay_url: &str,
    tor_proxy: Option<&str>,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    if tor_proxy.is_none() {
        let (socket, _) = connect_async(relay_url)
            .await
            .with_context(|| format!("cannot connect to relay {relay_url}"))?;
        return Ok(socket);
    }

    let parsed = url::Url::parse(relay_url).context("invalid relay URL")?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("relay URL has no host"))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| anyhow!("relay URL has no port"))?;
    let proxy = tor_proxy
        .expect("checked")
        .strip_prefix("socks5://")
        .unwrap_or(tor_proxy.expect("checked"));
    let socks = Socks5Stream::connect(proxy, (host, port))
        .await
        .with_context(|| format!("cannot reach {host}:{port} through SOCKS5 proxy {proxy}"))?;
    let tcp = socks.into_inner();
    let (socket, _) = client_async_tls_with_config(relay_url, tcp, None, None)
        .await
        .context("WebSocket/TLS handshake through SOCKS5 failed")?;
    Ok(socket)
}

async fn connect_with_retry(
    relay_url: &str,
    tor_proxy: Option<&str>,
    retry: bool,
    events: &mpsc::Sender<UiEvent>,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    let deadline = tokio::time::Instant::now() + TOR_CONNECT_RETRY_WINDOW;
    let mut attempt = 0_u32;
    loop {
        attempt += 1;
        let connection = if retry {
            match tokio::time::timeout(TOR_CONNECT_ATTEMPT_TIMEOUT, connect(relay_url, tor_proxy))
                .await
            {
                Ok(result) => result,
                Err(_) => Err(anyhow!(
                    "timed out connecting to {relay_url} through the managed Tor session"
                )),
            }
        } else {
            connect(relay_url, tor_proxy).await
        };
        match connection {
            Ok(socket) => return Ok(socket),
            Err(error) if retry && tokio::time::Instant::now() < deadline => {
                if attempt == 1 {
                    let _ = events
                        .try_send(UiEvent::Status("waiting for the Tor onion service…".into()));
                }
                tokio::time::sleep(Duration::from_secs(u64::from(attempt.min(5)))).await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn sanitize_filename(name: &str) -> String {
    let leaf = Path::new(name)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment");
    let cleaned: String = leaf
        .chars()
        .map(|character| {
            if character.is_control() || matches!(character, '/' | '\\' | ':' | '\0') {
                '_'
            } else {
                character
            }
        })
        .take(180)
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "attachment".into()
    } else {
        cleaned
    }
}

async fn unique_download_path(
    directory: &Path,
    name: &str,
    receiving: &HashMap<[u8; 16], ReceivingFile>,
) -> PathBuf {
    let candidate = directory.join(name);
    if download_path_available(&candidate, receiving).await {
        return candidate;
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment");
    let extension = path.extension().and_then(|value| value.to_str());
    for number in 1..10_000 {
        let file_name = match extension {
            Some(extension) => format!("{stem} ({number}).{extension}"),
            None => format!("{stem} ({number})"),
        };
        let candidate = directory.join(file_name);
        if download_path_available(&candidate, receiving).await {
            return candidate;
        }
    }
    directory.join(format!("{}-{}", PeerId::random().short(), name))
}

async fn download_path_available(
    candidate: &Path,
    receiving: &HashMap<[u8; 16], ReceivingFile>,
) -> bool {
    fs::metadata(candidate).await.is_err()
        && !receiving
            .values()
            .any(|receive| receive.final_path == candidate)
}

fn clean_alias(alias: String) -> String {
    let alias: String = alias
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_ALIAS_CHARS)
        .collect();
    if alias.trim().is_empty() {
        "anonymous".into()
    } else {
        alias
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{Duration, timeout};

    #[test]
    fn sanitizes_hostile_names() {
        assert_eq!(sanitize_filename("../../secret.txt"), "secret.txt");
        assert_eq!(sanitize_filename("bad:name\u{0}.txt"), "bad_name_.txt");
        assert_eq!(sanitize_filename(".."), "attachment");
    }

    #[tokio::test]
    async fn chooses_a_non_overwriting_download_name() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("photo.png"), b"x")
            .await
            .unwrap();
        let selected = unique_download_path(temp.path(), "photo.png", &HashMap::new()).await;
        assert_eq!(
            selected.file_name().and_then(|value| value.to_str()),
            Some("photo (1).png")
        );
    }

    #[tokio::test]
    async fn encrypted_group_chat_and_file_transfer_work_end_to_end() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay_address = listener.local_addr().unwrap();
        let relay_task = tokio::spawn(async move {
            crate::relay::serve_listener(listener).await.unwrap();
        });
        let relay_url = format!("ws://{relay_address}/ws");
        let host_downloads = tempfile::tempdir().unwrap();
        let guest_downloads = tempfile::tempdir().unwrap();
        let second_guest_downloads = tempfile::tempdir().unwrap();
        let (host_command_tx, host_command_rx) = mpsc::channel(128);
        let (host_event_tx, mut host_event_rx) = mpsc::channel(256);
        let (guest_command_tx, guest_command_rx) = mpsc::channel(128);
        let (guest_event_tx, mut guest_event_rx) = mpsc::channel(256);

        let host_task = tokio::spawn(run(
            ClientConfig {
                role: Role::Host { max_guests: 2 },
                code: "aB3xY9".into(),
                relay_url: relay_url.clone(),
                tor_proxy: None,
                retry_connect: false,
                alias: "host".into(),
                downloads: host_downloads.path().into(),
            },
            host_command_rx,
            host_event_tx,
        ));
        wait_for_secure(&mut host_event_rx).await;

        let guest_task = tokio::spawn(run(
            ClientConfig {
                role: Role::Guest,
                code: "aB3xY9".into(),
                relay_url,
                tor_proxy: None,
                retry_connect: false,
                alias: "guest".into(),
                downloads: guest_downloads.path().into(),
            },
            guest_command_rx,
            guest_event_tx,
        ));
        wait_for_secure(&mut guest_event_rx).await;

        let (second_command_tx, second_command_rx) = mpsc::channel(128);
        let (second_event_tx, mut second_event_rx) = mpsc::channel(256);
        let second_task = tokio::spawn(run(
            ClientConfig {
                role: Role::Guest,
                code: "aB3xY9".into(),
                relay_url: format!("ws://{relay_address}/ws"),
                tor_proxy: None,
                retry_connect: false,
                alias: "second".into(),
                downloads: second_guest_downloads.path().into(),
            },
            second_command_rx,
            second_event_tx,
        ));
        wait_for_secure(&mut second_event_rx).await;

        second_command_tx
            .send(NetCommand::Chat("hello encrypted group".into()))
            .await
            .unwrap();
        wait_for_chat(&mut host_event_rx, "second", "hello encrypted group").await;
        wait_for_chat(&mut guest_event_rx, "second", "hello encrypted group").await;

        let source = guest_downloads.path().join("payload.bin");
        let contents = b"encrypted attachment bytes";
        fs::write(&source, contents).await.unwrap();
        guest_command_tx
            .send(NetCommand::SendFile(source))
            .await
            .unwrap();
        let received_path = timeout(Duration::from_secs(5), async {
            loop {
                if let Some(UiEvent::FileReceived { from, path }) = host_event_rx.recv().await {
                    assert_eq!(from, "guest");
                    break path;
                }
            }
        })
        .await
        .expect("host did not receive the attachment");
        assert_eq!(fs::read(received_path).await.unwrap(), contents);

        let _ = second_command_tx.send(NetCommand::Shutdown).await;
        let _ = guest_command_tx.send(NetCommand::Shutdown).await;
        let _ = host_command_tx.send(NetCommand::Shutdown).await;
        timeout(Duration::from_secs(5), second_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        timeout(Duration::from_secs(5), guest_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        timeout(Duration::from_secs(5), host_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        relay_task.abort();
    }

    async fn wait_for_secure(events: &mut mpsc::Receiver<UiEvent>) {
        timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    events.recv().await,
                    Some(UiEvent::Ready { secure: true, .. })
                ) {
                    break;
                }
            }
        })
        .await
        .expect("secure handshake timed out");
    }

    async fn wait_for_chat(
        events: &mut mpsc::Receiver<UiEvent>,
        expected_alias: &str,
        expected_text: &str,
    ) {
        timeout(Duration::from_secs(5), async {
            loop {
                if let Some(UiEvent::Chat {
                    alias,
                    text,
                    mine: false,
                }) = events.recv().await
                {
                    assert_eq!(alias, expected_alias);
                    assert_eq!(text, expected_text);
                    break;
                }
            }
        })
        .await
        .expect("chat was not delivered");
    }
}
