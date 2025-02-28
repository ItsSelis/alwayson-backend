use std::sync::Arc;

use axum::{extract::{Path, State}, http::{header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE}, Method, StatusCode}, response::IntoResponse, routing::get, Json, Router};
use axum_extra::{headers::{authorization::Bearer, Authorization}, TypedHeader};
use dotenv::dotenv;
use mysql::{params, prelude::Queryable, Pool};
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
    created_at: u128
}

/*
#[derive(Deserialize)]
struct ReadChatlog {
    ckey: String,
    length: i32
}
*/

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

    let auth_query = "SELECT token FROM ckeys WHERE ckey = :ckey";
    let result: Option<String> = conn.exec_first(auth_query, params!{
        "ckey" => ckey.clone()
    }).map_err(|e| {
        let error_response = serde_json::json!({
            "status": "error",
            "message": format!("Error while trying to get chatlogs: {e}"),
        });
        (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response))
    }).unwrap();

    let token = match result {
        Some(token) => token,
        None => "".to_string()
    };

    if token != authorization.token() {
        return (StatusCode::UNAUTHORIZED).into_response();
    }

    let query = "SELECT round_id, text_raw, type, created_at FROM chatlogs WHERE target = :ckey ORDER BY ID ASC LIMIT :length";

    let results = conn.exec_map(query,
        params! {
            "ckey" => ckey,
            "length" => length
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
        let error_response = serde_json::json!({
            "status": "error",
            "message": format!("Error while trying to get chatlogs: {e}"),
        });
        (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response))
    }).unwrap();

    Json(results).into_response()
}