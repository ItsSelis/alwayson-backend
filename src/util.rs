use std::sync::Arc;

use axum_extra::headers::Authorization;
use axum_extra::headers::authorization::Bearer;
use chrono::DateTime;
use mysql::{params, prelude::Queryable};
use tracing::error;
use crate::ServiceState;

pub fn get_tokens(state: Arc<ServiceState>, ckey: String) -> Vec<String> {
    let mut conn = state.db.get_conn().unwrap();

    let auth_query = "SELECT token FROM chatlogs_ckeys WHERE ckey = :ckey LIMIT 2";
    conn.exec(
        auth_query,
        params! {
            "ckey" => ckey.clone()
        }
    ).unwrap_or_else(|e| {
        let error_response = serde_json::json!({
            "status": "error",
            "message": format!("Error while trying to get chatlogs: {e}"),
        });
        error!("{}", error_response);
        vec![]
    })
}

pub fn validate_tokens(state: Arc<ServiceState>, ckey: String, authorization: Authorization<Bearer>) -> bool {
    if authorization.token().is_empty() {
        return false;
    }

    let tokens = get_tokens(state, ckey.clone());

    if tokens.is_empty() {
        return false;
    }

    if !tokens.iter().any(|t| t == authorization.token()) {
        return false;
    }

    true
}

pub fn resolve_timestamp(timestamp: i64, timezone_offset: i32) -> String {
    let dt =
        DateTime::from_timestamp_millis(timestamp + i64::from(timezone_offset * 60 * 60 * 1000))
            .unwrap();

    dt.format("%H:%M").to_string()
}
