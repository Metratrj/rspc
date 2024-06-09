use std::{env, net::TcpListener, path::PathBuf, time::Duration};

use actix_web::{middleware, web, App, HttpResponse, HttpServer};
use async_stream::stream;
use rspc::Config;
use tokio::time::sleep;
struct Ctx {}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    println!("Manifest dir: {}", manifest_dir);

    let router = rspc::Router::<Ctx>::new()
        .config(
            Config::new().export_ts_bindings(PathBuf::from(manifest_dir).join("../bindings.ts")),
        )
        .query("version", |t| t(|_, _: ()| env!("CARGO_PKG_VERSION")))
        .query("echo", |t| t(|_, v: String| v))
        .query("error", |t| {
            t(|_, _: ()| {
                Err(rspc::Error::new(
                    rspc::ErrorCode::InternalServerError,
                    "Something went wrong".into(),
                )) as Result<String, rspc::Error>
            })
        })
        .query("transformMe", |t| t(|_, _: ()| "Hello, world!".to_string()))
        .mutation("sendMsg", |t| {
            t(|_, v: String| {
                println!("Client said '{}'", v);
                v
            })
        })
        // TODO: Need to fix implement of subscriptions
        .subscription("pings", |t| {
            t(|_ctx, _args: ()| {
                stream! {
                    println!("Client subscribed to 'pings'");
                    for i in 0..5 {
                        println!("Sending ping {}", i);
                        yield "ping".to_string();
                        sleep(Duration::from_secs(1)).await;
                    }
                }
            })
        })
        .build()
        .arced();

    let data = web::Data::new(Ctx {});

    let addr = "[::]:4000".parse::<std::net::SocketAddr>().unwrap();
    let tcp_listener = TcpListener::bind(addr).expect("Failed to bind address");

    env_logger::init_from_env(env_logger::Env::default().default_filter_or("debug"));
    env::set_var("RUST_BACKTRACE", "1");
    env::set_var("RUST_LOG", "actix_web=debug");

    let app = HttpServer::new(move || {
        App::new()
            .app_data(data.clone())
            .wrap(middleware::Logger::default())
            .default_service(web::route().to(|| async {
                return HttpResponse::NotFound().finish();
            }))
            .service(
                web::scope("/rspc").configure(rspc_actix::configure(router.clone(), || Ctx {})),
            )
    })
    .listen(tcp_listener)?
    .run();

    app.await?;

    Ok(())
}
