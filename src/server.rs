use std::io;
use std::sync::{Arc, Mutex};
use tonic::{Request, Response, Status};

use crate::scanner::ScannerClient;
use tokio_stream::wrappers::ReceiverStream;
use ubc125_grpc::ubc125::v1::scanner_control_service_server::ScannerControlService;
use ubc125_grpc::ubc125::v1::system_info_service_server::SystemInfoService;
use ubc125_grpc::ubc125::v1::{
    DeleteChannelRequest, DeleteChannelResponse, GetAudioSettingsRequest, GetAudioSettingsResponse,
    GetChannelRequest, GetChannelResponse, GetEnabledBanksRequest, GetEnabledBanksResponse,
    GetFirmwareVersionRequest, GetFirmwareVersionResponse, GetModelInfoRequest,
    GetModelInfoResponse, GetStatusRequest, GetStatusResponse, HoldScanRequest, HoldScanResponse,
    SetChannelRequest, SetChannelResponse, SetEnabledBanksRequest, SetEnabledBanksResponse,
    StartScanRequest, StartScanResponse,
};

#[derive(Clone)]
pub struct ScannerServer {
    pub client: Arc<Mutex<ScannerClient>>,
}

/// Execute a blocking scanner operation inside `spawn_blocking`.
///
/// Acquires the `Mutex<ScannerClient>`, runs the closure, and converts
/// `io::Error` to `Status::internal`.
async fn with_scanner<F, T>(client: Arc<Mutex<ScannerClient>>, f: F) -> Result<T, Status>
where
    F: FnOnce(&mut ScannerClient) -> Result<T, io::Error> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut scanner = client.lock().map_err(|e| Status::internal(e.to_string()))?;
        f(&mut scanner).map_err(|e| Status::internal(e.to_string()))
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
        Err(Status::unimplemented("Not implemented"))
    }

    async fn hold_scan(
        &self,
        _request: Request<HoldScanRequest>,
    ) -> Result<Response<HoldScanResponse>, Status> {
        Err(Status::unimplemented("Not implemented"))
    }

    async fn get_enabled_banks(
        &self,
        _request: Request<GetEnabledBanksRequest>,
    ) -> Result<Response<GetEnabledBanksResponse>, Status> {
        Err(Status::unimplemented("Not implemented"))
    }

    async fn set_enabled_banks(
        &self,
        _request: Request<SetEnabledBanksRequest>,
    ) -> Result<Response<SetEnabledBanksResponse>, Status> {
        Err(Status::unimplemented("Not implemented"))
    }

    async fn get_status(
        &self,
        _request: Request<GetStatusRequest>,
    ) -> Result<Response<Self::GetStatusStream>, Status> {
        Err(Status::unimplemented("Not implemented"))
    }

    async fn get_channel(
        &self,
        _request: Request<GetChannelRequest>,
    ) -> Result<Response<GetChannelResponse>, Status> {
        Err(Status::unimplemented("Not implemented"))
    }

    async fn set_channel(
        &self,
        _request: Request<SetChannelRequest>,
    ) -> Result<Response<SetChannelResponse>, Status> {
        Err(Status::unimplemented("Not implemented"))
    }

    async fn delete_channel(
        &self,
        _request: Request<DeleteChannelRequest>,
    ) -> Result<Response<DeleteChannelResponse>, Status> {
        Err(Status::unimplemented("Not implemented"))
    }
}
