use anyhow::{Error, Result};
use chrono::offset::Local;
use colored::Colorize;
use envfile::EnvFile;
use hyper::server::conn::AddrStream;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request as HyperRequest, Response as HyperResponse, Server};
use lagon_runtime::{options::RuntimeOptions, Runtime};
use lagon_runtime_http::{Request, Response, RunResult, X_FORWARDED_FOR, X_LAGON_REGION};
use lagon_runtime_isolate::{options::IsolateOptions, Isolate};
use lagon_runtime_isolate::{IsolateEvent, IsolateRequest};
use lagon_runtime_utils::assets::{find_asset, handle_asset};
use lagon_runtime_utils::response::{handle_response, ResponseEvent, FAVICON_URL};
use log::{
    set_boxed_logger, set_max_level, Level, LevelFilter, Log, Metadata, Record, SetLoggerError,
};
use notify::event::ModifyKind;
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::Mutex;

use crate::utils::{bundle_function, error, info, input, resolve_path, success, warn, Assets};

const LOCAL_REGION: &str = "local";

struct SimpleLogger;

impl Log for SimpleLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let level = match record.level() {
                Level::Error => "ERROR".red(),
                Level::Warn => "WARN".yellow(),
                _ => "INFO".blue(),
            };

            println!("{} {}", level, record.args());
        }
    }

    fn flush(&self) {}
}

fn init_logger() -> Result<(), SetLoggerError> {
    set_boxed_logger(Box::new(SimpleLogger)).map(|()| set_max_level(LevelFilter::Info))?;
    Ok(())
}

fn parse_environment_variables(
    root: &Path,
    env: Option<PathBuf>,
) -> Result<HashMap<String, String>> {
    let mut environment_variables = HashMap::new();

    if let Some(path) = env {
        let envfile = EnvFile::new(root.join(path))?;

        for (key, value) in envfile.store {
            environment_variables.insert(key, value);
        }
    }

    Ok(environment_variables)
}

// This function is similar to packages/serverless/src/main.rs,
// except that we don't have multiple deployments and such multiple
// threads to manage, and we don't manager logs and metrics.
async fn handle_request(
    req: HyperRequest<Body>,
    public_dir: Option<PathBuf>,
    ip: String,
    assets: Arc<Mutex<Assets>>,
    isolate_tx: flume::Sender<IsolateEvent>,
) -> Result<HyperResponse<Body>> {
    let url = req.uri().path();

    println!(
        "{} {} {}",
        format!("{}", Local::now().time()).bright_black(),
        req.method().to_string().blue(),
        url
    );

    let (tx, rx) = flume::unbounded();
    let assets = assets.lock().await.to_owned();

    let is_favicon = url == FAVICON_URL;

    if let Some(asset) = find_asset(url, &assets.keys().cloned().collect()) {
        println!("              {}", input("Asset found"));

        let run_result = match handle_asset(public_dir.unwrap(), asset) {
            Ok(response) => RunResult::Response(response),
            Err(error) => RunResult::Error(format!("Could not retrieve asset ({asset}): {error}")),
        };

        tx.send_async(run_result).await.unwrap_or(());
    } else if is_favicon {
        tx.send_async(RunResult::Response(Response {
            status: 404,
            ..Default::default()
        }))
        .await
        .unwrap_or(());
    } else {
        match Request::from_hyper(req).await {
            Ok(mut request) => {
                request.set_header(X_FORWARDED_FOR.to_string(), ip);
                request.set_header(X_LAGON_REGION.to_string(), LOCAL_REGION.to_string());

                isolate_tx
                    .send_async(IsolateEvent::Request(IsolateRequest {
                        request,
                        sender: tx,
                    }))
                    .await
                    .unwrap_or(());
            }
            Err(error) => {
                println!("Error while parsing request: {error}");

                tx.send_async(RunResult::Error("Error while parsing request".into()))
                    .await
                    .unwrap_or(());
            }
        };
    }

    handle_response(
        rx,
        (),
        Box::new(|event, _| match event {
            ResponseEvent::StreamDoneNoDataError => {
                println!(
                    "{}",
                    error("The stream was done before sending a response/data")
                );
            }
            ResponseEvent::StreamDoneDataError => {
                println!("{}", error("Got data after stream was done"));
            }
            ResponseEvent::UnexpectedStreamResult(result) => {
                println!("{} {:?}", error("Unexpected stream result:"), result);
            }
            ResponseEvent::LimitsReached(result) => {
                if result == RunResult::Timeout {
                    println!("{}", error("Function execution timed out"));
                } else {
                    println!("{}", error("Function execution reached memory limit"));
                }
            }
            ResponseEvent::Error(result) => {
                println!("{}", error(result.as_error().as_str()));
            }
            _ => {}
        }),
    )
    .await
}

pub async fn dev(
    path: Option<PathBuf>,
    client: Option<PathBuf>,
    public_dir: Option<PathBuf>,
    port: Option<u16>,
    hostname: Option<String>,
    env: Option<PathBuf>,
    allow_code_generation: bool,
) -> Result<()> {
    let (root, function_config) = resolve_path(path, client, public_dir)?;
    let (index, assets) = bundle_function(&function_config, &root)?;

    let server_index = index.clone();
    let assets = Arc::new(Mutex::new(assets));

    let runtime =
        Runtime::new(RuntimeOptions::default().allow_code_generation(allow_code_generation));
    let addr = format!(
        "{}:{}",
        hostname.unwrap_or_else(|| "127.0.0.1".into()),
        port.unwrap_or(1234)
    )
    .parse()?;

    let server_public_dir = function_config
        .assets
        .as_ref()
        .map(|assets| root.join(assets));
    let environment_variables = parse_environment_variables(&root, env)?;

    let (tx, rx) = flume::unbounded();
    let (index_tx, index_rx) = flume::unbounded();
    let handle = Handle::current();

    std::thread::spawn(move || {
        handle.block_on(async move {
            let mut index = server_index;

            loop {
                let mut isolate = Isolate::new(
                    IsolateOptions::new(
                        String::from_utf8(index.clone()).expect("Code is not UTF-8"),
                    )
                    .timeout(Duration::from_secs(1))
                    .startup_timeout(Duration::from_secs(2))
                    .metadata(Some((String::from(""), String::from(""))))
                    .environment_variables(environment_variables.clone()),
                    rx.clone(),
                );

                isolate.evaluate();

                tokio::select! {
                    _ = isolate.run_event_loop() => {},
                    new_index = index_rx.recv_async() => {
                        index = new_index.unwrap();
                    }
                }
            }
        });
    });

    let server_assets = Arc::clone(&assets);
    let server = Server::bind(&addr).serve(make_service_fn(move |conn: &AddrStream| {
        let public_dir = server_public_dir.clone();
        let assets = Arc::clone(&server_assets);
        let tx = tx.clone();

        let addr = conn.remote_addr();
        let ip = addr.ip().to_string();

        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                handle_request(
                    req,
                    public_dir.clone(),
                    ip.clone(),
                    Arc::clone(&assets),
                    tx.clone(),
                )
            }))
        }
    }));

    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = RecommendedWatcher::new(
        tx,
        Config::default().with_poll_interval(Duration::from_secs(1)),
    )?;

    watcher.watch(
        &root.join(function_config.index.clone()),
        RecursiveMode::NonRecursive,
    )?;

    tokio::spawn(async move {
        for event in rx.into_iter().flatten() {
            let should_update = if let EventKind::Modify(modify) = event.kind {
                matches!(modify, ModifyKind::Name(_)) || matches!(modify, ModifyKind::Data(_))
            } else {
                false
            };

            if should_update {
                // Clear the screen and put the cursor at first row & first col of the screen.
                print!("\x1B[2J\x1B[1;1H");
                println!("{}", info("Found change, updating..."));

                let (new_index, new_assets) = bundle_function(&function_config, &root)?;

                *assets.lock().await = new_assets;
                index_tx.send_async(new_index).await.unwrap();
            }
        }

        Ok::<(), Error>(())
    });

    println!();
    println!("{}", success("Dev Server started!"));

    if allow_code_generation {
        println!(
            "{}",
            warn("Code generation is allowed due to `--allow-code-generation`")
        );
    }

    println!();
    println!(
        " {} {}",
        "➤".bright_black(),
        format!("http://{addr}").blue()
    );

    init_logger()?;
    server.await?;
    runtime.dispose();

    Ok(())
}
