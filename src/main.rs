use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use dotenv::dotenv;
use mysql::{params, prelude::Queryable, Pool};
use serde::{Deserialize, Serialize};
use tower_http::trace::TraceLayer;
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

#[derive(Deserialize)]
struct ReadChatlog {
    ckey: String,
    length: i32
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

    let app = Router::new()
        .route("/api/healthcheck", get(health_check_handler))
        .route("/api/logs", get(read_logs_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(Arc::new(ServiceState { db: pool.clone() }));

    info!("App setup complete, listening to 0.0.0.0:50000");

    let listener = tokio::net::TcpListener::bind("0.0.0.0:50000").await.unwrap();
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
    State(state): State<Arc<ServiceState>>,
    Json(payload): Json<ReadChatlog>
) -> impl IntoResponse {
    let mut conn = state.db.get_conn().unwrap();
    let query = "SELECT round_id, text_raw, type, created_at FROM chatlogs WHERE target = :ckey ORDER BY ID ASC LIMIT :length";

    let results = conn.exec_map(query,
        params! {
            "ckey" => payload.ckey,
            "length" => payload.length
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

    Json(results)
}