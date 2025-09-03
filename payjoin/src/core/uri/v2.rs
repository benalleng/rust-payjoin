use std::collections::BTreeMap;
use std::str::FromStr;

use bitcoin::bech32::Hrp;
use bitcoin::consensus::encode::Decodable;
use bitcoin::consensus::Encodable;
use url::Url;

use crate::hpke::HpkePublicKey;
use crate::ohttp::OhttpKeys;
use crate::uri::ShortId;

/// Retrieve the receiver's public key from the URL fragment
fn receiver_pubkey(url: &Url) -> Result<HpkePublicKey, ParseReceiverPubkeyParamError> {
    let value = get_param(url, "RK1")
        .map_err(ParseReceiverPubkeyParamError::InvalidFragment)?
        .ok_or(ParseReceiverPubkeyParamError::MissingPubkey)?;

    let (hrp, bytes) = crate::bech32::nochecksum::decode(value)
        .map_err(ParseReceiverPubkeyParamError::DecodeBech32)?;

    let rk_hrp: Hrp = Hrp::parse("RK").unwrap();
    if hrp != rk_hrp {
        return Err(ParseReceiverPubkeyParamError::InvalidHrp(hrp));
    }

    HpkePublicKey::from_compressed_bytes(&bytes[..])
        .map_err(ParseReceiverPubkeyParamError::InvalidPubkey)
}

/// Set the receiver's public key in the URL fragment
fn set_receiver_pubkey(url: &mut Url, pubkey: &HpkePublicKey) {
    let rk_hrp: Hrp = Hrp::parse("RK").unwrap();
    set_param(
        url,
        &crate::bech32::nochecksum::encode(rk_hrp, &pubkey.to_compressed_bytes())
            .expect("encoding compressed pubkey bytes should never fail"),
    )
}

/// Retrieve the ohttp parameter from the URL fragment
fn ohttp(url: &Url) -> Result<OhttpKeys, ParseOhttpKeysParamError> {
    let value = get_param(url, "OH1")
        .map_err(ParseOhttpKeysParamError::InvalidFragment)?
        .ok_or(ParseOhttpKeysParamError::MissingOhttpKeys)?;
    OhttpKeys::from_str(value).map_err(ParseOhttpKeysParamError::InvalidOhttpKeys)
}

/// Set the ohttp parameter in the URL fragment
fn set_ohttp(url: &mut Url, ohttp: &OhttpKeys) { set_param(url, &ohttp.to_string()) }

/// Retrieve the exp parameter from the URL fragment
fn exp(url: &Url) -> Result<std::time::SystemTime, ParseExpParamError> {
    let value = get_param(url, "EX1")
        .map_err(ParseExpParamError::InvalidFragment)?
        .ok_or(ParseExpParamError::MissingExp)?;

    let (hrp, bytes) =
        crate::bech32::nochecksum::decode(value).map_err(ParseExpParamError::DecodeBech32)?;

    let ex_hrp: Hrp = Hrp::parse("EX").unwrap();
    if hrp != ex_hrp {
        return Err(ParseExpParamError::InvalidHrp(hrp));
    }

    u32::consensus_decode(&mut &bytes[..])
        .map(|timestamp| std::time::UNIX_EPOCH + std::time::Duration::from_secs(timestamp as u64))
        .map_err(ParseExpParamError::InvalidExp)
}

/// Set the exp parameter in the URL fragment
fn set_exp(url: &mut Url, exp: &std::time::SystemTime) {
    let t = match exp.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs().try_into().unwrap(), // TODO Result type instead of Option & unwrap
        Err(_) => 0u32,
    };

    let mut buf = [0u8; 4];
    t.consensus_encode(&mut &mut buf[..]).unwrap(); // TODO no unwrap

    let ex_hrp: Hrp = Hrp::parse("EX").unwrap();

    let exp_str = crate::bech32::nochecksum::encode(ex_hrp, &buf)
        .expect("encoding u32 timestamp should never fail");

    set_param(url, &exp_str)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PjParam {
    directory: Url,
    id: ShortId,
    expiration: std::time::SystemTime,
    ohttp_keys: OhttpKeys,
    receiver_pubkey: HpkePublicKey,
}

impl PjParam {
    pub fn new(
        directory: Url,
        id: ShortId,
        expiration: std::time::SystemTime,
        ohttp_keys: OhttpKeys,
        receiver_pubkey: HpkePublicKey,
    ) -> Self {
        Self { directory, id, expiration, ohttp_keys, receiver_pubkey }
    }

    pub(crate) fn parse(url: Url) -> Result<Self, PjParseError> {
        let path_segments: Vec<&str> = url.path_segments().map(|c| c.collect()).unwrap_or_default();
        let id = if path_segments.len() == 1 {
            ShortId::from_str(path_segments[0]).map_err(|_| PjParseError::NotV2)?
        } else {
            return Err(PjParseError::NotV2);
        };

        if let Some(fragment) = url.fragment() {
            if fragment.chars().any(|c| c.is_lowercase()) {
                return Err(PjParseError::LowercaseFragment);
            }

            if !fragment.contains("RK1") || !fragment.contains("OH1") || !fragment.contains("EX1") {
                return Err(PjParseError::NotV2);
            }
        }

        let rk = receiver_pubkey(&url).map_err(PjParseError::InvalidReceiverPubkey)?;
        let oh = ohttp(&url).map_err(PjParseError::InvalidOhttpKeys)?;
        let ex = exp(&url).map_err(PjParseError::InvalidExp)?;

        Ok(Self::new(url, id, ex, oh, rk))
    }

    pub fn receiver_pubkey(&self) -> &HpkePublicKey { &self.receiver_pubkey }

    pub fn ohttp_keys(&self) -> &OhttpKeys { &self.ohttp_keys }

    pub fn expiration(&self) -> std::time::SystemTime { self.expiration }

    pub fn endpoint(&self) -> Url {
        let mut endpoint = self.directory.clone().join(&self.id.to_string()).unwrap();
        set_receiver_pubkey(&mut endpoint, &self.receiver_pubkey);
        set_ohttp(&mut endpoint, &self.ohttp_keys);
        set_exp(&mut endpoint, &self.expiration);
        endpoint
    }
}

#[derive(Debug)]
pub(crate) enum ParseFragmentError {
    InvalidChar(char),
    AmbiguousDelimiter,
}

impl std::error::Error for ParseFragmentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { None }
}

impl std::fmt::Display for ParseFragmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use ParseFragmentError::*;

        match &self {
            InvalidChar(c) => write!(f, "invalid character: {c} (must be uppercase)"),
            AmbiguousDelimiter => write!(f, "ambiguous fragment delimiter (both + and - found)"),
        }
    }
}

fn check_fragment_delimiter(fragment: &str) -> Result<char, ParseFragmentError> {
    // For backwards compatibility, also accept `+` as a
    // fragment parameter delimiter. This was previously
    // specified, but may be interpreted as ` ` by some
    // URI parsoing libraries. Therefore if `-` is missing,
    // assume the URI was generated following the older
    // version of the spec.

    let has_dash = fragment.contains('-');
    let has_plus = fragment.contains('+');

    // Even though fragment is a &str, it should be ascii so bytes() correspond
    // to chars(), except that it's easier to check that they are in range
    for c in fragment.bytes() {
        // These character ranges are more permissive than uppercase bech32, but
        // also more restrictive than bech32 in general since lowercase is not
        // allowed
        if !(b'0'..b'9' + 1).contains(&c)
            && !(b'A'..b'Z' + 1).contains(&c)
            && c != b'-'
            && c != b'+'
        {
            return Err(ParseFragmentError::InvalidChar(c.into()));
        }
    }

    match (has_dash, has_plus) {
        (true, true) => Err(ParseFragmentError::AmbiguousDelimiter),
        (false, true) => Ok('+'),
        _ => Ok('-'),
    }
}

fn get_param<'a>(url: &'a Url, prefix: &str) -> Result<Option<&'a str>, ParseFragmentError> {
    if let Some(fragment) = url.fragment() {
        let delim = check_fragment_delimiter(fragment)?;

        // The spec says these MUST be ordered lexicographically.
        // However, this was a late spec change, and only matters
        // for privacy reasons (fingerprinting implementations).
        // To maintain compatibility, we don't care about the order
        // of the parameters.
        for param in fragment.split(delim) {
            if param.starts_with(prefix) {
                return Ok(Some(param));
            }
        }
    }
    Ok(None)
}

/// Set a URL fragment parameter, inserting it or replacing it depending on
/// whether a parameter with the same bech32 HRP is already present.
///
/// Parameters are sorted lexicographically by prefix.
fn set_param(url: &mut Url, new_param: &str) {
    let fragment = url.fragment().unwrap_or("");
    let delim = check_fragment_delimiter(fragment)
        .expect("set_param must be called on a URL with a valid fragment");

    // In case of an invalid fragment parameter the following will still attempt
    // to retain the existing data
    let mut params = fragment
        .split(delim)
        .filter(|param| !param.is_empty())
        .map(|param| {
            let key = param.split('1').next().unwrap_or(param);
            (key, param)
        })
        .collect::<BTreeMap<&str, &str>>();

    // TODO: change param to Option(&str) to allow deletion?
    let key = new_param.split('1').next().unwrap_or(new_param);
    params.insert(key, new_param);

    if params.is_empty() {
        url.set_fragment(None)
    } else {
        // Can we avoid intermediate allocation of Vec, intersperse() exists but not in MSRV
        let fragment = params.values().copied().collect::<Vec<_>>().join("-");
        url.set_fragment(Some(&fragment));
    }
}

#[derive(Debug)]
pub(crate) enum PjParseError {
    NotV2,
    LowercaseFragment,
    InvalidReceiverPubkey(ParseReceiverPubkeyParamError),
    InvalidOhttpKeys(ParseOhttpKeysParamError),
    InvalidExp(ParseExpParamError),
}

impl std::fmt::Display for PjParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            PjParseError::NotV2 => write!(f, "URL is not a valid v2 URL"),
            PjParseError::LowercaseFragment => write!(f, "fragment contains lowercase characters"),
            PjParseError::InvalidReceiverPubkey(e) => write!(f, "invalid receiver pubkey: {e}"),
            PjParseError::InvalidOhttpKeys(e) => write!(f, "invalid ohttp keys: {e}"),
            PjParseError::InvalidExp(e) => write!(f, "invalid exp: {e}"),
        }
    }
}

impl std::error::Error for PjParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self {
            PjParseError::NotV2 => None,
            PjParseError::LowercaseFragment => None,
            PjParseError::InvalidReceiverPubkey(e) => Some(e),
            PjParseError::InvalidOhttpKeys(e) => Some(e),
            PjParseError::InvalidExp(e) => Some(e),
        }
    }
}

#[derive(Debug)]
pub(crate) enum ParseOhttpKeysParamError {
    MissingOhttpKeys,
    InvalidOhttpKeys(crate::ohttp::ParseOhttpKeysError),
    InvalidFragment(ParseFragmentError),
}

impl std::fmt::Display for ParseOhttpKeysParamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use ParseOhttpKeysParamError::*;

        match &self {
            MissingOhttpKeys => write!(f, "ohttp keys are missing"),
            InvalidOhttpKeys(o) => write!(f, "invalid ohttp keys: {o}"),
            InvalidFragment(e) => write!(f, "invalid URL fragment: {e}"),
        }
    }
}

impl std::error::Error for ParseOhttpKeysParamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        use ParseOhttpKeysParamError::*;
        match &self {
            MissingOhttpKeys => None,
            InvalidOhttpKeys(e) => Some(e),
            InvalidFragment(e) => Some(e),
        }
    }
}

#[derive(Debug)]
pub(crate) enum ParseExpParamError {
    MissingExp,
    InvalidHrp(bitcoin::bech32::Hrp),
    DecodeBech32(bitcoin::bech32::primitives::decode::CheckedHrpstringError),
    InvalidExp(bitcoin::consensus::encode::Error),
    InvalidFragment(ParseFragmentError),
}

impl std::fmt::Display for ParseExpParamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use ParseExpParamError::*;

        match &self {
            MissingExp => write!(f, "exp is missing"),
            InvalidHrp(h) => write!(f, "incorrect hrp for exp: {h}"),
            DecodeBech32(d) => write!(f, "exp is not valid bech32: {d}"),
            InvalidExp(i) =>
                write!(f, "exp param does not contain a bitcoin consensus encoded u32: {i}"),
            InvalidFragment(e) => write!(f, "invalid URL fragment: {e}"),
        }
    }
}

impl std::error::Error for ParseExpParamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        use ParseExpParamError::*;
        match &self {
            MissingExp => None,
            InvalidHrp(_) => None,
            DecodeBech32(e) => Some(e),
            InvalidExp(e) => Some(e),
            InvalidFragment(e) => Some(e),
        }
    }
}

#[derive(Debug)]
pub(crate) enum ParseReceiverPubkeyParamError {
    MissingPubkey,
    InvalidHrp(bitcoin::bech32::Hrp),
    DecodeBech32(bitcoin::bech32::primitives::decode::CheckedHrpstringError),
    InvalidPubkey(crate::hpke::HpkeError),
    InvalidFragment(ParseFragmentError),
}

impl std::fmt::Display for ParseReceiverPubkeyParamError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        use ParseReceiverPubkeyParamError::*;

        match &self {
            MissingPubkey => write!(f, "receiver public key is missing"),
            InvalidHrp(h) => write!(f, "incorrect hrp for receiver key: {h}"),
            DecodeBech32(e) => write!(f, "receiver public is not valid base64: {e}"),
            InvalidPubkey(e) =>
                write!(f, "receiver public key does not represent a valid pubkey: {e}"),
            InvalidFragment(e) => write!(f, "invalid URL fragment: {e}"),
        }
    }
}

impl std::error::Error for ParseReceiverPubkeyParamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        use ParseReceiverPubkeyParamError::*;

        match &self {
            MissingPubkey => None,
            InvalidHrp(_) => None,
            DecodeBech32(error) => Some(error),
            InvalidPubkey(error) => Some(error),
            InvalidFragment(error) => Some(error),
        }
    }
}

#[cfg(all(test, feature = "v2"))]
mod tests {
    use payjoin_test_utils::{BoxError, EXAMPLE_URL};

    use super::*;
    use crate::{Uri, UriExt};

    #[test]
    fn test_ohttp_get_set() {
        let mut url = EXAMPLE_URL.clone();

        let serialized = "OH1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC";
        let ohttp_keys = OhttpKeys::from_str(serialized).unwrap();
        set_ohttp(&mut url, &ohttp_keys);

        assert_eq!(url.fragment(), Some(serialized));
        assert_eq!(
            ohttp(&url).expect("Ohttp keys have been set but are missing on get"),
            ohttp_keys
        );
    }

    #[test]
    fn test_errors_when_parsing_ohttp() {
        let missing_ohttp_url = EXAMPLE_URL.clone();
        assert!(matches!(
            ohttp(&missing_ohttp_url),
            Err(ParseOhttpKeysParamError::MissingOhttpKeys)
        ));

        let invalid_ohttp_url =
            Url::parse("https://example.com?pj=https://test-payjoin-url#OH1invalid_bech_32")
                .unwrap();
        assert!(matches!(
            ohttp(&invalid_ohttp_url),
            Err(ParseOhttpKeysParamError::InvalidFragment(_))
        ));
    }

    #[test]
    fn test_exp_get_set() {
        let mut url = EXAMPLE_URL.clone();

        let exp_time =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1720547781);
        set_exp(&mut url, &exp_time);
        assert_eq!(url.fragment(), Some("EX1C4UC6ES"));

        assert_eq!(exp(&url).expect("Expiry has been set but is missing on get"), exp_time);
    }

    #[test]
    fn test_errors_when_parsing_exp() {
        let missing_exp_url = EXAMPLE_URL.clone();
        assert!(matches!(exp(&missing_exp_url), Err(ParseExpParamError::MissingExp)));

        let invalid_fragment_exp_url =
            Url::parse("http://example.com?pj=https://test-payjoin-url#EX1invalid_bech_32")
                .unwrap();
        assert!(matches!(
            exp(&invalid_fragment_exp_url),
            Err(ParseExpParamError::InvalidFragment(_))
        ));

        let invalid_bech32_exp_url =
            Url::parse("http://example.com?pj=https://test-payjoin-url#EX1INVALIDBECH32").unwrap();
        assert!(matches!(exp(&invalid_bech32_exp_url), Err(ParseExpParamError::DecodeBech32(_))));

        // Since the HRP is everything to the left of the right-most separator, the invalid url in
        // this test would have it's HRP being parsed as EX101 instead of the expected EX1
        let invalid_hrp_exp_url =
            Url::parse("http://example.com?pj=https://test-payjoin-url#EX1010").unwrap();
        assert!(matches!(exp(&invalid_hrp_exp_url), Err(ParseExpParamError::InvalidHrp(_))));

        // Not enough data to decode into a u32
        let invalid_timestamp_exp_url =
            Url::parse("http://example.com?pj=https://test-payjoin-url#EX10").unwrap();
        assert!(matches!(exp(&invalid_timestamp_exp_url), Err(ParseExpParamError::InvalidExp(_))));
    }

    #[test]
    fn test_errors_when_parsing_receiver_pubkey() {
        let missing_receiver_pubkey_url = EXAMPLE_URL.clone();
        assert!(matches!(
            receiver_pubkey(&missing_receiver_pubkey_url),
            Err(ParseReceiverPubkeyParamError::MissingPubkey)
        ));

        let invalid_fragment_receiver_pubkey_url =
            Url::parse("http://example.com?pj=https://test-payjoin-url#RK1invalid_bech_32")
                .unwrap();
        assert!(matches!(
            receiver_pubkey(&invalid_fragment_receiver_pubkey_url),
            Err(ParseReceiverPubkeyParamError::InvalidFragment(_))
        ));

        let invalid_bech32_receiver_pubkey_url =
            Url::parse("http://example.com?pj=https://test-payjoin-url#RK1INVALIDBECH32").unwrap();
        assert!(matches!(
            receiver_pubkey(&invalid_bech32_receiver_pubkey_url),
            Err(ParseReceiverPubkeyParamError::DecodeBech32(_))
        ));

        // Since the HRP is everything to the left of the right-most separator, the invalid url in
        // this test would have it's HRP being parsed as RK101 instead of the expected RK1
        let invalid_hrp_receiver_pubkey_url =
            Url::parse("http://example.com?pj=https://test-payjoin-url#RK101").unwrap();
        assert!(matches!(
            receiver_pubkey(&invalid_hrp_receiver_pubkey_url),
            Err(ParseReceiverPubkeyParamError::InvalidHrp(_))
        ));

        // Not enough data to decode into a u32
        let invalid_receiver_pubkey_url =
            Url::parse("http://example.com?pj=https://test-payjoin-url#RK10").unwrap();
        assert!(matches!(
            receiver_pubkey(&invalid_receiver_pubkey_url),
            Err(ParseReceiverPubkeyParamError::InvalidPubkey(_))
        ));
    }

    #[test]
    fn test_valid_v2_url_fragment_on_bip21() -> Result<(), BoxError> {
        let uri = "bitcoin:12c6DSiU4Rq3P4ZxziKxzrL5LmMBrzjrJX?amount=0.01&pjos=0&pj=HTTPS://EXAMPLE.COM/TXJCGKTKXLUUZ%23EX1C4UC6ES-OH1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC-RK1Q0DJS3VVDXWQQTLQ8022QGXSX7ML9PHZ6EDSF6AKEWQG758JPS2EV";
        let pjuri = Uri::try_from(uri).unwrap().assume_checked().check_pj_supported().unwrap();
        assert!(ohttp(&pjuri.extras.endpoint()).is_ok());
        assert_eq!(format!("{pjuri}"), uri);

        let reordered = "bitcoin:12c6DSiU4Rq3P4ZxziKxzrL5LmMBrzjrJX?amount=0.01&pj=HTTPS://EXAMPLE.COM/TXJCGKTKXLUUZ%23EX1C4UC6ES-OH1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC-RK1Q0DJS3VVDXWQQTLQ8022QGXSX7ML9PHZ6EDSF6AKEWQG758JPS2EV&pjos=0";
        let pjuri =
            Uri::try_from(reordered).unwrap().assume_checked().check_pj_supported().unwrap();
        assert!(ohttp(&pjuri.extras.endpoint()).is_ok());
        assert_eq!(format!("{pjuri}"), uri);
        Ok(())
    }

    #[test]
    fn test_v2_failed_url_fragment() -> Result<(), BoxError> {
        let uri = "bitcoin:12c6DSiU4Rq3P4ZxziKxzrL5LmMBrzjrJX?amount=0.01\
                   &pjos=0&pj=HTTPS://EXAMPLE.COM/TXJCGKTKXLUUZ%23ex1c4uc6es-oh1qypm5jxyns754y4r45qwe336qfx6zr8dqgvqculvztv20tfveydmfqc-rk1q0djs3vvdxwqqtlq8022qgxsx7ml9phz6edsf6akewqg758jps2ev";
        assert!(matches!(
            Uri::try_from(uri),
            Err(bitcoin_uri::de::Error::Extras(crate::uri::PjParseError(
                crate::uri::InternalPjParseError::V2(PjParseError::LowercaseFragment)
            )))
        ));

        let uri = "bitcoin:12c6DSiU4Rq3P4ZxziKxzrL5LmMBrzjrJX?amount=0.01\
                   &pjos=0&pj=HTTPS://EXAMPLE.COM/TXJCGKTKXLUUZ%23EX1C4UC6ES-OH1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC-RK1Q0DJS3VVDXWQQTLQ8022QGXSX7ML9PHZ6EDSF6AKEWQG758JPS2Ev";
        assert!(matches!(
            Uri::try_from(uri),
            Err(bitcoin_uri::de::Error::Extras(crate::uri::PjParseError(
                crate::uri::InternalPjParseError::V2(PjParseError::LowercaseFragment)
            )))
        ));
        Ok(())
    }

    #[test]
    fn test_fragment_delimiter_backwards_compatibility() {
        // ensure + is still accepted as a delimiter
        let url = "HTTPS://EXAMPLE.COM/TXJCGKTKXLUUZ\
                   #EX1C4UC6ES+OH1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC+RK1Q0DJS3VVDXWQQTLQ8022QGXSX7ML9PHZ6EDSF6AKEWQG758JPS2EV";

        let mut endpoint = Url::parse(url).unwrap();
        assert!(ohttp(&endpoint).is_ok());
        assert!(exp(&endpoint).is_ok());

        // Before setting the delimiter should be preserved
        assert_eq!(
            endpoint.fragment(),
            Some("EX1C4UC6ES+OH1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC+RK1Q0DJS3VVDXWQQTLQ8022QGXSX7ML9PHZ6EDSF6AKEWQG758JPS2EV")
        );

        let exp = exp(&endpoint).unwrap();
        // Upon setting any value, the delimiter should be normalized to `-`
        set_exp(&mut endpoint, &exp);
        assert_eq!(
            endpoint.fragment(),
            Some("EX1C4UC6ES-OH1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC-RK1Q0DJS3VVDXWQQTLQ8022QGXSX7ML9PHZ6EDSF6AKEWQG758JPS2EV")
        );
    }

    #[test]
    fn test_fragment_lexicographical_order() {
        let url_with_fragment = "HTTPS://EXAMPLE.COM/TXJCGKTKXLUUZ\
                   #OH1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC-EX1C4UC6ES";
        let mut endpoint = Url::parse(url_with_fragment).unwrap();
        assert!(ohttp(&endpoint).is_ok());
        assert!(exp(&endpoint).is_ok());

        assert_eq!(
            endpoint.fragment(),
            Some("OH1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC-EX1C4UC6ES")
        );
        assert!(ohttp(&endpoint).is_ok());
        assert!(exp(&endpoint).is_ok());

        assert_eq!(
            endpoint.fragment(),
            Some("OH1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC-EX1C4UC6ES")
        );

        // Upon setting any value, the order should be normalized to lexicographical
        let exp = exp(&endpoint).unwrap();
        set_exp(&mut endpoint, &exp);
        assert_eq!(
            endpoint.fragment(),
            Some("EX1C4UC6ES-OH1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC")
        );
    }

    #[test]
    fn test_fragment_mixed_delimiter() {
        // mixing current and deprecated delimiters should fail
        let fragment = "23RK1QG2RH36X9ZWRK\
7UWCCQE0WD8T89XKK2W55KTK9UHSZLEG8Q2TGEGG-OH1QYP87E2AVMDKXDTU6R25WCPQ5ZUF02XHNPA65JMD8ZA2W4YRQN6UUWG+EX1XPK8Y6Q";
        assert!(matches!(
            check_fragment_delimiter(fragment),
            Err(ParseFragmentError::AmbiguousDelimiter)
        ));
    }

    /// Test that all three parameters (RK1, OH1, EX1) are required in the fragment
    /// This test specifically targets the logic: !fragment.contains("RK1") || !fragment.contains("OH1") || !fragment.contains("EX1")
    /// to catch mutants that change || to &&
    #[test]
    fn test_fragment_parameter_validation() {
        // Missing RK1 parameter only
        let url_missing_rk1 = Url::parse("https://example.com/TXJCGKTKXLUUZ#OH1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC-EX1C4UC6ES").unwrap();
        assert!(matches!(PjParam::parse(url_missing_rk1), Err(PjParseError::NotV2)));

        // Missing OH1 parameter only
        let url_missing_oh1 = Url::parse("https://example.com/TXJCGKTKXLUUZ#RK1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC-EX1C4UC6ES").unwrap();
        assert!(matches!(PjParam::parse(url_missing_oh1), Err(PjParseError::NotV2)));

        // Missing EX1 parameter only
        let url_missing_ex1 = Url::parse("https://example.com/TXJCGKTKXLUUZ#RK1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC-OH1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC").unwrap();
        assert!(matches!(PjParam::parse(url_missing_ex1), Err(PjParseError::NotV2)));

        // Missing multiple parameters (only EX1 present) - tests first part of OR condition
        let url_only_ex1 = Url::parse("https://example.com/TXJCGKTKXLUUZ#EX1C4UC6ES").unwrap();
        assert!(matches!(PjParam::parse(url_only_ex1), Err(PjParseError::NotV2)));

        // Missing multiple parameters (only OH1 present) - tests middle part of OR condition
        let url_only_oh1 = Url::parse("https://example.com/TXJCGKTKXLUUZ#OH1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC").unwrap();
        assert!(matches!(PjParam::parse(url_only_oh1), Err(PjParseError::NotV2)));

        // Missing multiple parameters (only RK1 present) - tests last part of OR condition
        let url_only_rk1 = Url::parse("https://example.com/TXJCGKTKXLUUZ#RK1QYPM5JXYNS754Y4R45QWE336QFX6ZR8DQGVQCULVZTV20TFVEYDMFQC").unwrap();
        assert!(matches!(PjParam::parse(url_only_rk1), Err(PjParseError::NotV2)));
    }
}
