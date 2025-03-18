use honggfuzz::fuzz;
use ohttp::hpke::{Aead, Kdf, Kem};
use ohttp::{KeyId, SymmetricSuite};
use payjoin::uri::url_ext::UrlExt;
use payjoin::{OhttpKeys, Url};

fn do_test(data: &[u8]) {
    let data_str = String::from_utf8_lossy(data);
    let mut url = Url::parse("https://example.com").unwrap();

    let serialized = "OH1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC";
    const KEY_ID: KeyId = 1;
    const KEM: Kem = Kem::K256Sha256;
    const SYMMETRIC: &[SymmetricSuite] =
        &[ohttp::SymmetricSuite::new(Kdf::HkdfSha256, Aead::ChaCha20Poly1305)];
    let ohttp_keys = OhttpKeys(ohttp::KeyConfig::new(KEY_ID, KEM, Vec::from(SYMMETRIC)).unwrap());
    //let ohttp_keys = OhttpKeys::from_str(serialized).unwrap();
    url.set_ohttp(ohttp_keys.clone());
    assert_eq!(url.fragment(), Some(serialized));

    let receiver_pubkey = HpkeKeyPair::gen_keypair().1;
    url.set_receiver_pubkey(receiver_pubkey);

    let exp_time = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1720547781);
    url.set_exp(exp_time);

    let compressed_receiver_pubkey = match url.receiver_pubkey() {
        Ok(rpk) => rpk.to_compressed_bytes(),
        Err(_) => return,
    };
    let receiver_pubkey_roundtrip =
        match HpkePublicKey::from_compressed_bytes(&compressed_receiver_pubkey) {
            Ok(rpk) => rpk.0,
            Err(_) => return,
        };

    assert_eq!(url.ohttp().unwrap(), ohttp_keys);
    assert_eq!(url.receiver_pubkey().unwrap().0, receiver_pubkey_roundtrip);
    assert_eq!(url.exp().unwrap(), exp_time);
    assert!(url_ext::parse_with_fragment(url.as_str()).is_ok())
}

fn main() {
    loop {
        fuzz!(|data| {
            do_test(data);
        });
    }
}
