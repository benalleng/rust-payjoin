use std::str::FromStr;
use std::sync::Arc;

use crate::request::Request;
use crate::send::error::{PsbtParseError, SenderInputError};
use crate::send::{RequestV1Context, V1Context};
use crate::uri::PjUri;
use crate::validation::{validate_amount_sat, validate_fee_rate_sat_per_kwu};

/// A builder for a BIP78 v1 [`V1Sender`].
///
/// Use this when the receiver's URI does not advertise v2 support and the
/// sender must fall back to the original BIP78 flow.
#[derive(Clone, uniffi::Object)]
pub struct V1SenderBuilder(payjoin::send::v1::SenderBuilder);

impl From<payjoin::send::v1::SenderBuilder> for V1SenderBuilder {
    fn from(value: payjoin::send::v1::SenderBuilder) -> Self { Self(value) }
}

#[uniffi::export]
impl V1SenderBuilder {
    /// Prepare the context from which to make v1 Sender requests.
    #[uniffi::constructor]
    pub fn new(psbt: String, uri: Arc<PjUri>) -> Result<Self, SenderInputError> {
        let psbt = payjoin::bitcoin::psbt::Psbt::from_str(psbt.as_str())
            .map_err(PsbtParseError::from)
            .map_err(SenderInputError::Psbt)?;
        let builder = payjoin::send::v1::SenderBuilder::new(psbt, Arc::unwrap_or_clone(uri).into());
        Ok(builder.into())
    }

    /// Disable output substitution even if the receiver didn't.
    pub fn always_disable_output_substitution(&self) -> Self {
        self.0.clone().always_disable_output_substitution().into()
    }

    /// Calculate the recommended fee contribution for an Original PSBT.
    pub fn build_recommended(
        &self,
        min_fee_rate_sat_per_kwu: u64,
    ) -> Result<V1Sender, SenderInputError> {
        let fee_rate = validate_fee_rate_sat_per_kwu(min_fee_rate_sat_per_kwu)?;
        self.0
            .clone()
            .build_recommended(fee_rate)
            .map(Into::into)
            .map_err(|e| SenderInputError::Build(Arc::new(e.into())))
    }

    /// Offer the receiver contribution to pay for his input.
    pub fn build_with_additional_fee(
        &self,
        max_fee_contribution_sats: u64,
        change_index: Option<u8>,
        min_fee_rate_sat_per_kwu: u64,
        clamp_fee_contribution: bool,
    ) -> Result<V1Sender, SenderInputError> {
        let max_fee_contribution = validate_amount_sat(max_fee_contribution_sats)?;
        let fee_rate = validate_fee_rate_sat_per_kwu(min_fee_rate_sat_per_kwu)?;
        self.0
            .clone()
            .build_with_additional_fee(
                max_fee_contribution,
                change_index.map(|x| x as usize),
                fee_rate,
                clamp_fee_contribution,
            )
            .map(Into::into)
            .map_err(|e| SenderInputError::Build(Arc::new(e.into())))
    }

    /// Perform Payjoin without incentivizing the payee to cooperate.
    pub fn build_non_incentivizing(
        &self,
        min_fee_rate_sat_per_kwu: u64,
    ) -> Result<V1Sender, SenderInputError> {
        let fee_rate = validate_fee_rate_sat_per_kwu(min_fee_rate_sat_per_kwu)?;
        self.0
            .clone()
            .build_non_incentivizing(fee_rate)
            .map(Into::into)
            .map_err(|e| SenderInputError::Build(Arc::new(e.into())))
    }
}

/// A BIP78 v1 sender ready to produce an HTTP POST request to the receiver.
#[derive(Clone, uniffi::Object)]
pub struct V1Sender(payjoin::send::v1::Sender);

impl From<payjoin::send::v1::Sender> for V1Sender {
    fn from(value: payjoin::send::v1::Sender) -> Self { Self(value) }
}

#[uniffi::export]
impl V1Sender {
    /// Construct serialized v1 Request and Context from a Payjoin Proposal.
    pub fn create_v1_post_request(&self) -> RequestV1Context {
        let (req, ctx) = self.0.create_v1_post_request();
        RequestV1Context { request: Request::from(req), context: Arc::new(V1Context::from(ctx)) }
    }

    /// The endpoint in the Payjoin URI.
    pub fn endpoint(&self) -> String { self.0.endpoint() }
}
