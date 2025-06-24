use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

use axum::{
    extract::{Path, Query, State},
    http::{
        header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE},
        Method, StatusCode,
    },
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use axum_extra::{
    headers::{authorization::Bearer, Authorization},
    TypedHeader,
};
use dotenv::dotenv;
use mysql::{params, prelude::Queryable, Params, Pool};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};
use tracing::{error, info, trace};
use util::{get_token, resolve_timestamp};

mod util;

pub struct ServiceState {
    db: Pool,
}

#[derive(Serialize, Deserialize)]
struct ChatlogEntry {
    round_id: i32,
    text_raw: String,
    msg_type: Option<String>,
    created_at: i64,
}

#[tokio::main]
async fn main() {
    let subscriber = tracing_subscriber::FmtSubscriber::new();
    tracing::subscriber::set_global_default(subscriber).unwrap();
    dotenv().ok();

    let config_path =
        std::env::var("DBCONFIG_PATH").expect("DBCONFIG_PATH Environment Variable has to be set");
    let dbconfig = File::open(config_path.clone()).unwrap_or_else(|e| {
        error!("Error while trying to read database configuration: {e}");
        std::process::exit(1);
    });

    let reader = BufReader::new(dbconfig);

    let mut config_map: HashMap<String, String> = HashMap::default();

    let re = Regex::new("^([A-Z_]+) (.*?)$").unwrap_or_else(|e| {
                    error!("Error while setting up regex: {e}");
                    std::process::exit(1);
                });
    for line in reader.lines() {
        match line {
            Ok(line) => {
                match re.captures(&line).ok_or("no match") {
                    Ok(caps) => {
                        let key = caps.get(1).unwrap().as_str();
                        let val = caps.get(2).unwrap().as_str();

                        config_map.insert(key.to_owned(), val.to_owned());
                    }
                    Err(e) => trace!("Match error: {e}"),
                }
            }
            Err(e) => error!("Error while leading line from {config_path}: {e}"),
        }
    }

    let db_user = config_map.get("FEEDBACK_LOGIN").unwrap();
    let db_pass = config_map.get("FEEDBACK_PASSWORD").unwrap();
    let db_host = format!(
        "{}:{}",
        config_map.get("ADDRESS").unwrap(),
        config_map.get("PORT").unwrap()
    );
    let db_database = config_map.get("FEEDBACK_DATABASE").unwrap();

    let url = format!("mysql://{db_user}:{db_pass}@{db_host}/{db_database}");

    let pool = match Pool::new(url.as_str()) {
        Ok(pool) => {
            info!("Connected to the database");
            pool
        }
        Err(err) => {
            error!("Failed to connect to the database: {:?}", err);
            std::process::exit(1);
        }
    };

    let cors = CorsLayer::new()
        .allow_methods([Method::GET])
        .allow_origin(Any)
        .allow_headers([ACCEPT, AUTHORIZATION, CONTENT_TYPE]);

    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(2)
            .burst_size(2)
            .finish()
            .unwrap(),
    );

    let governor_limiter = governor_conf.limiter().clone();
    let interval = Duration::from_secs(60);
    std::thread::spawn(move || loop {
        std::thread::sleep(interval);
        //info!("rate limiting storage size: {}", governor_limiter.len());
        governor_limiter.retain_recent();
    });

    let app = Router::new()
        .route("/api/healthcheck", get(health_check_handler))
        .route("/api/logs/{ckey}/{length}", get(read_logs_handler))
        .route("/api/export/{ckey}/{length}", get(export_logs_handler))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .layer(GovernorLayer {
            config: governor_conf,
        })
        .with_state(Arc::new(ServiceState { db: pool.clone() }));

    let api_host = std::env::var("API_HOST").expect("API_HOST must be set");
    let api_port = std::env::var("API_PORT").expect("API_PORT must be set");
    info!("App setup complete, listening to {api_host}:{api_port}");

    let listener = tokio::net::TcpListener::bind(format!("{api_host}:{api_port}"))
        .await
        .unwrap();
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}

async fn health_check_handler() -> impl IntoResponse {
    const MESSAGE: &str = "API Services";

    let response = serde_json::json!({
        "status": "ok",
        "message": MESSAGE
    });

    Json(response)
}

async fn read_logs_handler(
    TypedHeader(authorization): TypedHeader<Authorization<Bearer>>,
    State(state): State<Arc<ServiceState>>,
    Path((ckey, length)): Path<(String, i32)>,
) -> impl IntoResponse {
    let mut conn = state.db.get_conn().unwrap();

    let token = match get_token(state, ckey.clone()) {
        Some(token) => token,
        None => "".to_owned(),
    };

    if token != authorization.token() {
        return (StatusCode::UNAUTHORIZED).into_response();
    }

    let query = "SELECT round_id, text_raw, type, created_at FROM chatlogs_logs WHERE target = :ckey ORDER BY ID DESC LIMIT :length";

    let mut results = conn
        .exec_map(
            query,
            params! {
                "ckey" => ckey,
                "length" => if length > 100000 { 100000 } else { length }
            },
            |(round_id, text_raw, msg_type, created_at)| ChatlogEntry {
                round_id,
                text_raw,
                msg_type,
                created_at,
            },
        )
        .map_err(|e| {
            eprint!("read_logs_handler Error: {e}");
            let error_response = serde_json::json!({
                "status": "error",
                "message": format!("Error while trying to get chatlogs: {e}"),
            });
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response))
        })
        .unwrap();

    results.reverse();
    Json(results).into_response()
}

#[derive(Deserialize)]
struct ExportParams {
    start_id: Option<i32>,
    end_id: Option<i32>,
    timezone_offset: Option<i32>,
}

async fn export_logs_handler(
    TypedHeader(authorization): TypedHeader<Authorization<Bearer>>,
    State(state): State<Arc<ServiceState>>,
    Path((ckey, length)): Path<(String, i32)>,
    Query(export_params): Query<ExportParams>,
) -> impl IntoResponse {
    let mut conn = state.db.get_conn().unwrap();

    let token = match get_token(state, ckey.clone()) {
        Some(token) => token,
        None => "".to_owned(),
    };

    if token != authorization.token() {
        return (StatusCode::UNAUTHORIZED).into_response();
    }

    let query: &str;
    let params: Params;
    if export_params.start_id.is_some() && export_params.end_id.is_some() {
        query = "SELECT round_id, text_raw, type, created_at FROM chatlogs_logs WHERE round_id BETWEEN :start_round AND :end_round AND target = :ckey ORDER BY ID DESC LIMIT 100000";
        params = params! {
            "start_round" => export_params.start_id,
            "end_round" => export_params.end_id,
            "ckey" => ckey
        };
    } else {
        query = "SELECT round_id, text_raw, type, created_at FROM chatlogs_logs WHERE round_id = :round AND target = :ckey ORDER BY ID DESC LIMIT :length";
        params = params! {
            "round" => export_params.start_id,
            "ckey" => ckey,
            "length" => if length == 0 || length > 100000 { 100000 } else { length }
        };
    }

    let mut results = conn
        .exec_map(
            query,
            params,
            |(round_id, text_raw, msg_type, created_at)| ChatlogEntry {
                round_id,
                text_raw: if export_params.timezone_offset.is_some() {
                    format!(
                        "[{}] {}",
                        resolve_timestamp(created_at, export_params.timezone_offset.unwrap()),
                        text_raw
                    )
                } else {
                    text_raw
                },
                msg_type,
                created_at,
            },
        )
        .map_err(|e| {
            let error_response = serde_json::json!({
                "status": "error",
                "message": format!("Error while trying to get chatlogs: {e}"),
            });
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response))
        })
        .unwrap();

    results.reverse();

    Json(results).into_response()
}
