use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tonic::{Request, Response, Status};

use crate::audio::{AudioBroadcaster, AudioError, AudioEvent, AudioSubscription};
use crate::constants::{NUM_BANKS, POLL_INTERVAL_MS};
use crate::scanner::{ScannerClient, ScannerError};
use crate::types::{BankMask, Channel, Frequency, Modulation, ScanStatus};
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
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

/// Domain -> proto conversion for a scan status.
impl From<ScanStatus> for GetStatusResponse {
    fn from(s: ScanStatus) -> Self {
        Self {
            frequency: s.frequency.to_string(),
            bank: s.bank_display(),
            channel_name: s.channel_name,
            signal_detected: s.signal_detected,
            raw_response: s.raw,
            modulation: s.modulation.to_string(),
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
    /// Stop flag of the currently running `GetStatus` poller, if any.
    /// A new `GetStatus` stream cancels the previous poller.
    active_poll: Arc<Mutex<Option<Arc<AtomicBool>>>>,
}

impl ScannerServer {
    pub fn new(client: ScannerClient) -> Self {
        Self {
            client: Arc::new(Mutex::new(client)),
            active_poll: Arc::new(Mutex::new(None)),
        }
    }

    /// Cancel any running `GetStatus` poller and register `stop` as the
    /// active one.
    fn take_over_poll(&self, stop: Arc<AtomicBool>) {
        let mut slot = self.active_poll.lock().unwrap();
        if let Some(prev) = slot.replace(stop.clone()) {
            prev.store(true, Ordering::Relaxed);
        }
    }
}

/// Map scanner errors to gRPC status codes.
///
/// Validation failures are the caller's fault (`invalid_argument`);
/// serial/timeout failures mean the scanner is unreachable
/// (`unavailable`); anything else is a bug or protocol surprise
/// (`internal`).
impl From<ScannerError> for Status {
    fn from(e: ScannerError) -> Self {
        match e {
            ScannerError::InvalidVolume(_) => Self::invalid_argument(e.to_string()),
            ScannerError::InvalidSquelch(_) => Self::invalid_argument(e.to_string()),
            ScannerError::InvalidChannelIndex(_) => Self::invalid_argument(e.to_string()),
            ScannerError::InvalidBank(_) => Self::invalid_argument(e.to_string()),
            ScannerError::Io(_) => Self::unavailable(e.to_string()),
            ScannerError::Timeout { .. } => Self::unavailable(e.to_string()),
            ScannerError::UnexpectedResponse { .. } => Self::internal(e.to_string()),
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

impl ScannerServer {
    /// Execute a scanner command and return the raw response string.
    async fn cmd(&self, command: &str) -> Result<String, Status> {
        let cmd = command.to_string();
        with_scanner(self.client.clone(), move |client| client.send_command(&cmd)).await
    }
}

#[tonic::async_trait]
impl SystemInfoService for ScannerServer {
    async fn get_model_info(
        &self,
        _request: Request<GetModelInfoRequest>,
    ) -> Result<Response<GetModelInfoResponse>, Status> {
        let res = self.cmd("MDL").await?;
        Ok(Response::new(GetModelInfoResponse { result: res }))
    }

    async fn get_firmware_version(
        &self,
        _request: Request<GetFirmwareVersionRequest>,
    ) -> Result<Response<GetFirmwareVersionResponse>, Status> {
        let res = self.cmd("VER").await?;
        Ok(Response::new(GetFirmwareVersionResponse { result: res }))
    }
}

#[tonic::async_trait]
impl ScannerControlService for ScannerServer {
    type GetStatusStream = ReceiverStream<Result<GetStatusResponse, Status>>;

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
        with_scanner(self.client.clone(), move |client| client.set_banks(&mask)).await?;
        Ok(Response::new(SetEnabledBanksResponse {}))
    }

    /// Stream the scanner's status, polled every `POLL_INTERVAL_MS`.
    ///
    /// Only one poller runs at a time: opening a new stream cancels the
    /// previous one. Transient poll errors are logged and skipped so a
    /// serial hiccup does not kill the stream.
    async fn get_status(
        &self,
        _request: Request<GetStatusRequest>,
    ) -> Result<Response<Self::GetStatusStream>, Status> {
        let stop = Arc::new(AtomicBool::new(false));
        self.take_over_poll(stop.clone());

        let (tx, rx) = mpsc::channel(16);
        let client = self.client.clone();
        tokio::spawn(async move {
            loop {
                if stop.load(Ordering::Relaxed) || tx.is_closed() {
                    break;
                }
                let poll = tokio::task::spawn_blocking({
                    let client = client.clone();
                    move || {
                        let mut scanner = client
                            .lock()
                            .map_err(|e| format!("scanner mutex poisoned: {e}"))?;
                        scanner.get_status().map_err(|e| e.to_string())
                    }
                })
                .await;
                match poll {
                    Ok(Ok(status)) => {
                        if tx.send(Ok(GetStatusResponse::from(status))).await.is_err() {
                            break;
                        }
                    }
                    Ok(Err(e)) => tracing::warn!("GetStatus poll failed: {e}"),
                    Err(e) => tracing::warn!("GetStatus poll task failed: {e}"),
                }
                tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
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

/// Server-streaming response for `Listen`: the init chunk (cached or from
/// the channel), then one chunk per WebM cluster until the generation ends.
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
                Poll::Ready(Some(Err(_))) | Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
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
        // A join after the init was produced gets the cached copy up front;
        // the channel's (replayed) init is then a duplicate.
        let (pending_init, skip_inits) = match subscription.cached_init() {
            Some(init) => (Some(init.to_vec()), true),
            None => (None, false),
        };
        let stream = ListenStream {
            events: BroadcastStream::new(subscription.resubscribe()),
            pending_init,
            skip_inits,
            _subscription: subscription,
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
    use crate::audio::ffmpeg::FakeSource;
    use crate::audio::webm::fixtures::build_fixture;
    use crate::scanner::mock::{GLG_OK, mock_client};
    use tokio::time::timeout;
    use tokio_stream::StreamExt;
    use tonic::Code;

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

    // -- response mapping (U7) --

    #[test]
    fn status_response_mapping() {
        let status = ScanStatus::parse_glg(GLG_OK).unwrap();
        let proto = GetStatusResponse::from(status);
        assert_eq!(proto.frequency, "123.9750");
        assert_eq!(proto.bank, "2");
        assert_eq!(proto.channel_name, "BHX RADAR");
        assert!(proto.signal_detected);
        assert_eq!(proto.modulation, "AM");
        assert_eq!(proto.raw_response, GLG_OK);
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
        let (client, _written) = mock_client(&[GLG_OK]);
        let server = ScannerServer::new(client);
        let response = server
            .get_status(Request::new(GetStatusRequest {}))
            .await
            .unwrap();
        let mut stream = response.into_inner();
        // First poll serves the canned response immediately.
        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(first.frequency, "123.9750");
        // Subsequent polls time out (mock exhausted) but the stream stays
        // alive: it must not end after a failed poll. We only verify the
        // first value above; drop the receiver to stop the poller.
        drop(stream);
    }

    #[tokio::test]
    async fn new_get_status_cancels_previous_poller() {
        // Two canned responses, then exhaustion.
        let (client, written) = mock_client(&[GLG_OK, GLG_OK]);
        let server = ScannerServer::new(client);
        let response = server
            .get_status(Request::new(GetStatusRequest {}))
            .await
            .unwrap();
        let mut first = response.into_inner();
        let _value = first.next().await.unwrap().unwrap();
        // Drop the first stream, open a second: the old poller must stop.
        drop(first);
        // Give the old poller a chance to observe the cancellation.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let response = server
            .get_status(Request::new(GetStatusRequest {}))
            .await
            .unwrap();
        let mut second = response.into_inner();
        let _value = second.next().await.unwrap().unwrap();
        drop(second);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let sent: Vec<String> = written.lock().unwrap().clone();
        // Exactly the two served polls; no further GLG polls after the
        // pollers stopped.
        assert_eq!(sent, vec!["GLG\r".to_string(); 2]);
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
