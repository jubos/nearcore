#![feature(await_macro, async_await, arbitrary_self_types)]

use std::convert::TryFrom;
use std::convert::TryInto;
use std::sync::Arc;
use std::time::{Instant, Duration};

use actix::{Addr, MailboxError};
use actix_web::{middleware, web, App, Error as HttpError, HttpResponse, HttpServer};
use base64;
use futures::future::Future;
use futures03::{compat::Future01CompatExt as _, FutureExt as _, TryFutureExt as _};
use protobuf::parse_from_bytes;
use serde::de::DeserializeOwned;
use serde_derive::{Deserialize, Serialize};
use serde_json::Value;
use tokio::timer::Delay;

use message::Message;
use near_client::{ClientActor, GetBlock, Query, Status, TxDetails, TxStatus, ViewClientActor};
use near_network::NetworkClientMessages;
use near_primitives::hash::CryptoHash;
use near_primitives::transaction::{SignedTransaction, FinalTransactionStatus};
use near_primitives::types::BlockIndex;
use near_protos::signed_transaction as transaction_proto;

use crate::message::{Request, RpcError};

pub mod client;
mod message;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RpcConfig {
    pub addr: String,
    pub cors_allowed_origins: Vec<String>,
}

impl Default for RpcConfig {
    fn default() -> Self {
        RpcConfig { addr: "0.0.0.0:3030".to_string(), cors_allowed_origins: vec!["*".to_string()] }
    }
}

impl RpcConfig {
    pub fn new(addr: &str) -> Self {
        RpcConfig { addr: addr.to_string(), cors_allowed_origins: vec!["*".to_string()] }
    }
}

fn parse_params<T: DeserializeOwned>(value: Option<Value>) -> Result<T, RpcError> {
    if let Some(value) = value {
        serde_json::from_value(value)
            .map_err(|err| RpcError::invalid_params(Some(format!("Failed parsing args: {}", err))))
    } else {
        Err(RpcError::invalid_params(Some("Require at least one parameter".to_string())))
    }
}

fn jsonify<T: serde::Serialize>(
    response: Result<Result<T, String>, MailboxError>,
) -> Result<Value, RpcError> {
    response
        .map_err(|err| err.to_string())
        .and_then(|value| {
            value.and_then(|value| serde_json::to_value(value).map_err(|err| err.to_string()))
        })
        .map_err(|err| RpcError::server_error(Some(err)))
}

fn parse_tx(params: Option<Value>) -> Result<SignedTransaction, RpcError> {
    let (bs64,) = parse_params::<(String,)>(params)?;
    let bytes = base64::decode(&bs64).map_err(|err| RpcError::parse_error(err.to_string()))?;
    let tx: transaction_proto::SignedTransaction = parse_from_bytes(&bytes).map_err(|e| {
        RpcError::invalid_params(Some(format!("Failed to decode transaction proto: {}", e)))
    })?;
    Ok(tx.try_into().map_err(|e| {
        RpcError::invalid_params(Some(format!("Failed to decode transaction: {}", e)))
    })?)
}

fn parse_hash(params: Option<Value>) -> Result<CryptoHash, RpcError> {
    let (bs64,) = parse_params::<(String,)>(params)?;
    base64::decode(&bs64).map_err(|err| RpcError::parse_error(err.to_string())).and_then(|bytes| {
        CryptoHash::try_from(bytes).map_err(|err| RpcError::parse_error(err.to_string()))
    })
}

struct JsonRpcHandler {
    client_addr: Addr<ClientActor>,
    view_client_addr: Arc<Addr<ViewClientActor>>,
}

impl JsonRpcHandler {
    pub async fn process(self: web::Data<Self>, message: Message) -> Result<Message, HttpError> {
        let id = message.id();
        match message {
            Message::Request(request) => {
                let result = self.process_request(request).await;
                Ok(Message::response(id, result))
            },
            _ => Ok(Message::error(RpcError::invalid_request())),
        }
    }

    async fn process_request(self: web::Data<Self>, request: Request) -> Result<Value, RpcError> {
        match request.method.as_ref() {
            "broadcast_tx_async" => self.send_tx_async(request.params).await,
            "broadcast_tx_commit" => self.send_tx_commit(request.params).await,
            "query" => self.query(request.params).await,
            "health" => self.health().await,
            "status" => self.status().await,
            "tx" => self.tx_status(request.params).await,
            "tx_details" => self.tx_details(request.params).await,
            "block" => self.block(request.params).await,
            _ => Err(RpcError::method_not_found(request.method)),
        }
    }

    async fn send_tx_async(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let tx = parse_tx(params)?;
        let hash = (&tx.get_hash()).into();
        actix::spawn(
            self.client_addr
                .send(NetworkClientMessages::Transaction(tx))
                .map(|_| ())
                .map_err(|_| ()),
        );
        Ok(Value::String(hash))
    }

    async fn send_tx_commit(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let tx = parse_tx(params)?;
        let tx_hash = tx.get_hash();
        let view_client_addr = self.view_client_addr.clone();
        self.client_addr
            .send(NetworkClientMessages::Transaction(tx))
            .map_err(|err| RpcError::server_error(Some(err.to_string())))
            .compat()
            .await?;
        loop {
            let tx_status_result = view_client_addr.send(TxStatus { tx_hash }).compat().await;
            if let Ok(Ok(ref tx_status)) = tx_status_result {
                match tx_status.status {
                    | FinalTransactionStatus::Started
                    | FinalTransactionStatus::Unknown => {},
                    _ => {
                        break jsonify(tx_status_result);
                    },
                }
            }
            let _ = Delay::new(Instant::now() + Duration::from_millis(100)).compat().await;
        }
    }

    async fn health(self: web::Data<Self>) -> Result<Value, RpcError> {
        Ok(Value::Null)
    }

    async fn status(self: web::Data<Self>) -> Result<Value, RpcError> {
        jsonify(self.client_addr.send(Status {}).compat().await)
    }

    async fn query(self: web::Data<Self>, params: Option<Value>) -> Result<Value, RpcError> {
        let (path, data) = parse_params::<(String, Vec<u8>)>(params)?;
        jsonify(self.view_client_addr.send(Query { path, data }).compat().await)
    }

    async fn tx_status(self: web::Data<Self>, params: Option<Value>) -> Result<Value, RpcError> {
        let tx_hash = parse_hash(params)?;
        jsonify(self.clone().view_client_addr.clone().send(TxStatus { tx_hash }).compat().await)
    }

    async fn tx_details(self: web::Data<Self>, params: Option<Value>) -> Result<Value, RpcError> {
        let tx_hash = parse_hash(params)?;
        jsonify(self.view_client_addr.send(TxDetails { tx_hash }).compat().await)
    }

    async fn block(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let (height,) = parse_params::<(BlockIndex,)>(params)?;
        jsonify(self.view_client_addr.send(GetBlock::Height(height)).compat().await)
    }
}

fn rpc_handler(
    message: web::Json<Message>,
    handler: web::Data<JsonRpcHandler>,
) -> impl Future<Item = HttpResponse, Error = HttpError> {
    async move {
        let message = handler.process(message.0).await?;
        Ok(HttpResponse::Ok().json(message))
    }.boxed().compat()
}

pub fn start_http(
    config: RpcConfig,
    client_addr: Addr<ClientActor>,
    view_client_addr: Addr<ViewClientActor>,
) {
    HttpServer::new(move || {
        App::new()
            .data(JsonRpcHandler {
                client_addr: client_addr.clone(),
                view_client_addr: Arc::new(view_client_addr.clone()),
            })
            .wrap(middleware::Logger::default())
            .service(web::resource("/").route(web::post().to_async(rpc_handler)))
    })
    .bind(config.addr)
    .unwrap()
    .workers(4)
    .shutdown_timeout(5)
    .start();
}
