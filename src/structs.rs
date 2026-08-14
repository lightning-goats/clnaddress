use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{Result, anyhow, bail};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use url::Url;

pub const MAX_USER_LENGTH: usize = 64;
pub const MAX_DESCRIPTION_LENGTH: usize = 512;
pub const MAX_COMMENT_ALLOWED: u64 = 512;

#[derive(Debug, Clone)]
pub struct PluginState {
    pub rpc_path: PathBuf,
    pub max_sendable_msat: u64,
    pub min_sendable_msat: u64,
    pub default_description: String,
    pub users: Arc<Mutex<HashMap<String, UserMetadata>>>,
    pub user_update_lock: Arc<tokio::sync::Mutex<()>>,
    pub plugin_dir: PathBuf,
    pub base_url: Url,
    pub nostr_zapper_keys: Option<nostr::key::Keys>,
    pub payindex: u64,
    pub listen_address: SocketAddr,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_email: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_sendable_msat: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sendable_msat: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_allowed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nostr_enabled: Option<bool>,
}

impl UserMetadata {
    pub fn effective_min_sendable_msat(&self, state: &PluginState) -> u64 {
        self.min_sendable_msat.unwrap_or(state.min_sendable_msat)
    }

    pub fn effective_max_sendable_msat(&self, state: &PluginState) -> u64 {
        self.max_sendable_msat.unwrap_or(state.max_sendable_msat)
    }

    pub fn allows_nostr(&self, state: &PluginState) -> bool {
        state.nostr_zapper_keys.is_some() && self.nostr_enabled.unwrap_or(true)
    }

    pub fn validate(&self, default_min_msat: u64, default_max_msat: u64) -> Result<()> {
        if let Some(description) = &self.description {
            if description.chars().count() > MAX_DESCRIPTION_LENGTH {
                bail!(
                    "description exceeds maximum length of {MAX_DESCRIPTION_LENGTH} characters"
                );
            }
        }

        if let Some(comment_allowed) = self.comment_allowed {
            if comment_allowed == 0 || comment_allowed > MAX_COMMENT_ALLOWED {
                bail!(
                    "comment_allowed must be between 1 and {MAX_COMMENT_ALLOWED} characters"
                );
            }
        }

        let min_sendable_msat = self.min_sendable_msat.unwrap_or(default_min_msat);
        let max_sendable_msat = self.max_sendable_msat.unwrap_or(default_max_msat);
        if min_sendable_msat > max_sendable_msat {
            bail!(
                "effective min_sendable_msat ({min_sendable_msat}) exceeds max_sendable_msat ({max_sendable_msat})"
            );
        }

        Ok(())
    }
}

pub fn validate_user(user: &str) -> Result<()> {
    if user.is_empty() || user.len() > MAX_USER_LENGTH {
        bail!("user must contain between 1 and {MAX_USER_LENGTH} ASCII characters");
    }
    if !user.is_ascii() {
        bail!("user must contain ASCII characters only");
    }
    if user.bytes().any(|byte| byte.is_ascii_uppercase()) {
        bail!("user must be lowercase canonical form");
    }
    if !user.bytes().all(|byte| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'.' | b'_' | b'-')
    }) {
        bail!("user contains unsupported characters");
    }
    if !user
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !user
            .as_bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
    {
        bail!("user must start and end with an ASCII letter or digit");
    }
    if user.contains("..") {
        return Err(anyhow!("user must not contain consecutive dots"));
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LnurlpConfig {
    pub callback: String,
    #[serde(rename = "maxSendable")]
    pub max_sendable: u64,
    #[serde(rename = "minSendable")]
    pub min_sendable: u64,
    pub metadata: String,
    pub tag: String,
    #[serde(rename = "commentAllowed")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment_allowed: Option<u64>,
    #[serde(rename = "allowsNostr")]
    pub allows_nostr: bool,
    #[serde(rename = "nostrPubkey")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nostr_pubkey: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InvoiceQueryParams {
    pub amount: u64,
    #[serde(default)]
    pub nostr: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LnurlpCallback {
    pub pr: String,
    pub routes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_lightning_address_users() {
        for user in ["herd", "sat", "goat-1", "goat_2", "goat.3", "123"] {
            validate_user(user).unwrap();
        }
    }

    #[test]
    fn rejects_ambiguous_or_unsafe_users() {
        for user in [
            "",
            "Herd",
            "-herd",
            "herd-",
            ".herd",
            "herd.",
            "herd..goats",
            "herd:goats",
            "herd/goats",
            "hérd",
        ] {
            assert!(validate_user(user).is_err(), "accepted invalid user {user:?}");
        }
    }

    #[test]
    fn old_user_metadata_json_remains_compatible() {
        let metadata: UserMetadata = serde_json::from_str(
            r#"{"is_email":true,"description":"Lightning Goats"}"#,
        )
        .unwrap();
        assert_eq!(metadata.is_email, Some(true));
        assert_eq!(metadata.description.as_deref(), Some("Lightning Goats"));
        assert_eq!(metadata.min_sendable_msat, None);
        assert_eq!(metadata.max_sendable_msat, None);
        assert_eq!(metadata.comment_allowed, None);
        assert_eq!(metadata.nostr_enabled, None);
    }

    #[test]
    fn validates_effective_limits_and_comment_bounds() {
        let metadata = UserMetadata {
            min_sendable_msat: Some(2_000),
            max_sendable_msat: Some(1_000),
            ..UserMetadata::default()
        };
        assert!(metadata.validate(1, 10_000).is_err());

        let metadata = UserMetadata {
            comment_allowed: Some(MAX_COMMENT_ALLOWED + 1),
            ..UserMetadata::default()
        };
        assert!(metadata.validate(1, 10_000).is_err());
    }
}
