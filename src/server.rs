use tonic::{Request, Response, Status};
use ubc125_grpc::ubc125::v1::system_info_service_server::SystemInfoService;

pub struct SystemInfoServiceImpl {}

#[tonic::async_trait]
impl SystemInfoService for SystemInfoServiceImpl {
    async fn get_model_info(
        &self,
        request: Request<ubc125_grpc::ubc125::v1::GetModelInfoRequest>,
    ) -> Result<Response<ubc125_grpc::ubc125::v1::GetModelInfoResponse>, Status> {
        println!("Got a request: {:?}", request);

        let reply = ubc125_grpc::ubc125::v1::GetModelInfoResponse {
            result: "TODO UBC-125XLT".to_string(),
        };

        Ok(Response::new(reply))
    }

    async fn get_firmware_version(
        &self,
        request: Request<ubc125_grpc::ubc125::v1::GetFirmwareVersionRequest>,
    ) -> Result<Response<ubc125_grpc::ubc125::v1::GetFirmwareVersionResponse>, Status> {
        println!("Got a request: {:?}", request);

        let reply = ubc125_grpc::ubc125::v1::GetFirmwareVersionResponse {
            result: "TODO 1.0.0".to_string(),
        };

        Ok(Response::new(reply))
    }
}
