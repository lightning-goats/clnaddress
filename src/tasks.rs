use std::{path::Path, time::Duration};

use anyhow::Context;
use cln_plugin::Plugin;
use cln_rpc::{ClnRpc, model::requests::WaitanyinvoiceRequest};
use nostr::{
    event::{Event, FinalizeEventAsync},
    nips::nip57::{self, Nip57Tag},
};
use nostr_sdk::{authenticator::SignerAuthenticator, client::Client};
use tokio::fs;

use crate::{CLNADDRESS_PAYINDEX_FILENAME, structs::PluginState};

pub async fn zap_receipt_sender(plugin: Plugin<PluginState>) -> Result<(), anyhow::Error> {
    let mut rpc = ClnRpc::new(&plugin.state().rpc_path).await?;
    let keys = plugin
        .state()
        .nostr_zapper_keys
        .clone()
        .context("zap receipt sender started without configured Nostr keys")?;
    let mut lastpay_index = plugin.state().payindex;
    log::debug!("zap receipt cursor initialized at pay_index {lastpay_index}");
    loop {
        match rpc
            .call_typed(&WaitanyinvoiceRequest {
                lastpay_index: Some(lastpay_index),
                timeout: None,
            })
            .await
        {
            Ok(invoice) => {
                let next_pay_index = invoice.pay_index.unwrap_or(lastpay_index.saturating_add(1));
                lastpay_index = next_pay_index;
                save_payindex(&plugin.state().plugin_dir, lastpay_index).await?;

                if let Some(description) = invoice.description {
                    if let Ok(zap_request) = Event::from_json(description.as_bytes()) {
                        let Some(bolt11) = invoice.bolt11 else {
                            log::warn!("No bolt11 found for zap receipt!");
                            continue;
                        };
                        let Some(preimage) = invoice.payment_preimage else {
                            log::warn!("No preimage found for zap receipt!");
                            continue;
                        };
                        let zap_receipt = nip57::ZapReceipt::new(bolt11, &zap_request)
                            .preimage(serde_json::to_string(&preimage)?);

                        let zap_receipt = match zap_receipt.finalize_async(&keys).await {
                            Ok(receipt) => receipt,
                            Err(error) => {
                                log::warn!("Could not sign zap receipt: {error}");
                                continue;
                            }
                        };

                        let client = Client::builder()
                            .authenticator(SignerAuthenticator::new(keys.clone()))
                            .build();

                        for tag in zap_request.tags {
                            if let Ok(Nip57Tag::Relays(relay_urls)) = Nip57Tag::parse(tag) {
                                for relay_url in &relay_urls {
                                    if let Err(error) = client.add_relay(relay_url).await {
                                        log::warn!(
                                            "Could not add relay {relay_url} to client: {error}"
                                        );
                                    }
                                }
                            }
                        }
                        if client.relays().await.is_empty() {
                            log::warn!("No relays included in zap request!");
                        }
                        client.connect().and_wait(Duration::from_secs(30)).await;
                        match client.send_event(&zap_receipt).await {
                            Ok(output) => {
                                for (url, failure) in output.failed {
                                    log::warn!("Sending to relay {url} failed: {failure}");
                                }
                                for (url, _success) in output.success {
                                    log::info!("Successfully sent zap_receipt to relay: {url}");
                                }
                            }
                            Err(error) => log::warn!("Could not send zap receipt: {error}"),
                        }
                    }
                }
            }
            Err(error) => {
                log::warn!("Err waiting on invoices: {error}");
            }
        }
    }
}

pub async fn save_payindex(path: &Path, payindex: u64) -> Result<(), anyhow::Error> {
    let serialized = serde_json::to_string(&payindex)?;
    let destination = path.join(CLNADDRESS_PAYINDEX_FILENAME);
    let temporary = path.join(format!(".{CLNADDRESS_PAYINDEX_FILENAME}.tmp"));
    fs::write(&temporary, serialized).await?;
    fs::rename(&temporary, &destination).await?;
    Ok(())
}
