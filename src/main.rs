use std::sync::Arc;

use axum::{extract::{Path, Query, State}, http::{header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE}, Method, StatusCode}, response::IntoResponse, routing::get, Json, Router};
use axum_extra::{headers::{authorization::Bearer, Authorization}, TypedHeader};
use chrono::DateTime;
use dotenv::dotenv;
use mysql::{params, prelude::Queryable, Params, Pool};
use serde::{Deserialize, Serialize};
use tower_http::{cors::{Any, CorsLayer}, trace::TraceLayer};
use tracing::{error, info};

pub struct ServiceState {
    db: Pool,
}

#[derive(Serialize, Deserialize)]
struct ChatlogEntry {
    round_id: i32,
    text_raw: String,
    msg_type: Option<String>,
    created_at: i64
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_max_level(tracing::Level::DEBUG).init();
    dotenv().ok();
    
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = match Pool::new(database_url.as_str()) {
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

    let app = Router::new()
        .route("/api/healthcheck", get(health_check_handler))
        .route("/api/logs/{ckey}/{length}", get(read_logs_handler))
        .route("/api/export/{ckey}/{length}", get(export_logs_handler))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(Arc::new(ServiceState { db: pool.clone() }));

    let api_host = std::env::var("API_HOST").expect("API_HOST must be set");
    let api_port = std::env::var("API_PORT").expect("API_PORT must be set");
    info!("App setup complete, listening to {api_host}:{api_port}");

    let listener = tokio::net::TcpListener::bind(format!("{api_host}:{api_port}")).await.unwrap();
    axum::serve(listener, app.into_make_service()).await.unwrap();
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
    Path((ckey, length)): Path<(String, i32)>
) -> impl IntoResponse {
    let mut conn = state.db.get_conn().unwrap();

    let token = match get_token(state, ckey.clone()) {
        Some(token) => token,
        None => "".to_string()
    };

    if token != authorization.token() {
        return (StatusCode::UNAUTHORIZED).into_response();
    }

    let query = "SELECT round_id, text_raw, type, created_at FROM chatlogs_logs WHERE target = :ckey ORDER BY ID DESC LIMIT :length";

    let mut results = conn.exec_map(query,
        params! {
            "ckey" => ckey,
            "length" => if length > 100000 { 100000 } else { length }
        },
        |(round_id, text_raw, msg_type, created_at)| { 
            ChatlogEntry { 
                round_id,
                text_raw,
                msg_type,
                created_at
            } 
        }
    ).map_err(|e| {
        eprint!("read_logs_handler Error: {e}");
        let error_response = serde_json::json!({
            "status": "error",
            "message": format!("Error while trying to get chatlogs: {e}"),
        });
        (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response))
    }).unwrap();

    results.reverse();
    Json(results).into_response()
}

#[derive(Deserialize)]
struct ExportParams {
    start_id: Option<i32>,
    end_id: Option<i32>,
    timezone_offset: Option<i32>
}

async fn export_logs_handler(
    TypedHeader(authorization): TypedHeader<Authorization<Bearer>>,
    State(state): State<Arc<ServiceState>>,
    Path((ckey, length)): Path<(String, i32)>,
    Query(export_params): Query<ExportParams>
) -> impl IntoResponse {
    let mut conn = state.db.get_conn().unwrap();

    let token = match get_token(state, ckey.clone()) {
        Some(token) => token,
        None => "".to_string()
    };

    if token != authorization.token() {
        return (StatusCode::UNAUTHORIZED).into_response();
    }

    let query: &str;
    let params: Params;
    if export_params.start_id.is_some() && export_params.end_id.is_some() {
        query = "SELECT round_id, text_raw, type, created_at FROM chatlogs_logs WHERE round_id BETWEEN :start_round AND :end_round AND target = :ckey ORDER BY ID DESC LIMIT 100000";
        params = params!{
            "start_round" => export_params.start_id,
            "end_round" => export_params.end_id,
            "ckey" => ckey
        };
    } else {
        query = "SELECT round_id, text_raw, type, created_at FROM chatlogs_logs WHERE round_id = :round AND target = :ckey ORDER BY ID DESC LIMIT :length";
        params = params!{
            "round" => export_params.start_id,
            "ckey" => ckey,
            "length" => if length == 0 || length > 100000 { 100000 } else { length }
        };
    }

    let mut results = conn.exec_map(query, params,
        |(round_id, text_raw, msg_type, created_at)| { 
            ChatlogEntry { 
                round_id,
                text_raw: if export_params.timezone_offset.is_some() { format!("[{}] {}", resolve_timestamp(created_at, export_params.timezone_offset.unwrap()), text_raw) } else { text_raw },
                msg_type,
                created_at
            } 
        }
    ).map_err(|e| {
        let error_response = serde_json::json!({
            "status": "error",
            "message": format!("Error while trying to get chatlogs: {e}"),
        });
        (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response))
    }).unwrap();

    results.reverse();

    /* 
    let export_text: String;
    if export_params.timezone_offset.is_some() {
        export_text = format!(
            "{}",
            results.iter()
                .map(|msg| format!("<div class=\"ChatMessage\">{} {}</div>", msg.created_at.to_string(), msg.text_raw))
                .collect::<Vec<String>>()
                .join("\n")
        );
    } else {
        export_text = format!(
            "{}",
            results.iter()
                .map(|msg| format!("<div class=\"ChatMessage\">{}</div>", msg.text_raw))
                .collect::<Vec<String>>()
                .join("\n")
        );
    }
    */

    Json(results).into_response()
}

fn get_token(state: Arc<ServiceState>, ckey: String) -> Option<String> {
    let mut conn = state.db.get_conn().unwrap();

    let auth_query = "SELECT token FROM chatlogs_ckeys WHERE ckey = :ckey";
    conn.exec_first(auth_query, params!{
        "ckey" => ckey.clone()
    }).map_err(|e| {
        eprint!("get_token Error: {e}");
        let error_response = serde_json::json!({
            "status": "error",
            "message": format!("Error while trying to get chatlogs: {e}"),
        });
        (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response))
    }).unwrap()
}

fn resolve_timestamp(timestamp: i64, timezone_offset: i32) -> String {
    let dt = DateTime::from_timestamp_millis(timestamp + i64::from(timezone_offset * 60 * 60 * 1000)).unwrap();

    dt.format("%H:%M").to_string()
}