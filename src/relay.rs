use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::IntoResponse,
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use tokio::{
    net::TcpListener,
    sync::{Mutex, mpsc},
};

use crate::protocol::{
    MAX_RELAY_PACKET, PROTOCOL_VERSION, PeerId, RelayCommand, RelayEvent, decode, encode,
    validate_rendezvous,
};

#[derive(Clone, Default)]
struct RelayState {
    rooms: Arc<Mutex<HashMap<String, Room>>>,
}

struct Room {
    host: PeerId,
    max_guests: usize,
    peers: HashMap<PeerId, mpsc::Sender<RelayEvent>>,
    join_attempts: VecDeque<Instant>,
}

enum Registration {
    Host { code: String, max_guests: usize },
    Join { code: String },
}

pub async fn serve(listen: SocketAddr) -> Result<()> {
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("could not bind relay to {listen}"))?;
    serve_listener(listener).await
}

pub async fn bind_embedded(listen: SocketAddr) -> Result<TcpListener> {
    TcpListener::bind(listen)
        .await
        .with_context(|| format!("could not bind embedded relay to {listen}"))
}

pub async fn serve_listener(listener: TcpListener) -> Result<()> {
    let state = RelayState::default();
    let app = Router::new()
        .route("/health", get(|| async { "airwire relay ok" }))
        .route("/ws", get(websocket))
        .with_state(state);
    axum::serve(listener, app).await.context("relay stopped")
}

async fn websocket(
    upgrade: WebSocketUpgrade,
    State(state): State<RelayState>,
) -> impl IntoResponse {
    upgrade
        .max_message_size(MAX_RELAY_PACKET)
        .max_frame_size(MAX_RELAY_PACKET)
        .on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: RelayState) {
    let Some(Ok(Message::Binary(first))) = socket.next().await else {
        send_direct_error(&mut socket, "the first packet must register a room").await;
        return;
    };

    let registration = match decode::<RelayCommand>(&first) {
        Ok(RelayCommand::Host {
            version,
            rendezvous,
            max_guests,
        }) if version == PROTOCOL_VERSION && validate_rendezvous(&rendezvous) => {
            Registration::Host {
                code: rendezvous,
                max_guests: max_guests.clamp(1, 1_024),
            }
        }
        Ok(RelayCommand::Join {
            version,
            rendezvous,
        }) if version == PROTOCOL_VERSION && validate_rendezvous(&rendezvous) => {
            Registration::Join { code: rendezvous }
        }
        Ok(RelayCommand::Host { version, .. }) | Ok(RelayCommand::Join { version, .. })
            if version != PROTOCOL_VERSION =>
        {
            send_direct_error(&mut socket, "incompatible protocol version").await;
            return;
        }
        _ => {
            send_direct_error(&mut socket, "invalid registration").await;
            return;
        }
    };

    let id = PeerId::random();
    let (event_tx, mut event_rx) = mpsc::channel(64);
    let (code, is_host) = match register(&state, id, event_tx.clone(), registration).await {
        Ok(value) => value,
        Err(message) => {
            send_direct_error(&mut socket, &message).await;
            return;
        }
    };

    let (mut ws_tx, mut ws_rx) = socket.split();
    let writer = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let Ok(bytes) = encode(&event) else {
                break;
            };
            if ws_tx.send(Message::Binary(bytes.into())).await.is_err() {
                break;
            }
        }
    });

    let _ = event_tx.try_send(RelayEvent::Registered { id, is_host });

    while let Some(message) = ws_rx.next().await {
        let Ok(Message::Binary(bytes)) = message else {
            if matches!(message, Ok(Message::Close(_)) | Err(_)) {
                break;
            }
            continue;
        };
        if bytes.len() > MAX_RELAY_PACKET {
            break;
        }
        let Ok(RelayCommand::Data { to, payload }) = decode(&bytes) else {
            continue;
        };
        if payload.len() > MAX_RELAY_PACKET {
            continue;
        }
        route_data(&state, &code, id, to, payload).await;
    }

    unregister(&state, &code, id, is_host).await;
    writer.abort();
}

async fn register(
    state: &RelayState,
    id: PeerId,
    sender: mpsc::Sender<RelayEvent>,
    registration: Registration,
) -> std::result::Result<(String, bool), String> {
    let mut rooms = state.rooms.lock().await;
    match registration {
        Registration::Host { code, max_guests } => {
            if rooms.contains_key(&code) {
                return Err("room code collision; start again".into());
            }
            let mut peers = HashMap::new();
            peers.insert(id, sender);
            rooms.insert(
                code.clone(),
                Room {
                    host: id,
                    max_guests,
                    peers,
                    join_attempts: VecDeque::new(),
                },
            );
            Ok((code, true))
        }
        Registration::Join { code } => {
            let room = rooms
                .get_mut(&code)
                .ok_or_else(|| "room was not found".to_owned())?;
            let cutoff = Instant::now() - Duration::from_secs(60);
            while room
                .join_attempts
                .front()
                .is_some_and(|attempt| *attempt < cutoff)
            {
                room.join_attempts.pop_front();
            }
            if room.join_attempts.len() >= 60 {
                return Err("room join rate limit exceeded; try again shortly".into());
            }
            room.join_attempts.push_back(Instant::now());
            if room.peers.len().saturating_sub(1) >= room.max_guests {
                return Err("room has reached its guest limit".into());
            }
            let host_sender = room
                .peers
                .get(&room.host)
                .cloned()
                .ok_or_else(|| "room host is unavailable".to_owned())?;
            room.peers.insert(id, sender);
            let _ = host_sender.try_send(RelayEvent::PeerJoined { id });
            Ok((code, false))
        }
    }
}

async fn route_data(
    state: &RelayState,
    code: &str,
    from: PeerId,
    to: Option<PeerId>,
    payload: Vec<u8>,
) {
    let rooms = state.rooms.lock().await;
    let Some(room) = rooms.get(code) else {
        return;
    };
    if !room.peers.contains_key(&from) {
        return;
    }
    let event = RelayEvent::Data { from, payload };
    if let Some(target) = to {
        if target != from
            && let Some(sender) = room.peers.get(&target)
        {
            let _ = sender.try_send(event);
        }
    } else {
        for (peer, sender) in &room.peers {
            if *peer != from {
                let _ = sender.try_send(event.clone_for_relay());
            }
        }
    }
}

async fn unregister(state: &RelayState, code: &str, id: PeerId, is_host: bool) {
    let mut rooms = state.rooms.lock().await;
    if is_host {
        if let Some(room) = rooms.remove(code) {
            for (peer, sender) in room.peers {
                if peer != id {
                    let _ = sender.try_send(RelayEvent::RoomClosed);
                }
            }
        }
        return;
    }
    if let Some(room) = rooms.get_mut(code) {
        room.peers.remove(&id);
        for sender in room.peers.values() {
            let _ = sender.try_send(RelayEvent::PeerLeft { id });
        }
    }
}

async fn send_direct_error(socket: &mut WebSocket, message: &str) {
    if let Ok(bytes) = encode(&RelayEvent::Error {
        message: message.to_owned(),
    }) {
        let _ = socket.send(Message::Binary(bytes.into())).await;
    }
    let _ = socket.close().await;
}

trait CloneRelayEvent {
    fn clone_for_relay(&self) -> Self;
}

impl CloneRelayEvent for RelayEvent {
    fn clone_for_relay(&self) -> Self {
        match self {
            RelayEvent::Data { from, payload } => RelayEvent::Data {
                from: *from,
                payload: payload.clone(),
            },
            _ => unreachable!("only data events are broadcast"),
        }
    }
}
