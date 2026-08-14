use std::{
    collections::HashMap,
    fs,
    net::{SocketAddr, ToSocketAddrs},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, anyhow, bail};
use cln_plugin::ConfiguredPlugin;
use parking_lot::Mutex;
use url::Url;

use crate::{
    OPT_CLNADDRESS_BASE_URL, OPT_CLNADDRESS_DESCRIPTION, OPT_CLNADDRESS_LISTEN,
    OPT_CLNADDRESS_MAX_RECEIVABLE, OPT_CLNADDRESS_MIN_RECEIVABLE,
    OPT_CLNADDRESS_NOSTR_PRIVKEY, OPT_CLNADDRESS_NOSTR_PRIVKEY_FILE, PluginState,
};

pub fn get_startup_options(
    plugin: &ConfiguredPlugin<PluginState, tokio::io::Stdin, tokio::io::Stdout>,
) -> Result<PluginState, anyhow::Error> {
    let rpc_path: PathBuf =
        Path::new(&plugin.configuration().lightning_dir).join(plugin.configuration().rpc_file);

    let listen_opt = plugin.option(&OPT_CLNADDRESS_LISTEN)?;
    let Some((listen_address_str, _listen_port_str)) = listen_opt.rsplit_once(':') else {
        return Err(anyhow!(
            "`{}` is invalid, it should have one `:`",
            OPT_CLNADDRESS_LISTEN.name()
        ));
    };
    let listen_address: SocketAddr = match listen_address_str {
        i if i.eq("localhost") => listen_opt
            .to_socket_addrs()?
            .next()
            .ok_or(anyhow!("No address found for localhost"))?,
        _ => {
            if let Ok(addr) = listen_opt.parse() {
                addr
            } else {
                return Err(anyhow!(
                    "`{}` should be a valid IP.",
                    OPT_CLNADDRESS_LISTEN.name()
                ));
            }
        }
    };

    let Some(mut base_url_str) = plugin.option(&OPT_CLNADDRESS_BASE_URL)? else {
        return Err(anyhow!("Please specify a base URL!"));
    };
    let base_url: Url = if base_url_str.ends_with('/') {
        base_url_str.parse()?
    } else {
        base_url_str.push('/');
        base_url_str.parse()?
    };

    if !base_url.has_host() {
        return Err(anyhow!("Invalid base URL! Missing host part! {base_url}"));
    }

    let min_sendable_msat = u64::try_from(plugin.option(&OPT_CLNADDRESS_MIN_RECEIVABLE)?)?;
    let max_sendable_msat = u64::try_from(plugin.option(&OPT_CLNADDRESS_MAX_RECEIVABLE)?)?;

    if min_sendable_msat > max_sendable_msat {
        return Err(anyhow!(
            "`{}` is greater than `{}`!",
            OPT_CLNADDRESS_MIN_RECEIVABLE.name(),
            OPT_CLNADDRESS_MAX_RECEIVABLE.name()
        ));
    }

    let default_description = plugin.option(&OPT_CLNADDRESS_DESCRIPTION)?;
    let inline_nostr_privkey = plugin.option(&OPT_CLNADDRESS_NOSTR_PRIVKEY)?;
    let nostr_privkey_file = plugin.option(&OPT_CLNADDRESS_NOSTR_PRIVKEY_FILE)?;
    let nostr_zapper_keys = load_nostr_zapper_keys(inline_nostr_privkey, nostr_privkey_file)?;

    let plugin_dir = Path::new(&plugin.configuration().lightning_dir).join("clnaddress");

    Ok(PluginState {
        rpc_path,
        max_sendable_msat,
        min_sendable_msat,
        default_description,
        users: Arc::new(Mutex::new(HashMap::new())),
        user_update_lock: Arc::new(tokio::sync::Mutex::new(())),
        plugin_dir,
        base_url,
        nostr_zapper_keys,
        payindex: 0,
        listen_address,
    })
}

fn load_nostr_zapper_keys(
    inline_privkey: Option<String>,
    privkey_file: Option<String>,
) -> Result<Option<nostr::key::Keys>, anyhow::Error> {
    let secret = match (inline_privkey, privkey_file) {
        (Some(_), Some(_)) => {
            bail!(
                "configure only one of `{}` or `{}`",
                OPT_CLNADDRESS_NOSTR_PRIVKEY.name(),
                OPT_CLNADDRESS_NOSTR_PRIVKEY_FILE.name()
            );
        }
        (Some(secret), None) => Some(secret),
        (None, Some(path)) => Some(read_secret_file(Path::new(&path))?),
        (None, None) => None,
    };

    secret
        .map(|secret| {
            let secret = secret.trim();
            if secret.is_empty() {
                bail!("Nostr zap receipt private key is empty");
            }
            nostr::key::Keys::parse(secret).context("invalid Nostr zap receipt private key")
        })
        .transpose()
}

fn read_secret_file(path: &Path) -> Result<String, anyhow::Error> {
    if path.as_os_str().is_empty() {
        bail!("Nostr private key file path is empty");
    }

    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("could not stat Nostr private key file {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("Nostr private key file must not be a symbolic link");
    }
    if !metadata.is_file() {
        bail!("Nostr private key path must reference a regular file");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            bail!(
                "Nostr private key file permissions are too broad ({mode:o}); use mode 0600 or stricter"
            );
        }
    }

    fs::read_to_string(path)
        .with_context(|| format!("could not read Nostr private key file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_two_zap_secret_sources() {
        assert!(
            load_nostr_zapper_keys(Some("secret".to_owned()), Some("/tmp/key".to_owned()))
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_group_or_world_readable_zap_secret_file() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "clnaddress-insecure-zap-key-{}",
            std::process::id()
        ));
        fs::write(&path, "test-secret").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(read_secret_file(&path).is_err());
        let _ = fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn accepts_private_regular_zap_secret_file() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "clnaddress-private-zap-key-{}",
            std::process::id()
        ));
        fs::write(&path, "test-secret\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        assert_eq!(read_secret_file(&path).unwrap(), "test-secret\n");
        let _ = fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_zap_secret_file() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp_dir = std::env::temp_dir();
        let target = temp_dir.join(format!("clnaddress-zap-target-{}", std::process::id()));
        let link = temp_dir.join(format!("clnaddress-zap-link-{}", std::process::id()));
        fs::write(&target, "test-secret").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, &link).unwrap();

        assert!(read_secret_file(&link).is_err());
        let _ = fs::remove_file(link);
        let _ = fs::remove_file(target);
    }
}
