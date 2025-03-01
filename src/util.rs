use std::sync::Arc;

use axum::{http::StatusCode, Json};
use chrono::DateTime;
use mysql::{params, prelude::Queryable};

use crate::ServiceState;

pub fn get_token(state: Arc<ServiceState>, ckey: String) -> Option<String> {
    let mut conn = state.db.get_conn().unwrap();

    let auth_query = "SELECT token FROM chatlogs_ckeys WHERE ckey = :ckey";
    conn.exec_first(
        auth_query,
        params! {
            "ckey" => ckey.clone()
        },
    )
    .map_err(|e| {
        eprint!("get_token Error: {e}");
        let error_response = serde_json::json!({
            "status": "error",
            "message": format!("Error while trying to get chatlogs: {e}"),
        });
        (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response))
    })
    .unwrap()
}

pub fn resolve_timestamp(timestamp: i64, timezone_offset: i32) -> String {
    let dt =
        DateTime::from_timestamp_millis(timestamp + i64::from(timezone_offset * 60 * 60 * 1000))
            .unwrap();

    dt.format("%H:%M").to_string()
}
