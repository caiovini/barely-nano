use rsnano_rpc_handler::RpcCommandHandler;
use rsnano_rpc_messages::RpcCommand;
use alloc::string::String;
use alloc::format;
use core::str;

pub struct BareMetalRpcServer {
    handler: RpcCommandHandler,
}

impl BareMetalRpcServer {
    pub fn new(handler: RpcCommandHandler) -> Self {
        Self { handler }
    }

    pub fn handle_request(&self, request: &str) -> Option<String> {
        if let Some(body_start) = request.find("\r\n\r\n") {
            let body = &request[body_start + 4..];
            if !body.is_empty() {
                match serde_json::from_str::<RpcCommand>(body) {
                    Ok(command) => {
                        let result = self.handler.handle(command);
                        return Some(self.format_response(result));
                    }
                    Err(e) => {
                        return Some(format!(
                            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{{\"error\":\"Invalid JSON: {}\"}}",
                            e
                        ));
                    }
                }
            }
        }

        // Support for GET requests by mapping them to RpcCommands
        if request.starts_with("GET /telemetry") {
            let result = self.handler.handle(RpcCommand::Telemetry(Default::default()));
            return Some(self.format_response(result));
        }

        if request.starts_with("GET /block_count") {
            let result = self.handler.handle(RpcCommand::BlockCount);
            return Some(self.format_response(result));
        }

        if request.starts_with("GET /peers") {
            let result = self.handler.handle(RpcCommand::Peers(Default::default()));
            return Some(self.format_response(result));
        }

        if request.starts_with("GET /uptime") {
            let result = self.handler.handle(RpcCommand::Uptime);
            return Some(self.format_response(result));
        }

        if request.starts_with("GET /version") {
            let result = self.handler.handle(RpcCommand::Version);
            return Some(self.format_response(result));
        }

        None
    }

    fn format_response(&self, result: serde_json::Value) -> String {
        let resp_body = serde_json::to_string(&result).unwrap_or_else(|_| String::from("{\"error\":\"Internal error\"}"));
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            resp_body.len(), resp_body
        )
    }
}
