use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tonic::{Request, Response, Status};

use crate::audio::{
    stats::SharedAudioStats, AudioBroadcaster, AudioError, AudioEvent, AudioSubscription,
};
use crate::constants::NUM_BANKS;
use crate::scanner::{ScannerClient, ScannerError};
use crate::status::{StatusBroadcaster, StatusSubscription, StatusUpdate};
use crate::types::{BankMask, Channel, Frequency, Modulation};
use tokio_stream::Stream;
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};
use ubc125_grpc::ubc125::v1::audio_service_server::AudioService;
use ubc125_grpc::ubc125::v1::scanner_control_service_server::ScannerControlService;
use ubc125_grpc::ubc125::v1::system_info_service_server::SystemInfoService;
use ubc125_grpc::ubc125::v1::{
    AudioChunk, DeleteChannelRequest, DeleteChannelResponse, GetAudioSettingsRequest,
    StopCaptureRequest, StopCaptureResponse,
    GetAudioSettingsResponse, GetChannelRequest, GetChannelResponse, GetEnabledBanksRequest,
    GetEnabledBanksResponse, GetFirmwareVersionRequest, GetFirmwareVersionResponse,
    GetModelInfoRequest, GetModelInfoResponse, GetStatusRequest, GetStatusResponse,
    HoldScanRequest, HoldScanResponse, ListChannelsRequest, ListChannelsResponse,
    SetChannelRequest, SetChannelResponse, SetEnabledBanksRequest, SetEnabledBanksResponse,
    StartScanRequest, StartScanResponse, SubscribeAudioRequest,
};

/// Domain -> proto conversion for a status update (GLG + bank mask), so
/// every subscriber sees the mask the server currently believes.
impl From<StatusUpdate> for GetStatusResponse {
    fn from(u: StatusUpdate) -> Self {
        Self {
            frequency: u.status.frequency.to_string(),
            bank: u.status.bank_display(),
            channel_name: u.status.channel_name,
            signal_detected: u.status.signal_detected,
            raw_response: u.status.raw,
            modulation: u.status.modulation.to_string(),
            banks: u.banks.iter().map(|(_, enabled)| enabled).collect(),
        }
    }
}

/// Domain -> proto conversion for a channel.
impl From<Channel> for ubc125_grpc::ubc125::v1::Channel {
    fn from(c: Channel) -> Self {
        Self {
            index: c.index.get(),
            name: c.name,
            frequency: c.frequency.to_string(),
            modulation: c.modulation.to_string(),
        }
    }
}

/// Convert a proto channel to the domain type, validating index and
/// frequency. Returns `invalid_argument` on bad input.
fn channel_from_proto(c: ubc125_grpc::ubc125::v1::Channel) -> Result<Channel, Status> {
    let index = crate::types::ChannelIndex::new(c.index)
        .ok_or_else(|| Status::invalid_argument(format!("invalid channel index: {}", c.index)))?;
    let frequency = Frequency::from_user_input(&c.frequency)
        .ok_or_else(|| Status::invalid_argument(format!("invalid frequency: {}", c.frequency)))?;
    // Empty modulation defaults to AM (the console edit flow is AM-only).
    let modulation = if c.modulation.is_empty() {
        Modulation::Am
    } else {
        c.modulation.parse::<Modulation>().unwrap_or(Modulation::Am)
    };
    Ok(Channel {
        index,
        name: c.name,
        frequency,
        modulation,
    })
}

#[derive(Clone)]
pub struct ScannerServer {
    pub client: Arc<Mutex<ScannerClient>>,
    /// Shared `GetStatus` poller: one poll task for any number of
    /// subscribers. The old singleton design cancelled the previous poller
    /// when a new stream opened, so two clients cancelled each other in a
    /// ping-pong that flapped their offline banners (KI-2).
    status: StatusBroadcaster,
}

impl ScannerServer {
    pub fn new(client: ScannerClient) -> Self {
        let client = Arc::new(Mutex::new(client));
        let status = StatusBroadcaster::new(client.clone());
        Self { client, status }
    }
}

/// Map scanner errors to gRPC status codes.
///
/// Validation failures are the caller's fault (`invalid_argument`);
/// serial/timeout failures mean the scanner is unreachable
/// (`unavailable`); the scanner's own negative acks are classified by
/// their meaning (`ERR` = bad parameters → `invalid_argument`,
/// `NG` = wrong state/mode for the command → `failed_precondition`);
/// anything else is a protocol surprise (`internal`).
impl From<ScannerError> for Status {
    fn from(e: ScannerError) -> Self {
        // The message is taken before the match so the arms can move `e`'s
        // fields (e.g. `got`) while keeping the full Display text.
        let msg = e.to_string();
        match e {
            ScannerError::InvalidVolume(_) => Self::invalid_argument(msg),
            ScannerError::InvalidSquelch(_) => Self::invalid_argument(msg),
            ScannerError::InvalidChannelIndex(_) => Self::invalid_argument(msg),
            ScannerError::InvalidBank(_) => Self::invalid_argument(msg),
            ScannerError::Io(_) => Self::unavailable(msg),
            ScannerError::Timeout { .. } => Self::unavailable(msg),
            // `send_action` reports the scanner's nack tokens verbatim in
            // `got`; map them to the codes that tell the client what to do
            // (fix the arguments vs. check the scanner's mode).
            ScannerError::UnexpectedResponse { got, .. } => match got.as_str() {
                "ERR" => Self::invalid_argument(msg),
                "NG" => Self::failed_precondition(msg),
                _ => Self::internal(msg),
            },
        }
    }
}

/// Execute a blocking scanner operation inside `spawn_blocking`.
///
/// Acquires the `Mutex<ScannerClient>`, runs the closure, and maps
/// [`ScannerError`] to a [`Status`] via [`From`].
async fn with_scanner<F, T>(client: Arc<Mutex<ScannerClient>>, f: F) -> Result<T, Status>
where
    F: FnOnce(&mut ScannerClient) -> Result<T, ScannerError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut scanner = client.lock().map_err(|e| Status::internal(e.to_string()))?;
        f(&mut scanner).map_err(Status::from)
    })
    .await
    .map_err(|e| Status::internal(e.to_string()))?
}

#[tonic::async_trait]
impl SystemInfoService for ScannerServer {
    async fn get_model_info(
        &self,
        _request: Request<GetModelInfoRequest>,
    ) -> Result<Response<GetModelInfoResponse>, Status> {
        // Typed method: a timeout is an `unavailable` error, not a silent
        // empty string (the old `cmd()` escape hatch swallowed timeouts).
        let res = with_scanner(self.client.clone(), |client| client.get_model()).await?;
        Ok(Response::new(GetModelInfoResponse { result: res }))
    }

    async fn get_firmware_version(
        &self,
        _request: Request<GetFirmwareVersionRequest>,
    ) -> Result<Response<GetFirmwareVersionResponse>, Status> {
        // Typed method: see `get_model_info` for why not the raw escape hatch.
        let res =
            with_scanner(self.client.clone(), |client| client.get_firmware_version())
                .await?;
        Ok(Response::new(GetFirmwareVersionResponse { result: res }))
    }
}

#[tonic::async_trait]
impl ScannerControlService for ScannerServer {
    type GetStatusStream = Pin<Box<dyn Stream<Item = Result<GetStatusResponse, Status>> + Send>>;

    async fn get_audio_settings(
        &self,
        _request: Request<GetAudioSettingsRequest>,
    ) -> Result<Response<GetAudioSettingsResponse>, Status> {
        let (vol, sql) = with_scanner(self.client.clone(), |client| {
            let vol = client.get_volume()?;
            let sql = client.get_squelch()?;
            Ok((vol, sql))
        })
        .await?;

        Ok(Response::new(GetAudioSettingsResponse {
            volume: vol,
            squelch: sql,
        }))
    }

    async fn start_scan(
        &self,
        _request: Request<StartScanRequest>,
    ) -> Result<Response<StartScanResponse>, Status> {
        with_scanner(self.client.clone(), |client| client.start_scan()).await?;
        Ok(Response::new(StartScanResponse {}))
    }

    async fn hold_scan(
        &self,
        _request: Request<HoldScanRequest>,
    ) -> Result<Response<HoldScanResponse>, Status> {
        with_scanner(self.client.clone(), |client| client.hold_scan()).await?;
        Ok(Response::new(HoldScanResponse {}))
    }

    async fn get_enabled_banks(
        &self,
        _request: Request<GetEnabledBanksRequest>,
    ) -> Result<Response<GetEnabledBanksResponse>, Status> {
        let mask = with_scanner(self.client.clone(), |client| client.get_banks()).await?;
        // An authoritative read refreshes the status stream's cache.
        self.status.set_banks(&mask);
        Ok(Response::new(GetEnabledBanksResponse {
            banks: mask.iter().map(|(_, enabled)| enabled).collect(),
        }))
    }

    async fn set_enabled_banks(
        &self,
        request: Request<SetEnabledBanksRequest>,
    ) -> Result<Response<SetEnabledBanksResponse>, Status> {
        let req = request.into_inner();
        if req.banks.len() != NUM_BANKS {
            return Err(Status::invalid_argument(format!(
                "expected {NUM_BANKS} banks, got {}",
                req.banks.len()
            )));
        }
        let mask = BankMask::from_states(req.banks);
        // A copy for the status-stream cache below: `mask` moves into the
        // scanner call.
        let streamed_mask = mask.clone();
        with_scanner(self.client.clone(), move |client| client.set_banks(&mask)).await?;
        // Fast-forward the status stream's bank cache so every other
        // subscriber sees the new mask on the next poll (the poller's slow
        // radio refresh is the backstop).
        self.status.set_banks(&streamed_mask);
        Ok(Response::new(SetEnabledBanksResponse {}))
    }

    /// Stream the scanner's status, polled every `POLL_INTERVAL_MS` by a
    /// poller shared by all `GetStatus` subscribers (started by the first,
    /// stopped by the last). A client joining mid-generation gets the last
    /// polled status up front, then the live values. Each message also
    /// carries the bank mask the server currently believes, so a
    /// `SetEnabledBanks` from one client (or a bank button pressed on the
    /// unit) reaches all subscribers. Transient poll errors are logged and
    /// skipped so a serial hiccup does not kill the streams.
    async fn get_status(
        &self,
        _request: Request<GetStatusRequest>,
    ) -> Result<Response<Self::GetStatusStream>, Status> {
        let subscription = self.status.subscribe().await;
        // A join after the first poll gets the cached copy up front; the
        // channel only carries values sent after the join, so there is no
        // duplicate to skip.
        let pending_last = subscription
            .cached_status()
            .map(|u| GetStatusResponse::from(u.clone()));
        let stream = StatusStream {
            events: BroadcastStream::new(subscription.resubscribe()),
            pending_last,
            _subscription: subscription,
        };
        Ok(Response::new(Box::pin(stream)))
    }

    async fn get_channel(
        &self,
        request: Request<GetChannelRequest>,
    ) -> Result<Response<GetChannelResponse>, Status> {
        let index = request.into_inner().index;
        let channel =
            with_scanner(self.client.clone(), move |client| client.get_channel(index)).await?;
        Ok(Response::new(GetChannelResponse {
            channel: Some(channel.into()),
        }))
    }

    async fn set_channel(
        &self,
        request: Request<SetChannelRequest>,
    ) -> Result<Response<SetChannelResponse>, Status> {
        let req = request.into_inner();
        let proto_channel = req
            .channel
            .ok_or_else(|| Status::invalid_argument("channel is required"))?;
        let channel = channel_from_proto(proto_channel)?;
        with_scanner(self.client.clone(), move |client| {
            client.set_channel(&channel)
        })
        .await?;
        Ok(Response::new(SetChannelResponse {}))
    }

    async fn delete_channel(
        &self,
        request: Request<DeleteChannelRequest>,
    ) -> Result<Response<DeleteChannelResponse>, Status> {
        let index = request.into_inner().index;
        with_scanner(self.client.clone(), move |client| {
            client.delete_channel(index)
        })
        .await?;
        Ok(Response::new(DeleteChannelResponse {}))
    }

    async fn list_channels(
        &self,
        request: Request<ListChannelsRequest>,
    ) -> Result<Response<ListChannelsResponse>, Status> {
        let bank = request.into_inner().bank;
        let channels = with_scanner(self.client.clone(), move |client| {
            client.get_bank_channels(bank)
        })
        .await?;
        Ok(Response::new(ListChannelsResponse {
            channels: channels.into_iter().map(Into::into).collect(),
        }))
    }
}

/// Map audio errors to gRPC status codes: a shut-down broadcaster or a
/// capture that cannot run is `unavailable` to the client.
impl From<AudioError> for Status {
    fn from(e: AudioError) -> Self {
        match e {
            AudioError::Shutdown => Self::unavailable("audio is shut down"),
            AudioError::Source(s) => Self::unavailable(s.to_string()),
        }
    }
}

#[derive(Clone)]
pub struct AudioServer {
    broadcaster: Arc<AudioBroadcaster>,
}

impl AudioServer {
    pub fn new(broadcaster: Arc<AudioBroadcaster>) -> Self {
        Self { broadcaster }
    }
}

/// Server-streaming response for `GetStatus`: the cached update (for
/// mid-generation joins), then one message per successful poll of the
/// shared poller, until the poller stops.
struct StatusStream {
    events: BroadcastStream<StatusUpdate>,
    /// Last polled status owed to this client before any live values.
    pending_last: Option<GetStatusResponse>,
    /// Keeps the subscriber slot alive for the stream's lifetime; dropping
    /// the stream returns the slot to the broadcaster (the last one stops
    /// the shared poller).
    _subscription: StatusSubscription,
}

impl Stream for StatusStream {
    type Item = Result<GetStatusResponse, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(status) = self.pending_last.take() {
            return Poll::Ready(Some(Ok(status)));
        }
        loop {
            match Pin::new(&mut self.events).poll_next(cx) {
                // We fell behind (this client was slow): status is
                // latest-state, so skip to the next value rather than
                // ending the stream — ending it would flap the client's
                // offline banner (KI-2).
                Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(_)))) => continue,
                Poll::Ready(Some(Ok(update))) => {
                    return Poll::Ready(Some(Ok(GetStatusResponse::from(update))));
                }
                // The poller stopped (channel closed): end the stream; the
                // client reconnects, and a fresh poller starts if no other
                // subscriber remains.
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// How long the first subscriber waits for the pump to cache the init
/// segment before giving up and falling back to the channel. The init is
/// produced within the first capture period (~20–80 ms); the bound covers a
/// slow source start without masking a genuinely broken one for long.
const INIT_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Server-streaming response for `Listen`: the init chunk (cached or from
/// the channel), then one chunk per WebM cluster until the generation ends.
///
/// B5: this client's broadcast receiver drops the oldest chunks when it
/// cannot keep up (`Lagged`); the stream skips them instead of ending, so
/// a slow client drops audio instead of cycling a full reconnect — the
/// stream only ends when the generation itself ends.
struct ListenStream {
    events: BroadcastStream<AudioEvent>,
    /// Init chunk owed to this client before any buffered/channel media.
    pending_init: Option<Vec<u8>>,
    /// True when the cached init was sent: the channel's (replayed) init is
    /// a duplicate and is dropped so the client sees exactly one.
    skip_inits: bool,
    /// Keeps the subscriber slot (and the capture) alive for the stream's
    /// lifetime; dropping the stream returns the slot to the broadcaster.
    _subscription: AudioSubscription,
    /// B10: this listener's per-subscriber drop counters.
    stats: SharedAudioStats,
    subscriber_id: u64,
}

impl Stream for ListenStream {
    type Item = Result<AudioChunk, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(payload) = self.pending_init.take() {
            return Poll::Ready(Some(Ok(AudioChunk {
                payload,
                timestamp_ms: 0,
                init_segment: true,
            })));
        }
        loop {
            let events = Pin::new(&mut self.events);
            match events.poll_next(cx) {
                Poll::Ready(Some(Ok(AudioEvent::Init(payload, ts)))) => {
                    if self.skip_inits {
                        continue;
                    }
                    return Poll::Ready(Some(Ok(AudioChunk {
                        payload,
                        timestamp_ms: ts,
                        init_segment: true,
                    })));
                }
                Poll::Ready(Some(Ok(AudioEvent::Media(payload, ts)))) => {
                    return Poll::Ready(Some(Ok(AudioChunk {
                        payload,
                        timestamp_ms: ts,
                        init_segment: false,
                    })));
                }
                // Generation ended; surface the failure and end the stream.
                Poll::Ready(Some(Ok(AudioEvent::Failed))) => {
                    return Poll::Ready(Some(Err(Status::unavailable("audio capture ended"))));
                }
                // B5: we fell behind (this client was slow): the broadcast
                // channel already dropped the oldest `n` chunks. Skip to
                // the next one instead of ending the stream — ending it
                // would force a full reconnect (new init, new
                // MediaSource) for what is only a momentary stall. The
                // dropped audio is counted for the B10 reporter; the
                // client's buffer gap makes it audible as a hiccup, not
                // a restart.
                Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(n)))) => {
                    self.stats.subscriber_dropped(self.subscriber_id, n);
                    continue;
                }
                // The generation's channel closed: end the stream; the
                // client reconnects and a fresh generation starts if none
                // is running. (The only error variant is `Lagged`, handled
                // above, so a closed channel is the only other outcome.)
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl Drop for ListenStream {
    fn drop(&mut self) {
        // B10: remove this listener's counters (the stream ended by any
        // path: clean generation end, error, or client disconnect).
        self.stats.subscriber_stopped(self.subscriber_id);
    }
}

#[tonic::async_trait]
impl AudioService for AudioServer {
    type ListenStream = Pin<Box<dyn Stream<Item = Result<AudioChunk, Status>> + Send>>;

    async fn listen(
        &self,
        _request: Request<SubscribeAudioRequest>,
    ) -> Result<Response<Self::ListenStream>, Status> {
        let subscription = self.broadcaster.subscribe().await?;
        // A join after the init was produced gets the cached copy up front.
        // A fresh broadcast receiver only sees events sent after it was
        // created, so the original init cannot reach it; skip_inits guards
        // against a duplicate anyway.
        let (pending_init, skip_inits) = match subscription.cached_init() {
            Some(init) => (Some(init.to_vec()), true),
            // First subscriber: its cached_init is None because the pump
            // caches the init a moment after the source starts. Fetch it
            // from the cache (reliable) instead of relying on the bounded
            // channel to deliver the one-time Init event before a fast
            // source drops it — a dropped init would strand this client in
            // "connecting" with no MediaSource to start.
            None => match self
                .broadcaster
                .wait_for_init(subscription.gen_id(), INIT_WAIT_TIMEOUT)
                .await
            {
                Some(init) => (Some(init), true),
                None => (None, false),
            },
        };
        let stats = self.broadcaster.stats();
        let subscriber_id = self.broadcaster.register_subscriber();
        let stream = ListenStream {
            events: BroadcastStream::new(subscription.resubscribe()),
            pending_init,
            skip_inits,
            _subscription: subscription,
            stats,
            subscriber_id,
        };
        Ok(Response::new(Box::pin(stream)))
    }

    /// Explicit capture stop: a stopped browser listener keeps its
    /// keep-alive TCP connection open, so the server cannot detect the
    /// client is gone; the client calls this to release the device.
    async fn stop_capture(
        &self,
        _request: Request<StopCaptureRequest>,
    ) -> Result<Response<StopCaptureResponse>, Status> {
        self.broadcaster.stop_capture().await;
        Ok(Response::new(StopCaptureResponse {}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::source::FakeSource;
    use crate::audio::webm::fixtures::build_fixture;
    use crate::scanner::mock::{GLG_OK, mock_client};
    use crate::types::ScanStatus;
    use std::time::Duration;
    use tokio::time::timeout;
    use tokio_stream::StreamExt;
    use tonic::Code;

    /// A bank-mask read reply (alternating banks, like the fake scanner).
    const SCG_ALT: &str = "SCG,0101010101";
    /// One canned bank-refresh cycle: PRG, SCG reply, EPG, KEY,S,P reply.
    const SCG_CYCLE: [&str; 4] = ["PRG", SCG_ALT, "EPG", "KEY"];
    /// Canned responses for the poll-0 (GLG + bank refresh) plus `n` more
    /// GLG polls.
    fn canned_polls(n_glg_after_first: usize) -> Vec<&'static str> {
        let mut responses = vec![GLG_OK];
        responses.extend_from_slice(&SCG_CYCLE);
        responses.extend(std::iter::repeat(GLG_OK).take(n_glg_after_first));
        responses
    }
    /// The alternating mask `SCG_ALT` as the UI/server see it.
    const BANKS_ALT: [bool; 10] = [true, false, true, false, true, false, true, false, true, false];

    /// A broadcaster over a looping WebM fixture (header once, clusters on).
    fn audio_test_broadcaster(clusters: usize) -> (Arc<AudioBroadcaster>, Arc<FakeSource>) {
        let (stream, init) = build_fixture(clusters);
        let source = Arc::new(
            FakeSource::new(stream)
                .with_head(init.len())
                .with_delay(Duration::from_micros(200)),
        );
        (Arc::new(AudioBroadcaster::new(source.clone())), source)
    }

    // -- error mapping (U6) --

    #[test]
    fn status_from_validation_errors_is_invalid_argument() {
        let cases = [
            ScannerError::InvalidVolume(20),
            ScannerError::InvalidSquelch(20),
            ScannerError::InvalidChannelIndex(501),
        ];
        for e in cases {
            assert_eq!(Status::from(e).code(), Code::InvalidArgument);
        }
    }

    // -- ListChannels --

    fn list_channel_responses(bank: u32) -> Vec<String> {
        std::iter::once("PRG".to_string())
            .chain((1..=50u32).map(|i| {
                let index = (bank - 1) * 50 + i;
                format!("CIN,{index},NAME{index},01239750,AM,0,0,0,0")
            }))
            .collect()
    }

    #[tokio::test]
    async fn list_channels_returns_bank_channels() {
        let responses = list_channel_responses(2);
        let (client, _) = mock_client(&responses.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        let server = ScannerServer::new(client);
        let resp = server
            .list_channels(Request::new(ListChannelsRequest { bank: 2 }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.channels.len(), 50);
        assert_eq!(resp.channels[0].index, 51);
        assert_eq!(resp.channels[49].index, 100);
        assert_eq!(resp.channels[0].name, "NAME51");
    }

    #[tokio::test]
    async fn list_channels_invalid_bank_is_invalid_argument() {
        let (client, _) = mock_client(&[]);
        let server = ScannerServer::new(client);
        for bank in [0, 11] {
            let status = server
                .list_channels(Request::new(ListChannelsRequest { bank }))
                .await
                .unwrap_err();
            assert_eq!(status.code(), Code::InvalidArgument, "bank {bank}");
        }
    }

    #[test]
    fn status_from_serial_errors_is_unavailable() {
        let timeout = ScannerError::Timeout {
            command: "GLG".into(),
            partial: String::new(),
        };
        assert_eq!(Status::from(timeout).code(), Code::Unavailable);
        let io_err = ScannerError::Io(std::io::Error::other("port closed"));
        assert_eq!(Status::from(io_err).code(), Code::Unavailable);
    }

    #[test]
    fn status_from_unexpected_response_is_internal() {
        let e = ScannerError::UnexpectedResponse {
            command: "GLG".into(),
            got: "GARBAGE".into(),
        };
        assert_eq!(Status::from(e).code(), Code::Internal);
    }

    #[test]
    fn status_from_scanner_nacks_uses_precise_codes() {
        // ERR: the scanner rejected the command's parameters.
        let err = ScannerError::UnexpectedResponse {
            command: "VOL,99".into(),
            got: "ERR".into(),
        };
        assert_eq!(Status::from(err).code(), Code::InvalidArgument);
        // NG: the scanner is in the wrong state for the command.
        let ng = ScannerError::UnexpectedResponse {
            command: "SCG".into(),
            got: "NG".into(),
        };
        assert_eq!(Status::from(ng).code(), Code::FailedPrecondition);
    }

    // -- response mapping (U7) --

    #[test]
    fn status_response_mapping() {
        let status = ScanStatus::parse_glg(GLG_OK).unwrap();
        let banks = BankMask::from_scanner_response(SCG_ALT).unwrap();
        let proto = GetStatusResponse::from(StatusUpdate { status, banks });
        assert_eq!(proto.frequency, "123.9750");
        assert_eq!(proto.bank, "2");
        assert_eq!(proto.channel_name, "BHX RADAR");
        assert!(proto.signal_detected);
        assert_eq!(proto.modulation, "AM");
        assert_eq!(proto.raw_response, GLG_OK);
        assert_eq!(proto.banks, BANKS_ALT.to_vec());
    }

    #[test]
    fn channel_from_proto_valid() {
        let proto = ubc125_grpc::ubc125::v1::Channel {
            index: 52,
            name: "BHX RADAR".into(),
            frequency: "123.9750".into(),
            modulation: "AM".into(),
        };
        let channel = channel_from_proto(proto).unwrap();
        assert_eq!(channel.index.get(), 52);
        assert_eq!(channel.frequency.to_raw(), "01239750");
    }

    #[test]
    fn channel_from_proto_invalid_inputs() {
        let base = ubc125_grpc::ubc125::v1::Channel {
            index: 52,
            name: String::new(),
            frequency: "123.9750".into(),
            modulation: String::new(),
        };
        let bad_index = ubc125_grpc::ubc125::v1::Channel {
            index: 501,
            ..base.clone()
        };
        assert!(matches!(
            channel_from_proto(bad_index),
            Err(s) if s.code() == Code::InvalidArgument
        ));
        let bad_freq = ubc125_grpc::ubc125::v1::Channel {
            frequency: "not-a-frequency".into(),
            ..base
        };
        assert!(matches!(
            channel_from_proto(bad_freq),
            Err(s) if s.code() == Code::InvalidArgument
        ));
    }

    // -- GetStatus stream (U8) --

    #[tokio::test]
    async fn get_status_streams_polls_and_survives_failures() {
        let responses = canned_polls(0);
        let (client, _written) = mock_client(&responses);
        let server = ScannerServer::new(client);
        let response = server
            .get_status(Request::new(GetStatusRequest {}))
            .await
            .unwrap();
        let mut stream = response.into_inner();
        // First poll serves the canned response immediately, with the
        // bank mask from the poll-0 radio refresh.
        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(first.frequency, "123.9750");
        assert_eq!(first.banks, BANKS_ALT.to_vec());
        // Subsequent polls time out (mock exhausted) but the stream stays
        // alive: it must not end after a failed poll. We only verify the
        // first value above; drop the receiver to stop the poller.
        drop(stream);
    }

    #[tokio::test]
    async fn set_enabled_banks_reaches_subscribers_on_next_poll() {
        // Poll 1: GLG + poll-0 bank refresh. The SetEnabledBanks
        // round-trip (PRG, SCG<mask>, EPG, KEY,S,P) runs before poll 2's
        // GLG: it is issued as soon as the first message is received.
        let responses: Vec<&str> = [
            GLG_OK, "PRG", SCG_ALT, "EPG", "KEY",
            "PRG", "SCG,0000000000", "EPG", "KEY",
            GLG_OK,
        ]
        .into_iter()
        .collect();
        let (client, _written) = mock_client(&responses);
        let server = ScannerServer::new(client);
        let response = server
            .get_status(Request::new(GetStatusRequest {}))
            .await
            .unwrap();
        let mut stream = response.into_inner();
        let first = timeout(WAIT_AUDIO, stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first.banks, BANKS_ALT.to_vec());
        // Another client changes the mask...
        server
            .set_enabled_banks(Request::new(SetEnabledBanksRequest {
                banks: vec![true; NUM_BANKS],
            }))
            .await
            .unwrap();
        // ...and this subscriber sees it on the very next poll.
        let second = timeout(WAIT_AUDIO, stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            second.banks.iter().all(|b| *b),
            "streamed mask after set: {:?}",
            second.banks
        );
        drop(stream);
    }

    #[tokio::test]
    async fn get_status_subscribers_share_one_poller() {
        // Three canned GLG responses (plus the poll-0 bank refresh), then
        // exhaustion.
        let responses = canned_polls(3);
        let (client, written) = mock_client(&responses);
        let server = ScannerServer::new(client);
        let response = server
            .get_status(Request::new(GetStatusRequest {}))
            .await
            .unwrap();
        let mut first = response.into_inner();
        let v1 = timeout(WAIT_AUDIO, first.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(v1.frequency, "123.9750");
        // A second client joins: it must not cancel the first client's
        // stream (KI-2: the old singleton poller made two clients cancel
        // each other in a ping-pong that flapped their offline banners).
        let response = server
            .get_status(Request::new(GetStatusRequest {}))
            .await
            .unwrap();
        let mut second = response.into_inner();
        // The first client keeps receiving after the second joined.
        let v2 = timeout(WAIT_AUDIO, first.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(v2.frequency, "123.9750");
        // The second client gets the cached status up front (or a live
        // poll if it joined before the first one).
        let v3 = timeout(WAIT_AUDIO, second.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(v3.frequency, "123.9750");
        drop(first);
        drop(second);
        // The shared poller stops once the last subscriber leaves (grace
        // window plus at most one in-flight poll).
        let deadline = std::time::Instant::now() + WAIT_AUDIO;
        while server.status.is_active() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!server.status.is_active(), "poller did not stop");
        let sent: Vec<String> = written.lock().unwrap().clone();
        // One poller served both clients: one bank refresh for its life
        // (poll 0), and the three canned GLG polls plus at most one
        // in-flight after the stop.
        let count = |cmd: &str| {
            sent
                .iter()
                .filter(|c| **c == format!("{cmd}\r"))
                .count()
        };
        assert_eq!(count("SCG"), 1, "unexpected commands: {sent:?}");
        assert!(count("GLG") <= 4, "unexpected poll count: {sent:?}");
    }

    // -- AudioService::Listen (6.7) --

    async fn listen_first_chunks(server: &AudioServer, n: usize) -> Vec<AudioChunk> {
        let response = server
            .listen(Request::new(SubscribeAudioRequest {}))
            .await
            .expect("listen must succeed");
        let mut stream = response.into_inner();
        let mut chunks = Vec::new();
        for _ in 0..n {
            let chunk = timeout(WAIT_AUDIO, stream.next())
                .await
                .expect("timed out waiting for chunk")
                .expect("stream ended early")
                .expect("chunk error");
            chunks.push(chunk);
        }
        chunks
    }

    const WAIT_AUDIO: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn listen_starts_capture_and_streams_init_then_media() {
        let (broadcaster, _source) = audio_test_broadcaster(2);
        let server = AudioServer::new(broadcaster);
        let chunks = listen_first_chunks(&server, 3).await;
        assert!(chunks[0].init_segment, "first chunk is the init segment");
        assert!(!chunks[1].init_segment);
        assert!(!chunks[2].init_segment);
        assert!(!chunks[1].payload.is_empty());
        assert!(chunks[1].timestamp_ms <= chunks[2].timestamp_ms);
    }

    #[tokio::test]
    async fn listen_late_join_gets_cached_init_exactly_once() {
        let (broadcaster, _source) = audio_test_broadcaster(2);
        // Start the generation and wait until it produced the init. Keep
        // this subscriber alive so the `listen` below joins the same
        // generation (a second subscription then snapshots the cached init).
        let sub = broadcaster.subscribe().await.expect("subscribe");
        let mut rx = sub.resubscribe();
        loop {
            match timeout(WAIT_AUDIO, rx.recv()).await {
                Ok(Ok(AudioEvent::Init(_, _))) => break,
                Ok(Ok(AudioEvent::Media(_, _))) => continue,
                _ => panic!("init event never arrived"),
            }
        }
        let server = AudioServer::new(broadcaster);
        let chunks = listen_first_chunks(&server, 2).await;
        drop(sub);
        assert!(chunks[0].init_segment, "cached init comes first");
        assert!(!chunks[1].init_segment, "no duplicated init on the channel");
    }

    #[tokio::test]
    async fn listen_first_subscriber_gets_init_when_channel_drops_it() {
        // B1+B5 regression: a fast, bursty source drops the one-time Init
        // channel event out of the bounded (8-slot) subscriber channel
        // before the first subscriber's receiver — created after
        // wait_for_init — can read it. That subscriber must still get the
        // init, from the cache (reliable) rather than the lossy channel, or
        // it is stranded in "connecting" with no MediaSource to start.
        // Without the fix, the first chunk would be a Media (init_segment
        // false) because the channel Init was overwritten.
        let (stream, init) = build_fixture(100);
        // delay = 0 (FakeSource default): the pump emits all clusters as
        // fast as it can, so the Init is long gone from the 8-slot channel
        // by the time the receiver is created.
        let source = Arc::new(FakeSource::new(stream).with_head(init.len()));
        let broadcaster = Arc::new(AudioBroadcaster::new(source.clone()));
        let server = AudioServer::new(broadcaster);
        let chunks = listen_first_chunks(&server, 3).await;
        assert!(
            chunks[0].init_segment,
            "first chunk must be the init segment (from the cache, not the dropped channel event)"
        );
        assert!(!chunks[1].init_segment, "no duplicated init");
        assert!(!chunks[2].init_segment);
    }

    #[tokio::test]
    async fn listen_survives_lagging_and_counts_the_dropped_chunks() {
        // B5/R1: a subscriber that cannot keep up has its oldest chunks
        // dropped by the broadcast channel. The stream must stay alive and
        // resume at the newest chunks (the pre-B5 code ended it, forcing a
        // full reconnect for a momentary stall), and every drop must be
        // counted for that subscriber (B10).
        let (stream_bytes, init) = build_fixture(40);
        // Looping and unpaced: it outruns an unpolled subscriber by far more
        // than the 8-slot queue within a few hundred ms.
        let source = Arc::new(FakeSource::new(stream_bytes).with_head(init.len()));
        let broadcaster = Arc::new(AudioBroadcaster::new(source.clone()));
        let stats = broadcaster.stats();
        let server = AudioServer::new(broadcaster.clone());
        let mut listen = server
            .listen(Request::new(SubscribeAudioRequest {}))
            .await
            .expect("listen must succeed")
            .into_inner();
        let first = timeout(WAIT_AUDIO, listen.next())
            .await
            .expect("timed out waiting for the init chunk")
            .expect("stream ended early")
            .expect("init chunk error");
        assert!(first.init_segment, "init comes first");
        // Sit on the stream while the source floods past this subscriber.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let resumed = timeout(WAIT_AUDIO, listen.next())
            .await
            .expect("Lagged must not end the stream")
            .expect("stream ended instead of skipping to the newest chunks")
            .expect("chunk error");
        assert!(!resumed.init_segment, "media resumes after the lag");
        let drops: u64 = stats
            .snapshot()
            .subscribers
            .iter()
            .map(|(_, s)| s.lag_drops)
            .sum();
        assert!(drops > 0, "Lagged drops must be counted per subscriber");
        // The counters must not outlive the stream (no per-listener leak).
        drop(listen);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            stats.snapshot().subscribers.is_empty(),
            "a listener's counters must be removed when its stream ends"
        );
    }

    #[tokio::test]
    async fn listen_refused_after_shutdown() {
        let (broadcaster, _source) = audio_test_broadcaster(1);
        broadcaster.shutdown().await;
        let server = AudioServer::new(broadcaster);
        let err = match server.listen(Request::new(SubscribeAudioRequest {})).await {
            Ok(_) => panic!("listen after shutdown must fail"),
            Err(e) => e,
        };
        assert_eq!(err.code(), Code::Unavailable);
    }

    #[tokio::test]
    async fn listen_stream_terminates_when_capture_is_killed() {
        let (broadcaster, source) = audio_test_broadcaster(2);
        let server = AudioServer::new(broadcaster);
        let chunks = listen_first_chunks(&server, 1).await;
        assert!(chunks[0].init_segment);
        source.kill();
        // The stream must end (error or close), not hang.
        let response = server
            .listen(Request::new(SubscribeAudioRequest {}))
            .await
            .unwrap();
        let mut stream = response.into_inner();
        let ended = loop {
            match timeout(WAIT_AUDIO, stream.next()).await {
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(_))) | Ok(None) => break true,
                Err(_) => break false,
            }
        };
        assert!(ended, "stream must terminate after the source is killed");
    }
}
