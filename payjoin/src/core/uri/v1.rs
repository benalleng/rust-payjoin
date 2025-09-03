//! Payjoin v1 URI functionality

use url::Url;

use super::PjParseError;
use crate::uri::error::InternalPjParseError;

/// Payjoin v1 parameter containing the endpoint URL
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PjParam(Url);

impl PjParam {
    /// Parse a new v1 PjParam from a URL
    pub(crate) fn parse(url: Url) -> Result<Self, PjParseError> {
        if url.scheme() == "https"
            || url.scheme() == "http" && url.domain().unwrap_or_default().ends_with(".onion")
        {
            Ok(Self(url))
        } else {
            Err(InternalPjParseError::UnsecureEndpoint.into())
        }
    }

    /// Get the endpoint URL
    pub(crate) fn endpoint(&self) -> Url { self.0.clone() }
}

impl std::fmt::Display for PjParam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Use the same display logic as the encapsulated child Url
        self.0.fmt(f)
    }
}

#[cfg(all(test, feature = "v2"))]
mod tests {
    use payjoin_test_utils::BoxError;

    use crate::Uri;

    #[test]
    fn test_v1_failed_url_fragment() -> Result<(), BoxError> {
        let uri = "bitcoin:12c6DSiU4Rq3P4ZxziKxzrL5LmMBrzjrJX?amount=0.01\
                   &pjos=0&pj=HTTPS://EXAMPLE.COM/missing_short_id\
                   %23oh1qypm5jxyns754y4r45qwe336qfx6zr8dqgvqculvztv20tfveydmfqc";
        let extras = Uri::try_from(uri).unwrap().extras;
        match extras {
            crate::uri::MaybePayjoinExtras::Supported(extras) => {
                assert!(matches!(extras.pj_param, crate::uri::PjParam::V1(_)));
            }
            _ => panic!("Expected v1 pjparam"),
        }
        Ok(())
    }
}
