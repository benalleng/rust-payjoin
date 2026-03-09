use anyhow::{anyhow, Result};

use super::Config;

pub(crate) struct ValidatedOhttpKeys {
    pub(crate) ohttp_keys: payjoin::OhttpKeys,
    pub(crate) relay_url: url::Url,
}

pub(crate) async fn unwrap_ohttp_keys_or_else_fetch(
    config: &Config,
    directory: Option<url::Url>,
) -> Result<ValidatedOhttpKeys> {
    if let Some(ohttp_keys) = config.v2()?.ohttp_keys.clone() {
        println!("Using OHTTP Keys from config");
        let relays = config.v2()?.ohttp_relays.clone();
        let relay_url = relay_order(&relays)
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("No OHTTP relays configured"))?
            .clone();
        Ok(ValidatedOhttpKeys { ohttp_keys, relay_url })
    } else {
        println!("Bootstrapping private network transport over Oblivious HTTP");
        fetch_ohttp_keys(config, directory).await
    }
}

fn relay_order(relays: &[url::Url]) -> Vec<&url::Url> {
    let count = relays.len();
    let start = if count > 0 {
        use payjoin::bitcoin::key::rand::RngCore;
        (payjoin::bitcoin::key::rand::thread_rng().next_u64() as usize) % count
    } else {
        0
    };
    (0..count).map(|i| &relays[(start + i) % count]).collect()
}

async fn fetch_ohttp_keys(
    config: &Config,
    directory: Option<url::Url>,
) -> Result<ValidatedOhttpKeys> {
    let payjoin_directory = directory.unwrap_or(config.v2()?.pj_directory.clone());
    let relays = config.v2()?.ohttp_relays.clone();

    if relays.len() < 2 {
        tracing::warn!(
            "Only one OHTTP relay configured. Add more ohttp_relays to improve privacy."
        );
    }

    for relay in relay_order(&relays) {
        let ohttp_keys = {
            #[cfg(feature = "_manual-tls")]
            {
                if let Some(cert_path) = config.root_certificate.as_ref() {
                    let cert_der = std::fs::read(cert_path)?;
                    payjoin::io::fetch_ohttp_keys_with_cert(
                        relay.as_str(),
                        payjoin_directory.as_str(),
                        &cert_der,
                    )
                    .await
                } else {
                    payjoin::io::fetch_ohttp_keys(relay.as_str(), payjoin_directory.as_str()).await
                }
            }
            #[cfg(not(feature = "_manual-tls"))]
            payjoin::io::fetch_ohttp_keys(relay.as_str(), payjoin_directory.as_str()).await
        };

        match ohttp_keys {
            Ok(keys) =>
                return Ok(ValidatedOhttpKeys { ohttp_keys: keys, relay_url: relay.clone() }),
            Err(payjoin::io::Error::UnexpectedStatusCode(e)) =>
                return Err(payjoin::io::Error::UnexpectedStatusCode(e).into()),
            Err(e) => tracing::debug!("Failed to connect to relay: {relay}, {e:?}"),
        }
    }

    Err(anyhow!("No valid relays available"))
}
