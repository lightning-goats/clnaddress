use anyhow::anyhow;
use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use cln_rpc::{
    model::requests::InvoiceRequest,
    primitives::{Amount, AmountOrAny},
};
use nostr::{
    event::{Event, Kind},
    filter::SingleLetterTag,
    nips::nip57::Nip57Tag,
};
use serde_json::json;
use uuid::Uuid;

use crate::structs::{
    InvoiceQueryParams, LnurlpCallback, LnurlpConfig, PluginState, UserMetadata,
};

const INVOICE_LABEL_PREFIX: &str = "clnaddress:v1";
const DIRECT_INVOICE_USER: &str = "__direct__";

pub async fn get_lnurlp_config(
    maybe_user: Option<axum::extract::Path<String>>,
    State(state): State<PluginState>,
) -> Result<Json<LnurlpConfig>, axum::response::Response> {
    if let Some(axum::extract::Path(user)) = maybe_user {
        let user_meta = get_user_metadata(&state, &user)
            .map_err(|e| (StatusCode::NOT_FOUND, lnurl_error(&e.to_string())).into_response())?;
        let metadata = generate_user_metadata(&state, &user, &user_meta);
        let allows_nostr = user_meta.allows_nostr(&state);

        Ok(Json(LnurlpConfig {
            callback: state
                .base_url
                .join("invoice/")
                .unwrap()
                .join(&user)
                .unwrap()
                .to_string(),
            max_sendable: user_meta.effective_max_sendable_msat(&state),
            min_sendable: user_meta.effective_min_sendable_msat(&state),
            metadata: serde_json::to_string(&metadata).unwrap(),
            tag: "payRequest".to_owned(),
            comment_allowed: user_meta.comment_allowed,
            allows_nostr,
            nostr_pubkey: allows_nostr.then(|| {
                state
                    .nostr_zapper_keys
                    .as_ref()
                    .expect("allows_nostr implies configured zapper keys")
                    .public_key()
                    .to_hex()
            }),
        }))
    } else {
        Ok(Json(LnurlpConfig {
            callback: state.base_url.join("invoice").unwrap().to_string(),
            max_sendable: state.max_sendable_msat,
            min_sendable: state.min_sendable_msat,
            metadata: serde_json::to_string(&vec![vec![
                "text/plain".to_string(),
                state.default_description,
            ]])
            .unwrap(),
            tag: "payRequest".to_owned(),
            comment_allowed: None,
            allows_nostr: state.nostr_zapper_keys.is_some(),
            nostr_pubkey: state
                .nostr_zapper_keys
                .as_ref()
                .map(|keys| keys.public_key().to_hex()),
        }))
    }
}

pub async fn get_invoice(
    maybe_user: Option<axum::extract::Path<String>>,
    Query(params): Query<InvoiceQueryParams>,
    State(state): State<PluginState>,
) -> Result<Json<LnurlpCallback>, axum::response::Response> {
    let user = maybe_user.map(|axum::extract::Path(user)| user);
    let user_meta = match user.as_deref() {
        Some(user) => Some(get_user_metadata(&state, user).map_err(|e| {
            (StatusCode::NOT_FOUND, lnurl_error(&e.to_string())).into_response()
        })?),
        None => None,
    };

    let min_sendable_msat = user_meta
        .as_ref()
        .map(|metadata| metadata.effective_min_sendable_msat(&state))
        .unwrap_or(state.min_sendable_msat);
    let max_sendable_msat = user_meta
        .as_ref()
        .map(|metadata| metadata.effective_max_sendable_msat(&state))
        .unwrap_or(state.max_sendable_msat);

    validate_invoice_amount(params.amount, min_sendable_msat, max_sendable_msat)
        .map_err(axum::response::IntoResponse::into_response)?;
    validate_comment(
        params.comment.as_deref(),
        user_meta
            .as_ref()
            .and_then(|metadata| metadata.comment_allowed)
            .unwrap_or(0),
    )
    .map_err(axum::response::IntoResponse::into_response)?;

    let nostr_allowed = user_meta
        .as_ref()
        .map(|metadata| metadata.allows_nostr(&state))
        .unwrap_or_else(|| state.nostr_zapper_keys.is_some());

    let description = match &params.nostr {
        Some(raw_zap_request) => {
            if !nostr_allowed {
                return Err((
                    StatusCode::BAD_REQUEST,
                    lnurl_error("Nostr Zaps are disabled for this Lightning Address"),
                )
                    .into_response());
            }
            let zapper_keys = state.nostr_zapper_keys.as_ref().ok_or_else(|| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    lnurl_error("Nostr Zaps not configured"),
                )
                    .into_response()
            })?;
            let zap_request: Event = Event::from_json(raw_zap_request).map_err(|e| {
                (StatusCode::BAD_REQUEST, lnurl_error(&e.to_string())).into_response()
            })?;
            zap_request.verify().map_err(|e| {
                (StatusCode::BAD_REQUEST, lnurl_error(&e.to_string())).into_response()
            })?;
            let zap_request_json = zap_request.try_as_json().map_err(|e| {
                (StatusCode::BAD_REQUEST, lnurl_error(&e.to_string())).into_response()
            })?;
            verify_zap_request(&zap_request, params.amount, zapper_keys).map_err(|e| {
                (StatusCode::BAD_REQUEST, lnurl_error(&e.to_string())).into_response()
            })?;
            zap_request_json
        }
        None => match (user.as_deref(), user_meta.as_ref()) {
            (Some(user), Some(metadata)) => {
                serde_json::to_string(&generate_user_metadata(&state, user, metadata)).unwrap()
            }
            _ => serde_json::to_string(&vec![vec![
                "text/plain".to_string(),
                state.default_description.clone(),
            ]])
            .unwrap(),
        },
    };

    let mut cln_client = cln_rpc::ClnRpc::new(&state.rpc_path).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            lnurl_error(&e.to_string()),
        )
            .into_response()
    })?;

    let amount_msat = if params.amount > 0 {
        AmountOrAny::Amount(Amount::from_msat(params.amount))
    } else {
        AmountOrAny::Any
    };

    let cln_response = cln_client
        .call_typed(&InvoiceRequest {
            amount_msat,
            description,
            label: invoice_label(user.as_deref()),
            expiry: None,
            fallbacks: None,
            preimage: None,
            exposeprivatechannels: None,
            cltv: None,
            deschashonly: Some(true),
        })
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                lnurl_error(&e.to_string()),
            )
                .into_response()
        })?;

    Ok(Json(LnurlpCallback {
        pr: cln_response.bolt11,
        routes: vec![],
    }))
}

fn get_user_metadata(state: &PluginState, user: &str) -> Result<UserMetadata, anyhow::Error> {
    let users = state.users.lock();
    users
        .get(user)
        .cloned()
        .ok_or_else(|| anyhow!("User `{user}` not found!"))
}

fn invoice_label(user: Option<&str>) -> String {
    let user = user.unwrap_or(DIRECT_INVOICE_USER);
    format!("{INVOICE_LABEL_PREFIX}:{user}:{}", Uuid::new_v4())
}

fn validate_invoice_amount(
    requested_amount: u64,
    min_sendable_msat: u64,
    max_sendable_msat: u64,
) -> Result<(), (axum::http::StatusCode, axum::Json<serde_json::Value>)> {
    if requested_amount < min_sendable_msat {
        return Err((
            StatusCode::BAD_REQUEST,
            lnurl_error(&format!(
                "`amount` below minimum: {requested_amount}<{min_sendable_msat}",
            )),
        ));
    }
    if requested_amount > max_sendable_msat {
        return Err((
            StatusCode::BAD_REQUEST,
            lnurl_error(&format!(
                "`amount` above maximum: {requested_amount}>{max_sendable_msat}",
            )),
        ));
    }
    Ok(())
}

fn validate_comment(
    comment: Option<&str>,
    comment_allowed: u64,
) -> Result<(), (axum::http::StatusCode, axum::Json<serde_json::Value>)> {
    let Some(comment) = comment else {
        return Ok(());
    };
    let length = comment.chars().count() as u64;
    if length > comment_allowed {
        return Err((
            StatusCode::BAD_REQUEST,
            lnurl_error(&format!(
                "`comment` exceeds maximum length: {length}>{comment_allowed}",
            )),
        ));
    }
    Ok(())
}

fn generate_user_metadata(
    state: &PluginState,
    user: &str,
    user_meta: &UserMetadata,
) -> Vec<Vec<String>> {
    let mut metadata = if let Some(user_desc) = &user_meta.description {
        vec![vec!["text/plain".to_owned(), user_desc.to_owned()]]
    } else {
        vec![vec![
            "text/plain".to_owned(),
            state.default_description.clone(),
        ]]
    };

    let port = state
        .base_url
        .port()
        .map(|p| format!(":{p}"))
        .unwrap_or_default();

    if user_meta.is_email.unwrap_or(false) {
        metadata.push(vec![
            "text/email".to_owned(),
            format!("{}@{}{}", user, state.base_url.host_str().unwrap(), port),
        ]);
    } else {
        metadata.push(vec![
            "text/identifier".to_owned(),
            format!("{}@{}{}", user, state.base_url.host_str().unwrap(), port),
        ]);
    }
    log::debug!("metadata generated for user {user}");
    metadata
}

fn lnurl_error(error: &str) -> Json<serde_json::Value> {
    log::debug!("lnurl_error: {error}");
    Json(json!({"status":"ERROR", "reason":error}))
}

pub fn verify_zap_request(
    event: &Event,
    amount: u64,
    nostr_zapper_keys: &nostr::key::Keys,
) -> Result<(), anyhow::Error> {
    if event.kind != Kind::ZapRequest {
        return Err(anyhow!("Zap request has wrong kind: {}", event.kind));
    }
    if event.tags.is_empty() {
        return Err(anyhow!("Zap request MUST have tags"));
    }

    let mut e_tag = false;
    let mut p_tag = false;
    let mut relays_tag = false;
    let mut big_p_tag = None;
    for tag in event.tags.as_slice() {
        if let Ok(nip57tag) = Nip57Tag::parse(tag.clone()) {
            match nip57tag {
                Nip57Tag::Amount {
                    millisats,
                    bolt11: _,
                } => {
                    if amount != millisats {
                        return Err(anyhow!(
                            "Zap request amount does not match query amount: {amount}!={millisats}"
                        ));
                    }
                }
                Nip57Tag::Relays(relays) if !relays.is_empty() => {
                    relays_tag = true;
                }
                _ => {}
            }
        }
        if let Some(single_letter_tag) = tag.single_letter_tag() {
            match single_letter_tag {
                SingleLetterTag::LOWERCASE_A => {
                    let coord = tag.content().ok_or(anyhow!("Missing value in `a` tag"))?;
                    let parts: Vec<&str> = coord.split(':').collect();
                    if parts.len() < 2 || parts.len() > 3 {
                        return Err(anyhow!("Invalid `a` tag format"));
                    }
                    let kind = parts[0]
                        .parse::<u16>()
                        .map_err(|_| anyhow!("Invalid kind"))?;
                    Kind::from_u16(kind);
                    nostr::key::PublicKey::from_hex(parts[1])
                        .map_err(|_| anyhow!("Invalid pubkey"))?;
                }
                SingleLetterTag::LOWERCASE_E => {
                    if e_tag {
                        return Err(anyhow!("Zap request MUST have 0 or 1 e tags"));
                    }
                    e_tag = true;
                }
                SingleLetterTag::LOWERCASE_P => {
                    if p_tag {
                        return Err(anyhow!("Zap request MUST have only one p tag"));
                    }
                    p_tag = true;
                }
                SingleLetterTag::UPPERCASE_P => {
                    if big_p_tag.is_none() {
                        let key = tag.content().ok_or(anyhow!("Missing value in `P` tag"))?;
                        big_p_tag = Some(
                            nostr::key::PublicKey::from_hex(key)
                                .map_err(|_| anyhow!("Invalid pubkey"))?,
                        );
                    } else {
                        return Err(anyhow!("Zap request has too many `P` tags"));
                    }
                }
                _ => (),
            }
        }
    }

    if !p_tag {
        return Err(anyhow!("Zap request MUST have only one p tag"));
    }

    if let Some(big_p_tag) = big_p_tag {
        if big_p_tag != nostr_zapper_keys.public_key() {
            return Err(anyhow!("Zap request has wrong `P` tag"));
        }
    }

    if !relays_tag {
        log::info!("There should be a `relays` tag in the Zap request!");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_invoice_labels_are_namespaced_and_attributable() {
        let label = invoice_label(Some("herd"));
        let parts: Vec<&str> = label.split(':').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(&parts[..3], &["clnaddress", "v1", "herd"]);
        Uuid::parse_str(parts[3]).unwrap();
    }

    #[test]
    fn direct_invoice_labels_cannot_collide_with_valid_address_users() {
        let label = invoice_label(None);
        assert!(label.starts_with("clnaddress:v1:__direct__:"));
    }

    #[test]
    fn comment_length_is_counted_in_characters() {
        assert!(validate_comment(Some("goats"), 5).is_ok());
        assert!(validate_comment(Some("🐐🐐"), 2).is_ok());
        assert!(validate_comment(Some("🐐🐐"), 1).is_err());
        assert!(validate_comment(Some("x"), 0).is_err());
        assert!(validate_comment(Some(""), 0).is_ok());
    }
}
