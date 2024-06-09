use std::{collections::HashMap, marker::PhantomData, sync::Arc};

use actix::{Actor, StreamHandler};
use actix_web::{
    http::Method,
    web::{self, Payload, ServiceConfig},
    HttpRequest, HttpResponse,
};
use actix_web_actors::ws;
use extractor::TCtxFunc;
use futures::{executor::block_on, StreamExt};
use rspc::internal::{
    jsonrpc::{self, handle_json_rpc, RequestId, Sender},
    ProcedureKind,
};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

mod extractor;

pub fn configure<TCtx, TCtxFnMarker: Send + Sync + 'static, TCtxFn: TCtxFunc<TCtx, TCtxFnMarker>>(
    router: Arc<rspc::Router<TCtx>>,
    ctx_fn: TCtxFn,
) -> Box<dyn FnOnce(&mut ServiceConfig) + 'static>
where
    TCtx: Send + Sync + 'static,
    TCtxFnMarker: Unpin + Send + Sync + 'static,
{
    Box::new(move |config: &mut ServiceConfig| {
        config.service(web::resource("/{id}").to(
            move |req: HttpRequest,
                  data: web::Data<TCtx>,
                  id: web::Path<String>,
                  payload: web::Payload| {
                let router = router.clone();
                let ctx_fn = ctx_fn.clone();

                async move {
                    match (req.method(), id.as_str()) {
                        (&Method::GET, "ws") => {
                            #[cfg(feature = "ws")]
                            {
                                return handle_websocket(ctx_fn, &req, &router, &data, payload)
                                    .await;
                            }

                            #[cfg(not(feature = "ws"))]
                            return HttpResponse::NotFound().body("[]".to_owned());
                        }
                        (&Method::GET, _) => {
                            return Ok(handle_http(
                                ctx_fn,
                                ProcedureKind::Query,
                                &req,
                                payload,
                                &router,
                                &data,
                            )
                            .await);
                        }
                        (&Method::POST, _) => {
                            return Ok(handle_http(
                                ctx_fn,
                                ProcedureKind::Mutation,
                                &req,
                                payload,
                                &router,
                                &data,
                            )
                            .await);
                        }
                        _ => unreachable!("Invalid method or path"),
                    }
                }
            },
        ));
    })
}

async fn handle_http<TCtx, TCtxFn, TCtxFnMarker>(
    ctx_fn: TCtxFn,
    kind: ProcedureKind,
    req: &HttpRequest,
    mut payload: Payload,
    router: &Arc<rspc::Router<TCtx>>,
    data: &web::Data<TCtx>,
) -> HttpResponse
where
    TCtx: Send + Sync + 'static,
    TCtxFn: TCtxFunc<TCtx, TCtxFnMarker>,
{
    let procedure_name = req.uri().path().strip_prefix("/rspc").unwrap()[1..].to_string();
    let head = req.head();

    let input = match kind {
        ProcedureKind::Query => head
            .uri
            .query()
            .map(|query| form_urlencoded::parse(query.as_bytes()))
            .and_then(|mut params| params.find(|e| e.0 == "input").map(|e| e.1))
            .map(|v| serde_json::from_str(&v))
            .unwrap_or(Ok(None as Option<Value>)),
        ProcedureKind::Mutation => {
            let mut body = web::BytesMut::new();
            while let Some(chunk) = payload.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(_) => return HttpResponse::InternalServerError().finish(),
                };
                if (body.len() + chunk.len()) > usize::MAX {
                    return HttpResponse::PayloadTooLarge().finish();
                }
                body.extend_from_slice(&chunk);
            }
            (!body.is_empty())
                .then(|| serde_json::from_slice(&body))
                .unwrap_or(Ok(None))
        }
        _ => unreachable!("Invalid procedure kind"),
    };

    let input = match input {
        Ok(input) => input,
        Err(_err) => {
            #[cfg(feature = "tracing")]
            tracing::error!(
                "Error passing parameters to operation '{}' with key '{:?}': {}",
                kind.to_str(),
                procedure_name,
                _err
            );

            return HttpResponse::NotFound()
                .content_type("application/json")
                .body(b"[]".as_slice());
        }
    };

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "Executing operation '{}' with key '{}' with params {:?}",
        kind.to_str(),
        procedure_name,
        input
    );

    let mut resp = Sender::Response(None);

    let ctx = match ctx_fn.exec(&req, Some(data)) {
        Ok(ctx) => ctx,
        Err(_err) => {
            #[cfg(feature = "tracing")]
            tracing::error!("Error executing context function: {}", _err);

            return HttpResponse::InternalServerError()
                .content_type("application/json")
                .body(b"[]".as_slice());
        }
    };

    handle_json_rpc(
        ctx,
        jsonrpc::Request {
            jsonrpc: None,
            id: RequestId::Null,
            inner: match kind {
                ProcedureKind::Query => jsonrpc::RequestInner::Query {
                    path: procedure_name.to_string(), // TODO: Lifetime instead of allocate?
                    input,
                },
                ProcedureKind::Mutation => jsonrpc::RequestInner::Mutation {
                    path: procedure_name.to_string(), // TODO: Lifetime instead of allocate?
                    input,
                },
                ProcedureKind::Subscription => {
                    #[cfg(feature = "tracing")]
                    tracing::error!("Attempted to execute a subscription operation with HTTP");

                    return HttpResponse::InternalServerError()
                        .content_type("application/json")
                        .body(b"[]".as_slice());
                }
            },
        },
        router,
        &mut resp,
        &mut jsonrpc::SubscriptionMap::None,
    )
    .await;

    match resp {
        Sender::Response(Some(resp)) => match serde_json::to_vec(&resp) {
            Ok(v) => HttpResponse::Ok().content_type("application/json").body(v),
            Err(_err) => {
                #[cfg(feature = "tracing")]
                tracing::error!("Error serializing response: {}", _err);

                HttpResponse::InternalServerError()
                    .content_type("application/json")
                    .body(b"[]".as_slice())
            }
        },
        _ => unreachable!(),
    }
}

#[cfg(feature = "ws")]
async fn handle_websocket<TCtx, TCtxFn, TCtxFnMarker>(
    ctx_fn: TCtxFn,
    req: &HttpRequest,
    router: &Arc<rspc::Router<TCtx>>,
    data: &web::Data<TCtx>,
    payload: Payload,
) -> Result<HttpResponse, actix_web::Error>
where
    TCtx: Send + Sync + 'static,
    TCtxFn: TCtxFunc<TCtx, TCtxFnMarker>,
    TCtxFnMarker: Unpin + Send + Sync + 'static,
{
    #[cfg(feature = "tracing")]
    tracing::debug!("Accepting websocket connection");

    return actix_web_actors::ws::start(WebSocket::new(ctx_fn, req, router, data), &req, payload);
}

struct WebSocket<TCtx, TCtxFn, TCtxFnMarker>
where
    TCtx: Send + Sync + 'static,
    TCtxFn: TCtxFunc<TCtx, TCtxFnMarker> + Unpin,
{
    ctx_fn: TCtxFn,
    req: HttpRequest,
    router: Arc<rspc::Router<TCtx>>,
    data: web::Data<TCtx>,
    _marker: Option<PhantomData<TCtxFnMarker>>,
    tx: mpsc::Sender<jsonrpc::Response>,
    _rx: mpsc::Receiver<jsonrpc::Response>,
    subscriptions: HashMap<RequestId, oneshot::Sender<()>>,
}

impl<TCtx, TCtxFn, TCtxFnMarker> WebSocket<TCtx, TCtxFn, TCtxFnMarker>
where
    TCtx: Send + Sync + 'static,
    TCtxFn: TCtxFunc<TCtx, TCtxFnMarker> + Unpin,
    TCtxFnMarker: Unpin + Send + Sync + 'static,
{
    pub fn new(
        ctx_fn: TCtxFn,
        req: &HttpRequest,
        router: &Arc<rspc::Router<TCtx>>,
        data: &web::Data<TCtx>,
    ) -> Self {
        let (tx, _rx) = mpsc::channel::<jsonrpc::Response>(100);

        Self {
            ctx_fn,
            req: req.clone(),
            router: Arc::clone(router),
            data: data.clone(),
            _marker: None,
            tx,
            _rx,
            subscriptions: HashMap::new(),
        }
    }
}

impl<TCtx, TCtxFn, TCtxFnMarker> Actor for WebSocket<TCtx, TCtxFn, TCtxFnMarker>
where
    TCtx: Send + Sync + 'static,
    TCtxFn: TCtxFunc<TCtx, TCtxFnMarker> + Unpin,
    TCtxFnMarker: Unpin + Send + Sync + 'static,
{
    type Context = ws::WebsocketContext<Self>;
}

impl<TCtx, TCtxFn, TCtxFnMarker> StreamHandler<Result<ws::Message, ws::ProtocolError>>
    for WebSocket<TCtx, TCtxFn, TCtxFnMarker>
where
    TCtx: Send + Sync + 'static,
    TCtxFn: TCtxFunc<TCtx, TCtxFnMarker> + Unpin,
    TCtxFnMarker: Unpin + Send + Sync + 'static,
{
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(msg) => {
                let res = match msg {
                    ws::Message::Text(text) => serde_json::from_str::<Value>(&text),
                    ws::Message::Binary(binary) => serde_json::from_slice(&binary),
                    ws::Message::Ping(_) | ws::Message::Pong(_) | ws::Message::Close(_) => {
                        ctx.close(None);

                        return;
                    }
                    _ => {
                        ctx.close(None);

                        return;
                    }
                };

                match res.and_then(|v| match v.is_array() {
                    true => serde_json::from_value::<Vec<jsonrpc::Request>>(v),
                    false => serde_json::from_value::<jsonrpc::Request>(v).map(|v| vec![v]),
                }) {
                    Ok(reqs) => {
                        for request in reqs {
                            let execution_context = match self.ctx_fn.exec(&self.req.clone(), Some(&self.data)) {
                                Ok(ctx) => ctx,
                                Err(_err) => {
                                    #[cfg(feature = "tracing")]
                                    tracing::error!("Error executing context function: {}", _err);

                                    ctx.close(None);
                                    return;
                                }
                            };

                            block_on(handle_json_rpc(
                                execution_context,
                                request,
                                &self.router,
                                &mut Sender::Channel(&mut self.tx),
                                &mut jsonrpc::SubscriptionMap::Ref(&mut self.subscriptions),
                            ));
                        }
                    }
                    Err(_err) => {
                        #[cfg(feature = "tracing")]
                        tracing::error!("Error parsing websocket message: {}", _err);
                    }
                }
            }
            Err(_err) => {
                #[cfg(feature = "tracing")]
                tracing::error!("Error in websocket: {}", _err);

                ctx.close(None);
                return;
            }
        }

        if let Some(f) = block_on(self._rx.recv()) {
            ctx.text(serde_json::to_string(&f).unwrap());
        }

    }
}
