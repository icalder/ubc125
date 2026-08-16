// gRPC-Web client layer. Wraps the generated service clients and
// normalizes the scanner's raw "PREFIX,value" response strings.
import { createClient } from "@connectrpc/connect";
import { createGrpcWebTransport } from "@connectrpc/connect-web";
import { ConnectError } from "@connectrpc/connect";
import {
  SystemInfoService,
  ScannerControlService,
} from "../proto/ubc125/v1/services_pb.js";

export function createClients(baseUrl) {
  const transport = createGrpcWebTransport({ baseUrl });
  return {
    system: createClient(SystemInfoService, transport),
    scanner: createClient(ScannerControlService, transport),
  };
}

/**
 * Strip the scanner response prefix: "MDL,UBC125XLT" -> "UBC125XLT".
 * Returns the input unchanged when there is no comma.
 */
export function stripPrefix(response) {
  const i = response.indexOf(",");
  return i >= 0 ? response.slice(i + 1) : response;
}

/** Normalize a ConnectError to a short human-readable message. */
export function errorMessage(err) {
  if (err instanceof ConnectError) {
    return `${err.codeName}: ${err.message}`;
  }
  return err instanceof Error ? err.message : String(err);
}
