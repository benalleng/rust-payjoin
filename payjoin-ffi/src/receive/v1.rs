use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use payjoin::bitcoin::psbt::Psbt;
use payjoin::receive::v1;

use crate::receive::error::{
    InputContributionError, OutputSubstitutionError, ReceiverError, SelectionError,
};
use crate::receive::{
    CanBroadcast, InputPair, IsOutputKnown, IsScriptOwned, OutPoint, ProcessPsbt, TxOut,
};
use crate::validation::{
    validate_fee_rate_sat_per_kwu_opt, validate_fee_rate_sat_per_vb_opt, validate_script_vec,
};
use crate::ImplementationError;

struct HeaderMap(HashMap<String, String>);

impl v1::Headers for HeaderMap {
    fn get_header(&self, key: &str) -> Option<&str> { self.0.get(key).map(|s| s.as_str()) }
}

/// The BIP78 v1 receiver's first typestate: the unchecked original payload
/// received from the sender.
#[derive(Clone, uniffi::Object)]
pub struct V1UncheckedOriginalPayload(v1::UncheckedOriginalPayload);

impl From<v1::UncheckedOriginalPayload> for V1UncheckedOriginalPayload {
    fn from(value: v1::UncheckedOriginalPayload) -> Self { Self(value) }
}

#[uniffi::export]
impl V1UncheckedOriginalPayload {
    /// Parse a BIP78 v1 receiver request from the HTTP body, query string,
    /// and headers (must include `content-type` and `content-length`).
    #[uniffi::constructor]
    pub fn from_request(
        body: Vec<u8>,
        query: String,
        headers: HashMap<String, String>,
    ) -> Result<Self, ReceiverError> {
        v1::UncheckedOriginalPayload::from_request(
            body.as_slice(),
            query.as_str(),
            HeaderMap(headers),
        )
        .map(Self)
        .map_err(Into::into)
    }

    /// Verify that the original PSBT is broadcastable.
    pub fn check_broadcast_suitability(
        &self,
        min_fee_rate_sat_per_kwu: Option<u64>,
        can_broadcast: Arc<dyn CanBroadcast>,
    ) -> Result<Arc<V1MaybeInputsOwned>, ReceiverError> {
        let min_fee_rate = validate_fee_rate_sat_per_kwu_opt(min_fee_rate_sat_per_kwu)
            .map_err(|e| ReceiverError::Implementation(Arc::new(ImplementationError::new(e))))?;
        self.0
            .clone()
            .check_broadcast_suitability(min_fee_rate, |tx| {
                can_broadcast
                    .callback(payjoin::bitcoin::consensus::encode::serialize(tx))
                    .map_err(|e| ImplementationError::new(e).into())
            })
            .map(|v| Arc::new(V1MaybeInputsOwned(v)))
            .map_err(Into::into)
    }

    /// Skip broadcast-suitability checks. Use for interactive receivers.
    pub fn assume_interactive_receiver(&self) -> Arc<V1MaybeInputsOwned> {
        Arc::new(V1MaybeInputsOwned(self.0.clone().assume_interactive_receiver()))
    }
}

#[derive(Clone, uniffi::Object)]
pub struct V1MaybeInputsOwned(v1::MaybeInputsOwned);

#[uniffi::export]
impl V1MaybeInputsOwned {
    /// Returns the consensus-encoded original transaction for fallback
    /// broadcast scheduling.
    pub fn extract_tx_to_schedule_broadcast(&self) -> Vec<u8> {
        payjoin::bitcoin::consensus::encode::serialize(&self.0.extract_tx_to_schedule_broadcast())
    }

    pub fn check_inputs_not_owned(
        &self,
        is_owned: Arc<dyn IsScriptOwned>,
    ) -> Result<Arc<V1MaybeInputsSeen>, ReceiverError> {
        self.0
            .clone()
            .check_inputs_not_owned(&mut |script| {
                is_owned.callback(script.to_bytes()).map_err(|e| ImplementationError::new(e).into())
            })
            .map(|v| Arc::new(V1MaybeInputsSeen(v)))
            .map_err(Into::into)
    }
}

#[derive(Clone, uniffi::Object)]
pub struct V1MaybeInputsSeen(v1::MaybeInputsSeen);

#[uniffi::export]
impl V1MaybeInputsSeen {
    pub fn check_no_inputs_seen_before(
        &self,
        is_known: Arc<dyn IsOutputKnown>,
    ) -> Result<Arc<V1OutputsUnknown>, ReceiverError> {
        self.0
            .clone()
            .check_no_inputs_seen_before(&mut |outpoint| {
                is_known
                    .callback(OutPoint::from(*outpoint))
                    .map_err(|e| ImplementationError::new(e).into())
            })
            .map(|v| Arc::new(V1OutputsUnknown(v)))
            .map_err(Into::into)
    }
}

#[derive(Clone, uniffi::Object)]
pub struct V1OutputsUnknown(v1::OutputsUnknown);

#[uniffi::export]
impl V1OutputsUnknown {
    pub fn identify_receiver_outputs(
        &self,
        is_receiver_output: Arc<dyn IsScriptOwned>,
    ) -> Result<Arc<V1WantsOutputs>, ReceiverError> {
        self.0
            .clone()
            .identify_receiver_outputs(&mut |script| {
                is_receiver_output
                    .callback(script.to_bytes())
                    .map_err(|e| ImplementationError::new(e).into())
            })
            .map(|v| Arc::new(V1WantsOutputs(v)))
            .map_err(Into::into)
    }
}

#[derive(Clone, uniffi::Object)]
pub struct V1WantsOutputs(v1::WantsOutputs);

#[uniffi::export]
impl V1WantsOutputs {
    pub fn output_substitution(&self) -> crate::OutputSubstitution { self.0.output_substitution() }

    pub fn replace_receiver_outputs(
        &self,
        replacement_outputs: Vec<TxOut>,
        drain_script_pubkey: Vec<u8>,
    ) -> Result<Arc<V1WantsOutputs>, OutputSubstitutionError> {
        let replacement_outputs = replacement_outputs
            .into_iter()
            .map(|o| o.into_core())
            .collect::<Result<Vec<_>, _>>()?;
        let drain_script = validate_script_vec("drain_script_pubkey", drain_script_pubkey, false)?;
        self.0
            .clone()
            .replace_receiver_outputs(replacement_outputs, &drain_script)
            .map(|v| Arc::new(V1WantsOutputs(v)))
            .map_err(Into::into)
    }

    pub fn substitute_receiver_script(
        &self,
        output_script_pubkey: Vec<u8>,
    ) -> Result<Arc<V1WantsOutputs>, OutputSubstitutionError> {
        let output_script =
            validate_script_vec("output_script_pubkey", output_script_pubkey, false)?;
        self.0
            .clone()
            .substitute_receiver_script(&output_script)
            .map(|v| Arc::new(V1WantsOutputs(v)))
            .map_err(Into::into)
    }

    pub fn commit_outputs(&self) -> Arc<V1WantsInputs> {
        Arc::new(V1WantsInputs(self.0.clone().commit_outputs()))
    }
}

#[derive(Clone, uniffi::Object)]
pub struct V1WantsInputs(v1::WantsInputs);

#[uniffi::export]
impl V1WantsInputs {
    pub fn try_preserving_privacy(
        &self,
        candidate_inputs: Vec<Arc<InputPair>>,
    ) -> Result<Arc<InputPair>, SelectionError> {
        let candidates: Vec<payjoin::receive::InputPair> =
            candidate_inputs.into_iter().map(|p| Arc::unwrap_or_clone(p).into()).collect();
        self.0.try_preserving_privacy(candidates).map(|p| Arc::new(p.into())).map_err(Into::into)
    }

    pub fn contribute_inputs(
        &self,
        replacement_inputs: Vec<Arc<InputPair>>,
    ) -> Result<Arc<V1WantsInputs>, InputContributionError> {
        let inputs: Vec<payjoin::receive::InputPair> =
            replacement_inputs.into_iter().map(|p| Arc::unwrap_or_clone(p).into()).collect();
        self.0
            .clone()
            .contribute_inputs(inputs)
            .map(|v| Arc::new(V1WantsInputs(v)))
            .map_err(Into::into)
    }

    pub fn commit_inputs(&self) -> Arc<V1WantsFeeRange> {
        Arc::new(V1WantsFeeRange(self.0.clone().commit_inputs()))
    }
}

#[derive(Clone, uniffi::Object)]
pub struct V1WantsFeeRange(v1::WantsFeeRange);

#[uniffi::export]
impl V1WantsFeeRange {
    pub fn apply_fee_range(
        &self,
        min_fee_rate_sat_per_vb: Option<u64>,
        max_effective_fee_rate_sat_per_vb: Option<u64>,
    ) -> Result<Arc<V1ProvisionalProposal>, ReceiverError> {
        let min_fee_rate = validate_fee_rate_sat_per_vb_opt(min_fee_rate_sat_per_vb)
            .map_err(|e| ReceiverError::Implementation(Arc::new(ImplementationError::new(e))))?;
        let max_effective_fee_rate =
            validate_fee_rate_sat_per_vb_opt(max_effective_fee_rate_sat_per_vb).map_err(|e| {
                ReceiverError::Implementation(Arc::new(ImplementationError::new(e)))
            })?;
        self.0
            .clone()
            .apply_fee_range(min_fee_rate, max_effective_fee_rate)
            .map(|v| Arc::new(V1ProvisionalProposal(v)))
            .map_err(Into::into)
    }
}

#[derive(Clone, uniffi::Object)]
pub struct V1ProvisionalProposal(v1::ProvisionalProposal);

#[uniffi::export]
impl V1ProvisionalProposal {
    pub fn finalize_proposal(
        &self,
        process_psbt: Arc<dyn ProcessPsbt>,
    ) -> Result<Arc<V1PayjoinProposal>, ReceiverError> {
        self.0
            .clone()
            .finalize_proposal(|pre_processed| {
                let processed = process_psbt
                    .callback(pre_processed.to_string())
                    .map_err(payjoin::ImplementationError::new)?;
                Psbt::from_str(&processed).map_err(payjoin::ImplementationError::new)
            })
            .map(|v| Arc::new(V1PayjoinProposal(v)))
            .map_err(Into::into)
    }

    pub fn psbt_to_sign(&self) -> String { self.0.psbt_to_sign().to_string() }
}

#[derive(Clone, uniffi::Object)]
pub struct V1PayjoinProposal(v1::PayjoinProposal);

#[uniffi::export]
impl V1PayjoinProposal {
    /// The Payjoin Proposal PSBT serialized as base64.
    pub fn psbt(&self) -> String { self.0.psbt().to_string() }

    pub fn utxos_to_be_locked(&self) -> Vec<OutPoint> {
        self.0.utxos_to_be_locked().map(|o| OutPoint::from(*o)).collect()
    }
}
