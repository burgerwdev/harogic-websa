//! Fixture-only, loopback HTTP adapter. Not the WebSA /api/config or /ws protocol.
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Query, State,
    },
    http::{header::ORIGIN, HeaderMap, StatusCode, Uri},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    net::{SocketAddr, TcpListener},
    sync::{
        mpsc::{sync_channel, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, oneshot, Semaphore};
use websa_channelizer::{ChannelAction, ChannelCommand, ControlRequest};

#[derive(Clone)]
struct ControlState {
    sender: SyncSender<ControlRequest>,
    pending: Arc<Semaphore>,
    plan: Arc<Plan>,
    frames: FrameHub,
    browser: Option<BrowserAccess>,
}

#[derive(Clone)]
struct BrowserAccess {
    origin: String,
    token: String,
}

impl BrowserAccess {
    fn new(origin: String, token: String) -> Result<Self, String> {
        let uri: Uri = origin.parse().map_err(|_| "invalid lab browser Origin")?;
        if uri.scheme_str() != Some("http")
            || uri.host() != Some("127.0.0.1")
            || uri.port_u16().is_none()
            || uri.path() != "/"
            || uri.query().is_some()
            || token.len() < 32
        {
            return Err(
                "lab browser access requires loopback HTTP Origin and 32-byte token".into(),
            );
        }
        Ok(Self { origin, token })
    }

    fn from_env() -> Result<Option<Self>, String> {
        match (
            std::env::var("WEBSA_RUST_BROWSER_ORIGIN"),
            std::env::var("WEBSA_RUST_BROWSER_TOKEN"),
        ) {
            (Err(std::env::VarError::NotPresent), Err(std::env::VarError::NotPresent)) => Ok(None),
            (Ok(origin), Ok(token)) => Self::new(origin, token).map(Some),
            _ => Err("set both WEBSA_RUST_BROWSER_ORIGIN and WEBSA_RUST_BROWSER_TOKEN".into()),
        }
    }

    fn permits(&self, origin: &str, token: Option<&str>) -> bool {
        let supplied = token.unwrap_or("").as_bytes();
        let secret = self.token.as_bytes();
        origin == self.origin
            && supplied.len() == secret.len()
            && bool::from(supplied.ct_eq(secret))
    }
}

#[derive(Clone, Serialize)]
struct Plan {
    epoch: u64,
    bins: Vec<u32>,
    usable_bandwidth_hz: f64,
    capture_center_hz: f64,
    iq_sample_rate_hz: f64,
    channel_budget: usize,
    output_rate_budget: f64,
}

#[derive(Clone)]
pub struct FrameHub {
    inner: Arc<Mutex<FrameState>>,
    max_clients: usize,
}

impl Default for FrameHub {
    fn default() -> Self {
        Self::new(1)
    }
}

#[derive(Default)]
struct FrameState {
    generation: u64,
    current: BTreeMap<u64, Subscriber>,
}

struct Subscriber {
    generation: u64,
    sender: mpsc::Sender<Vec<u8>>,
}

impl FrameHub {
    pub fn new(max_clients: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FrameState::default())),
            max_clients,
        }
    }

    fn attach(&self, id: u64) -> Result<(u64, mpsc::Receiver<Vec<u8>>), &'static str> {
        let mut state = self.inner.lock().unwrap();
        state.current.retain(|_, sub| !sub.sender.is_closed());
        if state.current.contains_key(&id) {
            return Err("IQ subscriber already active");
        }
        if state.current.len() >= self.max_clients {
            return Err("IQ subscriber budget exceeded");
        }
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or("IQ session id exhausted")?;
        let (sender, receiver) = mpsc::channel(2);
        let generation = state.generation;
        state.current.insert(id, Subscriber { generation, sender });
        Ok((generation, receiver))
    }

    pub fn selected(&self, id: u64) -> Option<u64> {
        let mut state = self.inner.lock().unwrap();
        if state
            .current
            .get(&id)
            .is_some_and(|sub| sub.sender.is_closed())
        {
            state.current.remove(&id);
        }
        state.current.get(&id).map(|sub| sub.generation)
    }

    pub fn send(&self, id: u64, generation: u64, frame: Vec<u8>) -> Result<(), &'static str> {
        let mut state = self.inner.lock().unwrap();
        let sub = state.current.get(&id).ok_or("IQ subscriber disconnected")?;
        if sub.generation != generation {
            return Err("IQ subscriber changed during delivery");
        }
        match sub.sender.try_send(frame) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err("IQ websocket queue full"),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                state.current.remove(&id);
                Err("IQ subscriber disconnected during delivery")
            }
        }
    }

    fn detach(&self, id: u64, generation: u64) {
        let mut state = self.inner.lock().unwrap();
        if state
            .current
            .get(&id)
            .is_some_and(|sub| sub.generation == generation)
        {
            state.current.remove(&id);
        }
    }

    fn close(&self) {
        self.inner.lock().unwrap().current.clear();
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Subscribe {
        epoch: u64,
        id: u64,
        bin: u32,
        offset_hz: f64,
        decimate: i32,
    },
    Unsubscribe {
        epoch: u64,
        id: u64,
    },
    Retune {
        epoch: u64,
        old_id: u64,
        id: u64,
        bin: u32,
        offset_hz: f64,
        decimate: i32,
    },
}

impl Request {
    fn channel(&self, plan: &Plan) -> Option<ChannelInfo> {
        let (id, bin, offset_hz, decimate) = match self {
            Self::Subscribe {
                id,
                bin,
                offset_hz,
                decimate,
                ..
            }
            | Self::Retune {
                id,
                bin,
                offset_hz,
                decimate,
                ..
            } => (*id, *bin, *offset_hz, *decimate),
            Self::Unsubscribe { .. } => return None,
        };
        let signed = if bin >= 64 {
            bin as i32 - 128
        } else {
            bin as i32
        };
        Some(ChannelInfo {
            id,
            bin,
            center_hz: plan.capture_center_hz + signed as f64 * plan.iq_sample_rate_hz / 128.0
                - offset_hz,
            sample_rate_hz: plan.iq_sample_rate_hz / 64.0 / decimate as f64,
        })
    }

    fn command(self) -> ChannelCommand {
        let (epoch, action) = match self {
            Self::Subscribe {
                epoch,
                id,
                bin,
                offset_hz,
                decimate,
            } => (
                epoch,
                ChannelAction::Subscribe {
                    id,
                    bin,
                    offset_hz,
                    decimate,
                },
            ),
            Self::Unsubscribe { epoch, id } => (epoch, ChannelAction::Unsubscribe { id }),
            Self::Retune {
                epoch,
                old_id,
                id,
                bin,
                offset_hz,
                decimate,
            } => (
                epoch,
                ChannelAction::Retune {
                    old_id,
                    id,
                    bin,
                    offset_hz,
                    decimate,
                },
            ),
        };
        ChannelCommand { epoch, action }
    }
}

#[derive(Serialize)]
struct Accepted {
    seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<ChannelInfo>,
}

#[derive(Serialize)]
struct ChannelInfo {
    id: u64,
    bin: u32,
    center_hz: f64,
    sample_rate_hz: f64,
}
#[derive(Serialize)]
struct Rejected {
    error: &'static str,
}

type HttpError = (StatusCode, Json<Rejected>);

fn rejected(status: StatusCode, error: &'static str) -> HttpError {
    (status, Json(Rejected { error }))
}

async fn command(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Json(request): Json<Request>,
) -> Result<Json<Accepted>, HttpError> {
    // Fixture control has no authentication; do not accept browser-origin requests.
    if headers.contains_key(ORIGIN) {
        return Err(rejected(StatusCode::FORBIDDEN, "origin not allowed"));
    }
    let _permit = state
        .pending
        .try_acquire_owned()
        .map_err(|_| rejected(StatusCode::TOO_MANY_REQUESTS, "control requests busy"))?;
    let channel = request.channel(&state.plan);
    let (reply, received) = sync_channel(1);
    state
        .sender
        .try_send(ControlRequest {
            command: request.command(),
            reply,
        })
        .map_err(|error| match error {
            TrySendError::Full(_) => rejected(StatusCode::TOO_MANY_REQUESTS, "control queue full"),
            TrySendError::Disconnected(_) => {
                rejected(StatusCode::SERVICE_UNAVAILABLE, "IQ stream closed")
            }
        })?;
    let outcome =
        tokio::task::spawn_blocking(move || received.recv_timeout(Duration::from_millis(500)))
            .await
            .map_err(|_| rejected(StatusCode::SERVICE_UNAVAILABLE, "control worker failed"))?;
    match outcome {
        Ok(Ok(seq)) => Ok(Json(Accepted { seq, channel })),
        Ok(Err(error)) => Err(rejected(StatusCode::BAD_REQUEST, error)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(rejected(
            StatusCode::GATEWAY_TIMEOUT,
            "command outcome unknown",
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(rejected(
            StatusCode::SERVICE_UNAVAILABLE,
            "IQ stream closed",
        )),
    }
}

async fn get_plan(
    State(state): State<ControlState>,
    headers: HeaderMap,
) -> Result<Json<Plan>, HttpError> {
    if headers.contains_key(ORIGIN) {
        return Err(rejected(StatusCode::FORBIDDEN, "origin not allowed"));
    }
    Ok(Json(state.plan.as_ref().clone()))
}

async fn iq_socket(
    upgrade: WebSocketUpgrade,
    State(state): State<ControlState>,
    headers: HeaderMap,
    Query(query): Query<IqQuery>,
) -> Result<impl IntoResponse, HttpError> {
    if let Some(origin) = headers.get(ORIGIN) {
        let allowed = origin
            .to_str()
            .ok()
            .and_then(|value| {
                state
                    .browser
                    .as_ref()
                    .map(|access| access.permits(value, query.token.as_deref()))
            })
            .unwrap_or(false);
        if !allowed {
            return Err(rejected(StatusCode::FORBIDDEN, "origin not allowed"));
        }
    }
    if query.epoch != state.plan.epoch || query.id == 0 {
        return Err(rejected(
            StatusCode::BAD_REQUEST,
            "IQ channel epoch/id mismatch",
        ));
    }
    let (generation, receiver) = state
        .frames
        .attach(query.id)
        .map_err(|error| rejected(StatusCode::CONFLICT, error))?;
    let hub = state.frames;
    Ok(upgrade.on_upgrade(move |socket| send_iq(socket, receiver, hub, query.id, generation)))
}

#[derive(Deserialize)]
struct IqQuery {
    epoch: u64,
    id: u64,
    token: Option<String>,
}
async fn send_iq(
    mut socket: WebSocket,
    mut frames: mpsc::Receiver<Vec<u8>>,
    hub: FrameHub,
    id: u64,
    generation: u64,
) {
    loop {
        tokio::select! {
            frame = frames.recv() => {
                let Some(frame) = frame else { break; };
                if !matches!(tokio::time::timeout(Duration::from_secs(2),
                    socket.send(Message::Binary(frame.into()))).await, Ok(Ok(()))) { break; }
            }
            incoming = socket.recv() => {
                if !matches!(incoming, Some(Ok(Message::Ping(_) | Message::Pong(_)
                    | Message::Text(_) | Message::Binary(_)))) { break; }
            }
        }
    }
    hub.detach(id, generation);
}

pub struct HttpControl {
    addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<()>>,
    frames: FrameHub,
}

impl HttpControl {
    pub fn start(
        addr: SocketAddr,
        sender: SyncSender<ControlRequest>,
        epoch: u64,
        bins: Vec<u32>,
        usable_bandwidth_hz: f64,
        capture_center_hz: f64,
        iq_sample_rate_hz: f64,
        output_rate_budget: f64,
        frames: FrameHub,
    ) -> Result<Self, String> {
        if !addr.ip().is_loopback() {
            return Err("control HTTP must bind loopback".into());
        }
        let browser = BrowserAccess::from_env()?;
        let listener = TcpListener::bind(addr).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let plan = Arc::new(Plan {
            epoch,
            channel_budget: bins.len(),
            bins,
            usable_bandwidth_hz,
            capture_center_hz,
            iq_sample_rate_hz,
            output_rate_budget,
        });
        let (shutdown, stop) = oneshot::channel();
        let worker_frames = frames.clone();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("control runtime");
            runtime.block_on(async move {
                let listener =
                    tokio::net::TcpListener::from_std(listener).expect("control listener");
                let app = Router::new()
                    .route("/prototype/channels", get(get_plan).post(command))
                    .route("/prototype/iq", get(iq_socket))
                    .layer(DefaultBodyLimit::max(1024))
                    .with_state(ControlState {
                        sender,
                        pending: Arc::new(Semaphore::new(2)),
                        plan,
                        frames: worker_frames,
                        browser,
                    });
                if let Err(e) = axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = stop.await;
                    })
                    .await
                {
                    eprintln!("control HTTP: {e}");
                }
            });
        });
        Ok(Self {
            addr,
            shutdown: Some(shutdown),
            worker: Some(worker),
            frames,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for HttpControl {
    fn drop(&mut self) {
        self.frames.close();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_ws_access_needs_explicit_loopback_origin_and_token() {
        let key = "x".repeat(32);
        let access = BrowserAccess::new("http://127.0.0.1:8099".into(), key.clone()).unwrap();
        assert!(access.permits("http://127.0.0.1:8099", Some(&key)));
        assert!(!access.permits("http://127.0.0.1:8099", None));
        assert!(!access.permits("http://127.0.0.1:8099", Some(&"y".repeat(32))));
        assert!(!access.permits("http://evil.invalid", Some(&key)));
        assert!(BrowserAccess::new("http://evil.invalid".into(), key.clone()).is_err());
        assert!(BrowserAccess::new("http://127.0.0.1:8099".into(), "short".into()).is_err());
    }

    #[test]
    fn accepted_channel_geometry_uses_signed_bin_and_actual_center() {
        let plan = Plan {
            epoch: 7,
            bins: vec![5, 127],
            usable_bandwidth_hz: 50_000_000.0,
            capture_center_hz: 100_200_000.0,
            iq_sample_rate_hz: 62_500_000.0,
            channel_budget: 2,
            output_rate_budget: 2.0 * 62_500_000.0 / 64.0 / 12.0,
        };
        let positive = Request::Subscribe {
            epoch: 7,
            id: 1,
            bin: 5,
            offset_hz: 0.0,
            decimate: 12,
        }
        .channel(&plan)
        .unwrap();
        assert_eq!(positive.center_hz, 102_641_406.25);
        assert_eq!(positive.sample_rate_hz, 62_500_000.0 / 64.0 / 12.0);
        let negative = Request::Retune {
            epoch: 7,
            old_id: 1,
            id: 2,
            bin: 127,
            offset_hz: -10_000.0,
            decimate: 12,
        }
        .channel(&plan)
        .unwrap();
        assert_eq!(negative.center_hz, 99_721_718.75);
        assert!(Request::Unsubscribe { epoch: 7, id: 1 }
            .channel(&plan)
            .is_none());
    }

    #[test]
    fn one_subscriber_bounded_queue_and_reconnect_generation() {
        let hub = FrameHub::default();
        let (first, mut rx) = hub.attach(1).unwrap();
        assert_eq!(hub.selected(1), Some(first));
        assert_eq!(hub.attach(1).err(), Some("IQ subscriber already active"));
        assert_eq!(hub.attach(2).err(), Some("IQ subscriber budget exceeded"));
        hub.send(1, first, vec![0]).unwrap();
        hub.send(1, first, vec![1]).unwrap();
        assert_eq!(hub.send(1, first, vec![2]), Err("IQ websocket queue full"));
        assert_eq!(rx.try_recv().unwrap(), [0]);
        drop(rx);
        assert_eq!(hub.selected(1), None);
        let (second, _rx) = hub.attach(1).unwrap();
        assert!(second > first);
        hub.detach(1, first);
        assert_eq!(hub.selected(1), Some(second));
        assert_eq!(
            hub.send(1, first, vec![1]),
            Err("IQ subscriber changed during delivery")
        );
        hub.close();
        assert_eq!(hub.selected(1), None);
    }

    #[test]
    fn independent_clients_have_independent_two_frame_queues() {
        let hub = FrameHub::new(2);
        let (first, mut a) = hub.attach(1).unwrap();
        let (second, mut b) = hub.attach(2).unwrap();
        assert_eq!(hub.attach(3).err(), Some("IQ subscriber budget exceeded"));
        hub.send(1, first, vec![10]).unwrap();
        hub.send(1, first, vec![11]).unwrap();
        assert_eq!(hub.send(1, first, vec![12]), Err("IQ websocket queue full"));
        hub.send(2, second, vec![20]).unwrap();
        assert_eq!(a.try_recv().unwrap(), [10]);
        assert_eq!(b.try_recv().unwrap(), [20]);
        hub.detach(1, first);
        assert_eq!(hub.selected(2), Some(second));
        let (third, _c) = hub.attach(3).unwrap();
        assert!(third > second);
    }

    #[test]
    fn full_iq_queue_fails_channel_instead_of_dropping_frames() {
        use crate::{CaptureConfig, Engine, N};
        let hub = FrameHub::default();
        let (generation, _receiver) = hub.attach(1).unwrap();
        hub.send(1, generation, vec![1]).unwrap();
        hub.send(1, generation, vec![2]).unwrap();
        let mut engine = Engine::new(Some(vec![5]), true, true, None).unwrap();
        let (_sender, commands) = websa_channelizer::control_channel();
        engine
            .configure_bandwidth(
                CaptureConfig {
                    bandwidth_hz: 50_000_000.0,
                    center_hz: 100_000_000.0,
                },
                commands,
                false,
                Some(hub),
            )
            .unwrap();
        let Engine::Output { hot: Some(hot), .. } = &mut engine else {
            panic!("missing bank")
        };
        hot.bank.subscribe(1, 5, 0.0, 12).unwrap();
        let packet = websa_channelizer::PfbOutput {
            seq: 0,
            epoch: 7,
            stamp: 0,
            input_index: 0,
            groups: 192,
            process_ns: 0,
            iq: vec![0.0; 2 * 192],
        };
        let err = hot.deliver(&packet, &[5]).unwrap_err();
        assert!(err.contains("IQ websocket queue full"), "{err}");
        assert_eq!(hot.wire_frames, 0);
        let next = websa_channelizer::PfbOutput {
            seq: 1,
            input_index: N as u64,
            ..packet
        };
        assert!(hot
            .deliver(&next, &[5])
            .unwrap_err()
            .contains("fine bank failed"));
    }
}
