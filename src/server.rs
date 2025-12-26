use std::sync::{Arc, Mutex};
use tonic::{Request, Response, Status};
use crate::scanner::ScannerClient;
use ubc125_grpc::ubc125::v1::system_info_service_server::SystemInfoService;
use ubc125_grpc::ubc125::v1::scanner_control_service_server::ScannerControlService;
use ubc125_grpc::ubc125::v1::{
    GetAudioSettingsRequest, GetAudioSettingsResponse,
    GetModelInfoRequest, GetModelInfoResponse,
    GetFirmwareVersionRequest, GetFirmwareVersionResponse,
    StartScanRequest, StartScanResponse,
    HoldScanRequest, HoldScanResponse,
    GetEnabledBanksRequest, GetEnabledBanksResponse,
    SetEnabledBanksRequest, SetEnabledBanksResponse,
    GetStatusRequest, GetStatusResponse,
    GetChannelRequest, GetChannelResponse,
    SetChannelRequest, SetChannelResponse,
    DeleteChannelRequest, DeleteChannelResponse,
};
use tokio_stream::wrappers::ReceiverStream;

#[derive(Clone)]
pub struct ScannerServer {
    pub client: Arc<Mutex<ScannerClient>>,
}

#[tonic::async_trait]
impl SystemInfoService for ScannerServer {
    async fn get_model_info(
        &self,
        request: Request<GetModelInfoRequest>,
    ) -> Result<Response<GetModelInfoResponse>, Status> {
        println!("Got a request: {:?}", request);
        let client = self.client.clone();
        let res = tokio::task::spawn_blocking(move || {
            let mut client = client.lock().unwrap();
            client.send_command("MDL").map_err(|e| Status::internal(e.to_string()))
        }).await.unwrap()?;

        Ok(Response::new(GetModelInfoResponse { result: res }))
    }

    async fn get_firmware_version(
        &self,
        request: Request<GetFirmwareVersionRequest>,
    ) -> Result<Response<GetFirmwareVersionResponse>, Status> {
        println!("Got a request: {:?}", request);
        let client = self.client.clone();
        let res = tokio::task::spawn_blocking(move || {
            let mut client = client.lock().unwrap();
            client.send_command("VER").map_err(|e| Status::internal(e.to_string()))
        }).await.unwrap()?;

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
        let client = self.client.clone();
        let (vol, sql) = tokio::task::spawn_blocking(move || {
            let mut client = client.lock().unwrap();
            let vol = client.get_volume().map_err(|e| Status::internal(e.to_string()))?;
            let sql = client.get_squelch().map_err(|e| Status::internal(e.to_string()))?;
            Ok::<_, Status>((vol, sql))
        }).await.unwrap()?;

        Ok(Response::new(GetAudioSettingsResponse { volume: vol, squelch: sql }))
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