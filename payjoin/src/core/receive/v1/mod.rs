//! Receive BIP 78 Payjoin v1
//!
//! This module contains types and methods used to receive payjoin via BIP78.
//! Usage is pretty simple:
//!
//! 1. Generate a pj_uri [BIP 21](https://github.com/bitcoin/bips/blob/master/bip-0021.mediawiki)
//!    using [`build_v1_pj_uri`]
//! 2. Listen for a sender's request on the `pj` endpoint
//! 3. Parse the request using
//!    [`UncheckedProposal::from_request()`]
//! 4. Validate the proposal using the `check` methods to guide you.
//! 5. Assuming the proposal is valid, augment it into a payjoin with the available
//!    `try_preserving_privacy` and `contribute` methods
//! 6. Extract the payjoin PSBT and sign it
//! 7. Respond to the sender's http request with the signed PSBT as payload.
//!
//! The `receive` feature provides all of the check methods, PSBT data manipulation, coin
//! selection, and transport structures to receive payjoin and handle errors in a privacy
//! preserving way.
//!
//! Receiving payjoin entails listening to a secure http endpoint for inbound requests.  The
//! endpoint is displayed in the `pj` parameter of a [bip
//! 21](https://github.com/bitcoin/bips/blob/master/bip-0021.mediawiki) request URI.
//!
//! [reference implementation](https://github.com/payjoin/rust-payjoin/tree/master/payjoin-cli)
//!
//! OHTTP Privacy Warning
//! Encapsulated requests whether GET or POST—**must not be retried or reused**.
//! Retransmitting the same ciphertext (including via automatic retries) breaks the unlinkability and privacy guarantees of OHTTP,
//! as it allows the relay to correlate requests by comparing ciphertexts.
//! Note: Even fresh requests may be linkable via metadata (e.g. client IP, request timing),
//! but request reuse makes correlation trivial for the relay.

use std::cmp::{max, min};
use std::collections::BTreeMap;

use bitcoin::psbt::Psbt;
use bitcoin::secp256k1::rand::seq::SliceRandom;
use bitcoin::secp256k1::rand::{self, Rng};
use bitcoin::{Amount, FeeRate, OutPoint, Script, TxIn, TxOut, Weight};
use serde::{Deserialize, Serialize};

use super::error::{
    InputContributionError, InternalInputContributionError, InternalOutputSubstitutionError,
    InternalSelectionError,
};
use super::optional_parameters::Params;
use super::{InputPair, OutputSubstitutionError, ReplyableError, SelectionError};
use crate::output_substitution::OutputSubstitution;
use crate::psbt::PsbtExt;
use crate::receive::InternalPayloadError;
use crate::ImplementationError;

#[cfg(feature = "v1")]
mod exclusive;
#[cfg(feature = "v1")]
pub use exclusive::*;

/// The sender's original PSBT and optional parameters
///
/// This type is used to process the request. It is returned by
/// [`UncheckedProposal::from_request()`]
///
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UncheckedProposal {
    pub(crate) psbt: Psbt,
    pub(crate) params: Params,
}

impl UncheckedProposal {
    fn psbt_fee_rate(&self) -> Result<FeeRate, InternalPayloadError> {
        let original_psbt_fee = self.psbt.fee().map_err(|e| {
            InternalPayloadError::ParsePsbt(bitcoin::psbt::PsbtParseError::PsbtEncoding(e))
        })?;
        Ok(original_psbt_fee / self.psbt.clone().extract_tx_unchecked_fee_rate().weight())
    }

    /// Check that the Original PSBT can be broadcasted.
    ///
    /// Receiver MUST check that the Original PSBT from the sender
    /// can be broadcast, i.e. `testmempoolaccept` bitcoind rpc returns { "allowed": true,.. }.
    ///
    /// Receiver can optionally set a minimum feerate that will be enforced on the Original PSBT.
    /// This can be used to prevent probing attacks and make it easier to deal with
    /// high feerate environments.
    ///
    /// Do this check if you generate bitcoin uri to receive Payjoin on sender request without manual human approval, like a payment processor.
    /// Such so called "non-interactive" receivers are otherwise vulnerable to probing attacks.
    /// If a sender can make requests at will, they can learn which bitcoin the receiver owns at no cost.
    /// Broadcasting the Original PSBT after some time in the failure case makes incurs sender cost and prevents probing.
    ///
    /// Call this after checking downstream.
    pub fn check_broadcast_suitability(
        self,
        min_fee_rate: Option<FeeRate>,
        can_broadcast: impl Fn(&bitcoin::Transaction) -> Result<bool, ImplementationError>,
    ) -> Result<MaybeInputsOwned, ReplyableError> {
        let original_psbt_fee_rate = self.psbt_fee_rate()?;
        if let Some(min_fee_rate) = min_fee_rate {
            if original_psbt_fee_rate < min_fee_rate {
                return Err(InternalPayloadError::PsbtBelowFeeRate(
                    original_psbt_fee_rate,
                    min_fee_rate,
                )
                .into());
            }
        }
        if can_broadcast(&self.psbt.clone().extract_tx_unchecked_fee_rate())
            .map_err(ReplyableError::Implementation)?
        {
            Ok(MaybeInputsOwned { psbt: self.psbt, params: self.params })
        } else {
            Err(InternalPayloadError::OriginalPsbtNotBroadcastable.into())
        }
    }

    /// Call this method if the only way to initiate a Payjoin with this receiver
    /// requires manual intervention, as in most consumer wallets.
    ///
    /// So-called "non-interactive" receivers, like payment processors, that allow arbitrary requests are otherwise vulnerable to probing attacks.
    /// Those receivers call `extract_tx_to_check_broadcast()` after making those checks downstream.
    pub fn assume_interactive_receiver(self) -> MaybeInputsOwned {
        MaybeInputsOwned { psbt: self.psbt, params: self.params }
    }
}

/// Typestate to validate that the Original PSBT has no receiver-owned inputs.
///
/// Call [`Self::check_inputs_not_owned`] to proceed.
/// If you are implementing an interactive payment processor, you should get extract the original
/// transaction with extract_tx_to_schedule_broadcast() and schedule
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MaybeInputsOwned {
    psbt: Psbt,
    params: Params,
}

impl MaybeInputsOwned {
    /// The Sender's Original PSBT transaction
    pub fn extract_tx_to_schedule_broadcast(&self) -> bitcoin::Transaction {
        self.psbt.clone().extract_tx_unchecked_fee_rate()
    }
    /// Check that the Original PSBT has no receiver-owned inputs.
    /// Return original-psbt-rejected error or otherwise refuse to sign undesirable inputs.
    ///
    /// An attacker could try to spend receiver's own inputs. This check prevents that.
    pub fn check_inputs_not_owned(
        self,
        is_owned: impl Fn(&Script) -> Result<bool, ImplementationError>,
    ) -> Result<MaybeInputsSeen, ReplyableError> {
        let mut err: Result<(), ReplyableError> = Ok(());
        if let Some(e) = self
            .psbt
            .input_pairs()
            .scan(&mut err, |err, input| match input.previous_txout() {
                Ok(txout) => Some(txout.script_pubkey.to_owned()),
                Err(e) => {
                    **err = Err(InternalPayloadError::PrevTxOut(e).into());
                    None
                }
            })
            .find_map(|script| match is_owned(&script) {
                Ok(false) => None,
                Ok(true) => Some(InternalPayloadError::InputOwned(script).into()),
                Err(e) => Some(ReplyableError::Implementation(e)),
            })
        {
            return Err(e);
        }
        err?;

        Ok(MaybeInputsSeen { psbt: self.psbt, params: self.params })
    }
}

/// Typestate to validate that the Original PSBT has no inputs that have been seen before.
///
/// Call [`Self::check_no_inputs_seen_before`] to proceed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MaybeInputsSeen {
    psbt: Psbt,
    params: Params,
}
impl MaybeInputsSeen {
    /// Make sure that the original transaction inputs have never been seen before.
    /// This prevents probing attacks. This prevents reentrant Payjoin, where a sender
    /// proposes a Payjoin PSBT as a new Original PSBT for a new Payjoin.
    pub fn check_no_inputs_seen_before(
        self,
        is_known: impl Fn(&OutPoint) -> Result<bool, ImplementationError>,
    ) -> Result<OutputsUnknown, ReplyableError> {
        self.psbt.input_pairs().try_for_each(|input| {
            match is_known(&input.txin.previous_output) {
                Ok(false) => Ok::<(), ReplyableError>(()),
                Ok(true) =>  {
                    log::warn!("Request contains an input we've seen before: {}. Preventing possible probing attack.", input.txin.previous_output);
                    Err(InternalPayloadError::InputSeen(input.txin.previous_output))?
                },
                Err(e) => Err(ReplyableError::Implementation(e))?,
            }
        })?;

        Ok(OutputsUnknown { psbt: self.psbt, params: self.params })
    }
}

/// The receiver has not yet identified which outputs belong to the receiver.
///
/// Only accept PSBTs that send us money.
/// Identify those outputs with [`Self::identify_receiver_outputs`] to proceed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputsUnknown {
    psbt: Psbt,
    params: Params,
}

impl OutputsUnknown {
    /// Find which outputs belong to the receiver
    pub fn identify_receiver_outputs(
        self,
        is_receiver_output: impl Fn(&Script) -> Result<bool, ImplementationError>,
    ) -> Result<WantsOutputs, ReplyableError> {
        let owned_vouts: Vec<usize> = self
            .psbt
            .unsigned_tx
            .output
            .iter()
            .enumerate()
            .filter_map(|(vout, txo)| match is_receiver_output(&txo.script_pubkey) {
                Ok(true) => Some(Ok(vout)),
                Ok(false) => None,
                Err(e) => Some(Err(e)),
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(ReplyableError::Implementation)?;

        if owned_vouts.is_empty() {
            return Err(InternalPayloadError::MissingPayment.into());
        }

        let mut params = self.params.clone();
        if let Some((_, additional_fee_output_index)) = params.additional_fee_contribution {
            // If the additional fee output index specified by the sender is pointing to a receiver output,
            // the receiver should ignore the parameter.
            if owned_vouts.contains(&additional_fee_output_index) {
                params.additional_fee_contribution = None;
            }
        }

        Ok(WantsOutputs {
            original_psbt: self.psbt.clone(),
            payjoin_psbt: self.psbt,
            params,
            change_vout: owned_vouts[0],
            owned_vouts,
        })
    }
}

/// A checked proposal that the receiver may substitute or add outputs to
///
/// Call [`Self::commit_outputs`] to proceed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WantsOutputs {
    original_psbt: Psbt,
    payjoin_psbt: Psbt,
    params: Params,
    change_vout: usize,
    owned_vouts: Vec<usize>,
}

impl WantsOutputs {
    /// Whether the receiver is allowed to substitute original outputs or not.
    pub fn output_substitution(&self) -> OutputSubstitution { self.params.output_substitution }

    /// Substitute the receiver output script with the provided script.
    pub fn substitute_receiver_script(
        self,
        output_script: &Script,
    ) -> Result<Self, OutputSubstitutionError> {
        let output_value = self.original_psbt.unsigned_tx.output[self.change_vout].value;
        let outputs = [TxOut { value: output_value, script_pubkey: output_script.into() }];
        self.replace_receiver_outputs(outputs, output_script)
    }

    /// Replace **all** receiver outputs with one or more provided outputs.
    /// The drain script specifies which address to *drain* coins to. An output corresponding to
    /// that address must be included in `replacement_outputs`. The value of that output may be
    /// increased or decreased depending on the receiver's input contributions and whether the
    /// receiver needs to pay for additional miner fees (e.g. in the case of adding many outputs).
    pub fn replace_receiver_outputs(
        self,
        replacement_outputs: impl IntoIterator<Item = TxOut>,
        drain_script: &Script,
    ) -> Result<Self, OutputSubstitutionError> {
        let mut payjoin_psbt = self.original_psbt.clone();
        let mut outputs = vec![];
        let mut replacement_outputs: Vec<TxOut> = replacement_outputs.into_iter().collect();
        let mut rng = rand::thread_rng();
        // Substitute the existing receiver outputs, keeping the sender/receiver output ordering
        for (i, original_output) in self.original_psbt.unsigned_tx.output.iter().enumerate() {
            if self.owned_vouts.contains(&i) {
                // Receiver output: substitute in-place a provided replacement output
                if replacement_outputs.is_empty() {
                    return Err(InternalOutputSubstitutionError::NotEnoughOutputs.into());
                }
                match replacement_outputs
                    .iter()
                    .position(|txo| txo.script_pubkey == original_output.script_pubkey)
                {
                    // Select an output with the same address if one was provided
                    Some(pos) => {
                        let txo = replacement_outputs.swap_remove(pos);
                        if self.output_substitution() == OutputSubstitution::Disabled
                            && txo.value < original_output.value
                        {
                            return Err(
                                InternalOutputSubstitutionError::DecreasedValueWhenDisabled.into(),
                            );
                        }
                        outputs.push(txo);
                    }
                    // Otherwise randomly select one of the replacement outputs
                    None => {
                        if self.output_substitution() == OutputSubstitution::Disabled {
                            return Err(
                                InternalOutputSubstitutionError::ScriptPubKeyChangedWhenDisabled
                                    .into(),
                            );
                        }
                        let index = rng.gen_range(0..replacement_outputs.len());
                        let txo = replacement_outputs.swap_remove(index);
                        outputs.push(txo);
                    }
                }
            } else {
                // Sender output: leave it as is
                outputs.push(original_output.clone());
            }
        }
        // Insert all remaining outputs at random indices for privacy
        interleave_shuffle(&mut outputs, &mut replacement_outputs, &mut rng);
        // Identify the receiver output that will be used for change and fees
        let change_vout = outputs.iter().position(|txo| txo.script_pubkey == *drain_script);
        // Update the payjoin PSBT outputs
        payjoin_psbt.outputs = vec![Default::default(); outputs.len()];
        payjoin_psbt.unsigned_tx.output = outputs;
        Ok(Self {
            original_psbt: self.original_psbt,
            payjoin_psbt,
            params: self.params,
            change_vout: change_vout.ok_or(InternalOutputSubstitutionError::InvalidDrainScript)?,
            owned_vouts: self.owned_vouts,
        })
    }

    /// Proceed to the input contribution step.
    /// Outputs cannot be modified after this function is called.
    pub fn commit_outputs(self) -> WantsInputs {
        WantsInputs {
            original_psbt: self.original_psbt,
            payjoin_psbt: self.payjoin_psbt,
            params: self.params,
            change_vout: self.change_vout,
            receiver_inputs: vec![],
        }
    }
}

/// Shuffles `new` vector, then interleaves its elements with those from `original`,
/// maintaining the relative order in `original` but randomly inserting elements from `new`.
/// The combined result replaces the contents of `original`.
fn interleave_shuffle<T: Clone, R: rand::Rng>(original: &mut Vec<T>, new: &mut [T], rng: &mut R) {
    // Shuffle the substitute_outputs
    new.shuffle(rng);
    // Create a new vector to store the combined result
    let mut combined = Vec::with_capacity(original.len() + new.len());
    // Initialize indices
    let mut original_index = 0;
    let mut new_index = 0;
    // Interleave elements
    while original_index < original.len() || new_index < new.len() {
        if original_index < original.len() && (new_index >= new.len() || rng.gen_bool(0.5)) {
            combined.push(original[original_index].clone());
            original_index += 1;
        } else {
            combined.push(new[new_index].clone());
            new_index += 1;
        }
    }
    *original = combined;
}

/// A checked proposal that the receiver may contribute inputs to to make a payjoin
///
/// Call [`Self::commit_inputs`] to proceed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WantsInputs {
    original_psbt: Psbt,
    payjoin_psbt: Psbt,
    params: Params,
    change_vout: usize,
    receiver_inputs: Vec<InputPair>,
}

impl WantsInputs {
    /// Select receiver input such that the payjoin avoids surveillance.
    /// Return the input chosen that has been applied to the Proposal.
    ///
    /// Proper coin selection allows payjoin to resemble ordinary transactions.
    /// To ensure the resemblance, a number of heuristics must be avoided.
    ///
    /// Attempt to avoid UIH (Unnecessary input heuristic) for 2-output transactions.
    /// A simple consolidation is otherwise chosen if available.
    pub fn try_preserving_privacy(
        &self,
        candidate_inputs: impl IntoIterator<Item = InputPair>,
    ) -> Result<InputPair, SelectionError> {
        let mut candidate_inputs = candidate_inputs.into_iter().peekable();

        self.avoid_uih(&mut candidate_inputs)
            .or_else(|_| self.select_first_candidate(&mut candidate_inputs))
    }

    /// UIH "Unnecessary input heuristic" is one class of heuristics to avoid. We define
    /// UIH1 and UIH2 according to the BlockSci practice
    /// BlockSci UIH1 and UIH2:
    /// if min(in) > min(out) then UIH1 else UIH2
    /// <https://eprint.iacr.org/2022/589.pdf>
    ///
    /// This UIH avoidance function supports only
    /// many-input, 2-output transactions for now
    fn avoid_uih(
        &self,
        candidate_inputs: impl IntoIterator<Item = InputPair>,
    ) -> Result<InputPair, SelectionError> {
        if self.payjoin_psbt.outputs.len() != 2 {
            return Err(InternalSelectionError::UnsupportedOutputLength.into());
        }

        let min_out_sats = self
            .payjoin_psbt
            .unsigned_tx
            .output
            .iter()
            .map(|output| output.value)
            .min()
            .unwrap_or(Amount::MAX_MONEY);

        let min_in_sats = self
            .payjoin_psbt
            .input_pairs()
            .filter_map(|input| input.previous_txout().ok().map(|txo| txo.value))
            .min()
            .unwrap_or(Amount::MAX_MONEY);

        let prior_payment_sats = self.payjoin_psbt.unsigned_tx.output[self.change_vout].value;

        for input_pair in candidate_inputs {
            let candidate_sats = input_pair.previous_txout().value;
            let candidate_min_out = min(min_out_sats, prior_payment_sats + candidate_sats);
            let candidate_min_in = min(min_in_sats, candidate_sats);

            if candidate_min_in > candidate_min_out {
                // The candidate avoids UIH2 but conforms to UIH1: Optimal change heuristic.
                // It implies the smallest output is the sender's change address.
                return Ok(input_pair);
            }
        }

        // No suitable privacy preserving selection found
        Err(InternalSelectionError::NotFound.into())
    }

    fn select_first_candidate(
        &self,
        candidate_inputs: impl IntoIterator<Item = InputPair>,
    ) -> Result<InputPair, SelectionError> {
        candidate_inputs.into_iter().next().ok_or(InternalSelectionError::Empty.into())
    }

    /// Add the provided list of inputs to the transaction.
    /// Any excess input amount is added to the change_vout output indicated previously.
    pub fn contribute_inputs(
        self,
        inputs: impl IntoIterator<Item = InputPair>,
    ) -> Result<WantsInputs, InputContributionError> {
        let mut payjoin_psbt = self.payjoin_psbt.clone();
        // The payjoin proposal must not introduce mixed input sequence numbers
        let original_sequence = self
            .original_psbt
            .unsigned_tx
            .input
            .first()
            .map(|input| input.sequence)
            .unwrap_or_default();

        let inputs = inputs.into_iter().collect::<Vec<_>>();

        // Insert contributions at random indices for privacy
        let mut rng = rand::thread_rng();
        let mut receiver_input_amount = Amount::ZERO;
        for input_pair in inputs.clone() {
            receiver_input_amount += input_pair.previous_txout().value;
            let index = rng.gen_range(0..=self.payjoin_psbt.unsigned_tx.input.len());
            payjoin_psbt.inputs.insert(index, input_pair.psbtin);
            payjoin_psbt
                .unsigned_tx
                .input
                .insert(index, TxIn { sequence: original_sequence, ..input_pair.txin });
        }

        // Add the receiver change amount to the receiver change output, if applicable
        let receiver_min_input_amount = self.receiver_min_input_amount();
        if receiver_input_amount >= receiver_min_input_amount {
            let change_amount = receiver_input_amount - receiver_min_input_amount;
            payjoin_psbt.unsigned_tx.output[self.change_vout].value += change_amount;
        } else {
            return Err(InternalInputContributionError::ValueTooLow.into());
        }

        let mut receiver_inputs = self.receiver_inputs;
        receiver_inputs.extend(inputs);

        Ok(WantsInputs {
            original_psbt: self.original_psbt,
            payjoin_psbt,
            params: self.params,
            change_vout: self.change_vout,
            receiver_inputs,
        })
    }

    // Compute the minimum amount that the receiver must contribute to the transaction as input
    fn receiver_min_input_amount(&self) -> Amount {
        let output_amount = self
            .payjoin_psbt
            .unsigned_tx
            .output
            .iter()
            .fold(Amount::ZERO, |acc, output| acc + output.value);
        let original_output_amount = self
            .original_psbt
            .unsigned_tx
            .output
            .iter()
            .fold(Amount::ZERO, |acc, output| acc + output.value);
        output_amount.checked_sub(original_output_amount).unwrap_or(Amount::ZERO)
    }

    /// Proceed to the proposal finalization step.
    /// Inputs cannot be modified after this function is called.
    pub fn commit_inputs(self) -> ProvisionalProposal {
        ProvisionalProposal {
            original_psbt: self.original_psbt,
            payjoin_psbt: self.payjoin_psbt,
            params: self.params,
            change_vout: self.change_vout,
            receiver_inputs: self.receiver_inputs,
        }
    }
}

/// A checked proposal that the receiver may sign and finalize to make a proposal PSBT that the
/// sender will accept.
///
/// Call [`Self::finalize_proposal`] to return a finalized [`PayjoinProposal`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProvisionalProposal {
    original_psbt: Psbt,
    pub(crate) payjoin_psbt: Psbt,
    params: Params,
    change_vout: usize,
    receiver_inputs: Vec<InputPair>,
}

impl ProvisionalProposal {
    /// Apply additional fee contribution now that the receiver has contributed input
    /// this is kind of a "build_proposal" step before we sign and finalize and extract
    ///
    /// max_effective_fee_rate is the maximum effective fee rate that the receiver is
    /// willing to pay for their own input/output contributions. A max_effective_fee_rate
    /// of zero indicates that the receiver is not willing to pay any additional
    /// fees.
    ///
    /// If not provided, min_fee_rate and max_effective_fee_rate default to the
    /// minimum relay fee, as defined by [`FeeRate::BROADCAST_MIN`].
    fn apply_fee(
        &mut self,
        min_fee_rate: Option<FeeRate>,
        max_effective_fee_rate: Option<FeeRate>,
    ) -> Result<&Psbt, InternalPayloadError> {
        let min_fee_rate = min_fee_rate.unwrap_or(FeeRate::BROADCAST_MIN);
        log::trace!("min_fee_rate: {min_fee_rate:?}");
        log::trace!("params.min_fee_rate: {:?}", self.params.min_fee_rate);
        let min_fee_rate = max(min_fee_rate, self.params.min_fee_rate);
        log::debug!("min_fee_rate: {min_fee_rate:?}");

        let max_fee_rate = max_effective_fee_rate.unwrap_or(FeeRate::BROADCAST_MIN);

        // If the sender specified a fee contribution, the receiver is allowed to decrease the
        // sender's fee output to pay for additional input fees. Any fees in excess of
        // `max_additional_fee_contribution` must be covered by the receiver.
        let input_contribution_weight = self.additional_input_weight()?;
        let additional_fee = input_contribution_weight * min_fee_rate;
        log::trace!("additional_fee: {additional_fee}");
        let mut receiver_additional_fee = additional_fee;
        if additional_fee > Amount::ZERO {
            log::trace!(
                "self.params.additional_fee_contribution: {:?}",
                self.params.additional_fee_contribution
            );
            if let Some((max_additional_fee_contribution, additional_fee_output_index)) =
                self.params.additional_fee_contribution
            {
                // Find the sender's specified output in the original psbt.
                // This step is necessary because the sender output may have shifted if new
                // receiver outputs were added to the payjoin psbt.
                let sender_fee_output =
                    &self.original_psbt.unsigned_tx.output[additional_fee_output_index];
                // Find the index of that output in the payjoin psbt
                let sender_fee_vout = self
                    .payjoin_psbt
                    .unsigned_tx
                    .output
                    .iter()
                    .position(|txo| txo.script_pubkey == sender_fee_output.script_pubkey)
                    .expect("Sender output is missing from payjoin PSBT");
                // Determine the additional amount that the sender will pay in fees
                let sender_additional_fee = min(max_additional_fee_contribution, additional_fee);
                log::trace!("sender_additional_fee: {sender_additional_fee}");
                // Remove additional miner fee from the sender's specified output
                self.payjoin_psbt.unsigned_tx.output[sender_fee_vout].value -=
                    sender_additional_fee;
                receiver_additional_fee -= sender_additional_fee;
            }
        }

        // The sender's fee contribution can only be used to pay for additional input weight, so
        // any additional outputs must be paid for by the receiver.
        let output_contribution_weight = self.additional_output_weight();
        receiver_additional_fee += output_contribution_weight * min_fee_rate;
        log::trace!("receiver_additional_fee: {receiver_additional_fee}");
        // Ensure that the receiver does not pay more in fees
        // than they would by building a separate transaction at max_effective_fee_rate instead.
        let max_fee = (input_contribution_weight + output_contribution_weight) * max_fee_rate;
        log::trace!("max_fee: {max_fee}");
        if receiver_additional_fee > max_fee {
            let proposed_fee_rate =
                receiver_additional_fee / (input_contribution_weight + output_contribution_weight);
            return Err(InternalPayloadError::FeeTooHigh(proposed_fee_rate, max_fee_rate));
        }
        if receiver_additional_fee > Amount::ZERO {
            // Remove additional miner fee from the receiver's specified output
            self.payjoin_psbt.unsigned_tx.output[self.change_vout].value -= receiver_additional_fee;
        }
        Ok(&self.payjoin_psbt)
    }

    /// Calculate the additional input weight contributed by the receiver
    fn additional_input_weight(&self) -> Result<Weight, InternalPayloadError> {
        Ok(self.receiver_inputs.iter().map(|input_pair| input_pair.expected_weight).sum())
    }

    /// Calculate the additional output weight contributed by the receiver
    fn additional_output_weight(&self) -> Weight {
        let payjoin_outputs_weight = self
            .payjoin_psbt
            .unsigned_tx
            .output
            .iter()
            .fold(Weight::ZERO, |acc, txo| acc + txo.weight());
        let original_outputs_weight = self
            .original_psbt
            .unsigned_tx
            .output
            .iter()
            .fold(Weight::ZERO, |acc, txo| acc + txo.weight());
        let output_contribution_weight = payjoin_outputs_weight - original_outputs_weight;
        log::trace!("output_contribution_weight : {output_contribution_weight}");
        output_contribution_weight
    }

    /// Prepare the PSBT by creating a new PSBT and copying only the fields allowed by the [spec](https://github.com/bitcoin/bips/blob/master/bip-0078.mediawiki#senders-payjoin-proposal-checklist)
    fn prepare_psbt(self, processed_psbt: Psbt) -> PayjoinProposal {
        log::trace!("Original PSBT from callback: {processed_psbt:#?}");

        // Create a new PSBT and copy only the allowed fields
        let mut filtered_psbt = Psbt {
            unsigned_tx: processed_psbt.unsigned_tx,
            version: processed_psbt.version,
            xpub: BTreeMap::new(),
            proprietary: BTreeMap::new(),
            unknown: BTreeMap::new(),
            inputs: vec![],
            outputs: vec![],
        };

        for input in &processed_psbt.inputs {
            filtered_psbt.inputs.push(bitcoin::psbt::Input {
                witness_utxo: input.witness_utxo.clone(),
                non_witness_utxo: input.non_witness_utxo.clone(),
                sighash_type: input.sighash_type,
                final_script_sig: input.final_script_sig.clone(),
                final_script_witness: input.final_script_witness.clone(),
                tap_key_sig: input.tap_key_sig,
                tap_script_sigs: input.tap_script_sigs.clone(),
                tap_merkle_root: input.tap_merkle_root,
                ..Default::default()
            });
        }

        for _ in &processed_psbt.outputs {
            filtered_psbt.outputs.push(bitcoin::psbt::Output::default());
        }

        log::trace!("Filtered PSBT: {filtered_psbt:#?}");

        PayjoinProposal { payjoin_psbt: filtered_psbt }
    }

    /// Return the indexes of the sender inputs
    fn sender_input_indexes(&self) -> Vec<usize> {
        // iterate proposal as mutable WITH the outpoint (previous_output) available too
        let mut original_inputs = self.original_psbt.input_pairs().peekable();
        let mut sender_input_indexes = vec![];
        for (i, input) in self.payjoin_psbt.input_pairs().enumerate() {
            if let Some(original) = original_inputs.peek() {
                log::trace!(
                    "match previous_output: {} == {}",
                    input.txin.previous_output,
                    original.txin.previous_output
                );
                if input.txin.previous_output == original.txin.previous_output {
                    sender_input_indexes.push(i);
                    original_inputs.next();
                }
            }
        }
        sender_input_indexes
    }

    /// Return a Payjoin Proposal PSBT that the sender will find acceptable.
    ///
    /// This attempts to calculate any network fee owed by the receiver, subtract it from their output,
    /// and return a PSBT that can produce a consensus-valid transaction that the sender will accept.
    ///
    /// wallet_process_psbt should sign and finalize receiver inputs
    pub fn finalize_proposal(
        mut self,
        wallet_process_psbt: impl Fn(&Psbt) -> Result<Psbt, ImplementationError>,
        min_fee_rate: Option<FeeRate>,
        max_effective_fee_rate: Option<FeeRate>,
    ) -> Result<PayjoinProposal, ReplyableError> {
        let mut psbt = self.apply_fee(min_fee_rate, max_effective_fee_rate)?.clone();
        // Remove now-invalid sender signatures before applying the receiver signatures
        for i in self.sender_input_indexes() {
            log::trace!("Clearing sender input {i}");
            psbt.inputs[i].final_script_sig = None;
            psbt.inputs[i].final_script_witness = None;
            psbt.inputs[i].tap_key_sig = None;
        }
        let psbt = wallet_process_psbt(&psbt).map_err(ReplyableError::Implementation)?;
        let payjoin_proposal = self.prepare_psbt(psbt);
        Ok(payjoin_proposal)
    }
}

/// A finalized payjoin proposal, complete with fees and receiver signatures, that the sender
/// should find acceptable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PayjoinProposal {
    payjoin_psbt: Psbt,
}

impl PayjoinProposal {
    /// The UTXOs that would be spent by this Payjoin transaction
    pub fn utxos_to_be_locked(&self) -> impl '_ + Iterator<Item = &bitcoin::OutPoint> {
        self.payjoin_psbt.unsigned_tx.input.iter().map(|input| &input.previous_output)
    }

    /// The Payjoin Proposal PSBT
    pub fn psbt(&self) -> &Psbt { &self.payjoin_psbt }
}

#[cfg(test)]
pub(crate) mod test {
    use std::str::FromStr;

    use bitcoin::absolute::{LockTime, Time};
    use bitcoin::bip32::{DerivationPath, Fingerprint, Xpriv, Xpub};
    use bitcoin::hashes::Hash;
    use bitcoin::psbt::Input;
    use bitcoin::secp256k1::Secp256k1;
    use bitcoin::taproot::LeafVersion;
    use bitcoin::{
        Address, Amount, Network, OutPoint, PubkeyHash, ScriptBuf, Sequence, TapLeafHash,
        Transaction,
    };
    use payjoin_test_utils::{
        DUMMY20, PARSED_ORIGINAL_PSBT, QUERY_PARAMS, RECEIVER_INPUT_CONTRIBUTION,
    };
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    use super::*;
    use crate::receive::PayloadError;
    use crate::Version;

    pub(crate) fn unchecked_proposal_from_test_vector() -> UncheckedProposal {
        let pairs = url::form_urlencoded::parse(QUERY_PARAMS.as_bytes());
        let params = Params::from_query_pairs(pairs, &[Version::One])
            .expect("Could not parse params from query pairs");
        UncheckedProposal { psbt: PARSED_ORIGINAL_PSBT.clone(), params }
    }

    fn wants_outputs_from_test_vector(proposal: UncheckedProposal) -> WantsOutputs {
        proposal
            .assume_interactive_receiver()
            .check_inputs_not_owned(|_| Ok(false))
            .expect("No inputs should be owned")
            .check_no_inputs_seen_before(|_| Ok(false))
            .expect("No inputs should be seen before")
            .identify_receiver_outputs(|script| {
                let network = Network::Bitcoin;
                Ok(Address::from_script(script, network).unwrap()
                    == Address::from_str("3CZZi7aWFugaCdUCS15dgrUUViupmB8bVM")
                        .unwrap()
                        .require_network(network)
                        .unwrap())
            })
            .expect("Receiver output should be identified")
    }

    fn provisional_proposal_from_test_vector(proposal: UncheckedProposal) -> ProvisionalProposal {
        wants_outputs_from_test_vector(proposal).commit_outputs().commit_inputs()
    }

    #[test]
    fn is_output_substitution_disabled() {
        let mut proposal = unchecked_proposal_from_test_vector();
        let payjoin = wants_outputs_from_test_vector(proposal.clone());
        assert_eq!(payjoin.output_substitution(), OutputSubstitution::Enabled);

        proposal.params.output_substitution = OutputSubstitution::Disabled;
        let payjoin = wants_outputs_from_test_vector(proposal);
        assert_eq!(payjoin.output_substitution(), OutputSubstitution::Disabled);
    }

    #[test]
    fn unchecked_proposal_below_min_fee() {
        let proposal = unchecked_proposal_from_test_vector();
        let min_fee_rate = FeeRate::MAX;
        match proposal.clone().check_broadcast_suitability(Some(min_fee_rate), |_| Ok(true)) {
            Err(ReplyableError::Payload(PayloadError(InternalPayloadError::PsbtBelowFeeRate(
                proposal_rate,
                min_rate,
            )))) => {
                assert_eq!(proposal_rate, proposal.clone().psbt_fee_rate().unwrap());
                assert_eq!(min_rate, min_fee_rate);
            },
            _ => panic!("Broadcast suitability check should fail due to being below the min fee rate or unexpected error type"),
        };
    }

    #[test]
    fn unchecked_proposal_unlocks_after_checks() {
        let proposal = unchecked_proposal_from_test_vector();
        assert_eq!(proposal.psbt_fee_rate().unwrap().to_sat_per_vb_floor(), 2);
        let payjoin = provisional_proposal_from_test_vector(proposal);

        {
            let mut payjoin = payjoin.clone();
            let psbt = payjoin.apply_fee(None, None);
            assert!(psbt.is_ok(), "Payjoin should be a valid PSBT");
        }
        {
            let mut payjoin = payjoin.clone();
            let psbt = payjoin.apply_fee(None, Some(FeeRate::ZERO));
            assert!(psbt.is_ok(), "Payjoin should be a valid PSBT");
        }
    }

    #[test]
    fn empty_candidates_inputs() {
        let proposal = unchecked_proposal_from_test_vector();
        let wants_inputs = proposal
            .assume_interactive_receiver()
            .check_inputs_not_owned(|_| Ok(false))
            .expect("No inputs should be owned")
            .check_no_inputs_seen_before(|_| Ok(false))
            .expect("No inputs should be seen before")
            .identify_receiver_outputs(|script| {
                let network = Network::Bitcoin;
                let target_address = Address::from_str("3CZZi7aWFugaCdUCS15dgrUUViupmB8bVM")
                    .map_err(|e| e.to_string())?
                    .require_network(network)
                    .map_err(|e| e.to_string())?;

                let script_address =
                    Address::from_script(script, network).map_err(|e| e.to_string())?;
                Ok(script_address == target_address)
            })
            .expect("Receiver output should be identified")
            .commit_outputs();
        let empty_candidate_inputs: Vec<InputPair> = vec![];
        let result = wants_inputs.try_preserving_privacy(empty_candidate_inputs);
        assert_eq!(
            result.unwrap_err(),
            SelectionError::from(InternalSelectionError::Empty),
            "try_preserving_privacy should fail with empty candidate inputs"
        );
    }

    #[test]
    fn sender_specifies_excessive_fee_rate() {
        let mut proposal = unchecked_proposal_from_test_vector();
        assert_eq!(proposal.psbt_fee_rate().unwrap().to_sat_per_vb_floor(), 2);
        // Specify excessive fee rate in sender params
        proposal.params.min_fee_rate = FeeRate::from_sat_per_vb_unchecked(1000);
        let proposal_psbt = Psbt::from_str(RECEIVER_INPUT_CONTRIBUTION).unwrap();
        let input = InputPair::new(
            proposal_psbt.unsigned_tx.input[1].clone(),
            proposal_psbt.inputs[1].clone(),
            None,
        )
        .unwrap();
        let mut payjoin = wants_outputs_from_test_vector(proposal)
            .commit_outputs()
            .contribute_inputs(vec![input])
            .expect("Failed to contribute inputs")
            .commit_inputs();
        let additional_output = TxOut {
            value: Amount::ZERO,
            script_pubkey: payjoin.original_psbt.unsigned_tx.output[0].script_pubkey.clone(),
        };
        payjoin.payjoin_psbt.unsigned_tx.output.push(additional_output);
        let mut payjoin_clone = payjoin.clone();
        let psbt = payjoin.apply_fee(None, Some(FeeRate::from_sat_per_vb_unchecked(1000)));
        assert!(psbt.is_ok(), "Payjoin should be a valid PSBT");
        let psbt = payjoin_clone.apply_fee(None, Some(FeeRate::from_sat_per_vb_unchecked(995)));
        match psbt {
            Err(InternalPayloadError::FeeTooHigh(proposed, max)) => {
                assert_eq!(FeeRate::from_str("249630").unwrap(), proposed);
                assert_eq!(FeeRate::from_sat_per_vb_unchecked(995), max);
            }
            _ => panic!(
                "Payjoin exceeds receiver fee preference and should error or unexpected error type"
            ),
        }
    }

    #[test]
    fn additional_input_weight_matches_known_weight() {
        // All expected input weights pulled from:
        // https://bitcoin.stackexchange.com/questions/84004/how-do-virtual-size-stripped-size-and-raw-size-compare-between-legacy-address-f#84006
        // Input weight for a single P2PKH (legacy) receiver input
        let p2pkh_proposal = ProvisionalProposal {
            original_psbt: Psbt::from_str("cHNidP8BAHECAAAAAb2qhegy47hqffxh/UH5Qjd/G3sBH6cW2QSXZ86nbY3nAAAAAAD9////AhXKBSoBAAAAFgAU4TiLFD14YbpddFVrZa3+Zmz96yQQJwAAAAAAABYAFB4zA2o+5MsNRT/j+0twLi5VbwO9AAAAAAABAIcCAAAAAQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/////wMBSgD/////AgDyBSoBAAAAGXapFGUxpU6cGldVpjUm9rV2B+jTlphDiKwAAAAAAAAAACZqJKohqe3i9hw/cdHe/T+pmd+jaVN1XGkGiXmZYrSL69g2l06M+QAAAAABB2pHMEQCIGsOxO/bBv20bd68sBnEU3cxHR8OxEcUroL3ENhhjtN3AiB+9yWuBGKXu41hcfO4KP7IyLLEYc6j8hGowmAlCPCMPAEhA6WNSN4CqJ9F+42YKPlIFN0wJw7qawWbdelGRMkAbBRnACICAsdIAjsfMLKgfL2J9rfIa8yKdO1BOpSGRIFbFMBdTsc9GE4roNNUAACAAQAAgAAAAIABAAAAAAAAAAAA").unwrap(),
            payjoin_psbt: Psbt::from_str("cHNidP8BAJoCAAAAAtTRxwAtk38fRMP3ffdKkIi5r+Ss9AjaO8qEv+eQ/ho3AAAAAAD9////vaqF6DLjuGp9/GH9QflCN38bewEfpxbZBJdnzqdtjecAAAAAAP3///8CgckFKgEAAAAWABThOIsUPXhhul10VWtlrf5mbP3rJBAZBioBAAAAFgAUiDIby0wSbj1kv3MlvwoEKw3vNZUAAAAAAAEAhwIAAAABAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD/////AwFoAP////8CAPIFKgEAAAAZdqkUPXhu3I6D9R0wUpvTvvUm+VGNcNuIrAAAAAAAAAAAJmokqiGp7eL2HD9x0d79P6mZ36NpU3VcaQaJeZlitIvr2DaXToz5AAAAAAEBIgDyBSoBAAAAGXapFD14btyOg/UdMFKb0771JvlRjXDbiKwBB2pHMEQCIGzKy8QfhHoAY0+LZCpQ7ZOjyyXqaSBnr89hH3Eg/xsGAiB3n8hPRuXCX/iWtURfXoJNUFu3sLeQVFf1dDFCZPN0dAEhA8rTfrwcq6dEBSNOrUfNb8+dm7q77vCtfdOmWx0HfajRAAEAhwIAAAABAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD/////AwFKAP////8CAPIFKgEAAAAZdqkUZTGlTpwaV1WmNSb2tXYH6NOWmEOIrAAAAAAAAAAAJmokqiGp7eL2HD9x0d79P6mZ36NpU3VcaQaJeZlitIvr2DaXToz5AAAAAAAAAA==").unwrap(),
            params: Params::default(),
            change_vout: 0,
            receiver_inputs: vec![
                InputPair::new(
                    TxIn{
                        previous_output: OutPoint::from_str("371afe90e7bf84ca3bda08f4ace4afb988904af77df7c3441f7f932d00c7d1d4:0").unwrap(),
                        ..Default::default()
                    }, Input {
                        witness_utxo: Some(TxOut {
                            value: Amount::from_sat(5_000_000_000),
                            script_pubkey: ScriptBuf::from_hex("76a9143d786edc8e83f51d30529bd3bef526f9518d70db88ac").unwrap(),
                        }),
                        ..Default::default()
                    }, None)
                .unwrap()],
        };
        assert_eq!(
            p2pkh_proposal.additional_input_weight().expect("should calculate input weight"),
            Weight::from_wu(592)
        );

        // Input weight for a single nested P2WPKH (nested segwit) receiver input
        let nested_p2wpkh_proposal = ProvisionalProposal {
            original_psbt: Psbt::from_str("cHNidP8BAHECAAAAAeOsT9cRWRz3te+bgmtweG1vDLkdSH4057NuoodDNPFWAAAAAAD9////AhAnAAAAAAAAFgAUtp3bPFM/YWThyxD5Cc9OR4mb8tdMygUqAQAAABYAFODlplDoE6EGlZvmqoUngBgsu8qCAAAAAAABAIUCAAAAAQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/////wMBZwD/////AgDyBSoBAAAAF6kU2JnIn4Mmcb5kuF3EYeFei8IB43qHAAAAAAAAAAAmaiSqIant4vYcP3HR3v0/qZnfo2lTdVxpBol5mWK0i+vYNpdOjPkAAAAAAQEgAPIFKgEAAAAXqRTYmcifgyZxvmS4XcRh4V6LwgHjeocBBxcWABSPGoPK1yl60X4Z9OfA7IQPUWCgVwEIawJHMEQCICZG3s2cbulPnLTvK4TwlKhsC+cem8tD2GjZZ3eMJD7FAiADh/xwv0ib8ksOrj1M27DYLiw7WFptxkMkE2YgiNMRVgEhAlDMm5DA8kU+QGiPxEWUyV1S8+XGzUOepUOck257ZOhkAAAiAgP+oMbeca66mt+UtXgHm6v/RIFEpxrwG7IvPDim5KWHpBgfVHrXVAAAgAEAAIAAAACAAQAAAAAAAAAA").unwrap(),
            payjoin_psbt: Psbt::from_str("cHNidP8BAJoCAAAAAuXYOTUaVRiB8cPPhEXzcJ72/SgZOPEpPx5pkG0fNeGCAAAAAAD9////46xP1xFZHPe175uCa3B4bW8MuR1IfjTns26ih0M08VYAAAAAAP3///8CEBkGKgEAAAAWABQHuuu4H4fbQWV51IunoJLUtmMTfEzKBSoBAAAAFgAU4OWmUOgToQaVm+aqhSeAGCy7yoIAAAAAAAEBIADyBSoBAAAAF6kUQ4BssmVBS3r0s95c6dl1DQCHCR+HAQQWABQbDc333XiiOeEXroP523OoYNb1aAABAIUCAAAAAQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/////wMBZwD/////AgDyBSoBAAAAF6kU2JnIn4Mmcb5kuF3EYeFei8IB43qHAAAAAAAAAAAmaiSqIant4vYcP3HR3v0/qZnfo2lTdVxpBol5mWK0i+vYNpdOjPkAAAAAAQEgAPIFKgEAAAAXqRTYmcifgyZxvmS4XcRh4V6LwgHjeocBBxcWABSPGoPK1yl60X4Z9OfA7IQPUWCgVwEIawJHMEQCICZG3s2cbulPnLTvK4TwlKhsC+cem8tD2GjZZ3eMJD7FAiADh/xwv0ib8ksOrj1M27DYLiw7WFptxkMkE2YgiNMRVgEhAlDMm5DA8kU+QGiPxEWUyV1S8+XGzUOepUOck257ZOhkAAAA").unwrap(),
            params: Params::default(),
            change_vout: 0,
            receiver_inputs: vec![
                InputPair::new(
                    TxIn {
                        previous_output: OutPoint::from_str("82e1351f6d90691e3f29f1381928fdf69e70f34584cfc3f18118551a3539d8e5:0").unwrap(),
                        ..Default::default()
                    },
                    Input {
                        witness_utxo: Some(TxOut {
                            value: Amount::from_sat(5_000_000_000),
                            script_pubkey: ScriptBuf::from_hex("a91443806cb265414b7af4b3de5ce9d9750d0087091f87").unwrap(),
                        }),
                        redeem_script: Some(ScriptBuf::from_hex("00141b0dcdf7dd78a239e117ae83f9db73a860d6f568").unwrap()),
                        ..Default::default()
                    }, None)
                .unwrap()],
        };
        assert_eq!(
            nested_p2wpkh_proposal
                .additional_input_weight()
                .expect("should calculate input weight"),
            Weight::from_wu(364)
        );

        // Input weight for a single P2WPKH (native segwit) receiver input
        let p2wpkh_proposal = ProvisionalProposal {
            original_psbt: Psbt::from_str("cHNidP8BAHECAAAAASom13OiXZIr3bKk+LtUndZJYqdHQQU8dMs1FZ93IctIAAAAAAD9////AmPKBSoBAAAAFgAU6H98YM9NE1laARQ/t9/90nFraf4QJwAAAAAAABYAFBPJFmYuJBsrIaBBp9ur98pMSKxhAAAAAAABAIQCAAAAAQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/////wMBWwD/////AgDyBSoBAAAAFgAUjTJXmC73n+URSNdfgbS6Oa6JyQYAAAAAAAAAACZqJKohqe3i9hw/cdHe/T+pmd+jaVN1XGkGiXmZYrSL69g2l06M+QAAAAABAR8A8gUqAQAAABYAFI0yV5gu95/lEUjXX4G0ujmuickGAQhrAkcwRAIgUqbHS0difIGTRwN56z2/EiqLQFWerfJspyjuwsGSCXcCIA3IRTu8FVgniU5E4gecAMeegVnlTbTVfFyusWhQ2kVVASEDChVRm26KidHNWLdCLBTq5jspGJr+AJyyMqmUkvPkwFsAIgIDeBqmRB3ESjFWIp+wUXn/adGZU3kqWGjdkcnKpk8bAyUY94v8N1QAAIABAACAAAAAgAEAAAAAAAAAAAA=").unwrap(),
            payjoin_psbt: Psbt::from_str("cHNidP8BAJoCAAAAAiom13OiXZIr3bKk+LtUndZJYqdHQQU8dMs1FZ93IctIAAAAAAD9////NG21aH8Vat3thaVmPvWDV/lvRmymFHeePcfUjlyngHIAAAAAAP3///8CH8oFKgEAAAAWABTof3xgz00TWVoBFD+33/3ScWtp/hAZBioBAAAAFgAU1mbnqky3bMxfmm0OgFaQCAs5fsoAAAAAAAEAhAIAAAABAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD/////AwFbAP////8CAPIFKgEAAAAWABSNMleYLvef5RFI11+BtLo5ronJBgAAAAAAAAAAJmokqiGp7eL2HD9x0d79P6mZ36NpU3VcaQaJeZlitIvr2DaXToz5AAAAAAEBHwDyBSoBAAAAFgAUjTJXmC73n+URSNdfgbS6Oa6JyQYAAQCEAgAAAAEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAP////8DAWcA/////wIA8gUqAQAAABYAFJFtkfHTt3y1EDMaN6CFjjNWtpCRAAAAAAAAAAAmaiSqIant4vYcP3HR3v0/qZnfo2lTdVxpBol5mWK0i+vYNpdOjPkAAAAAAQEfAPIFKgEAAAAWABSRbZHx07d8tRAzGjeghY4zVraQkQEIawJHMEQCIDTC49IB9AnItqd8zy5RDc05f2ApBAfJ5x4zYfj3bsD2AiAQvvSt5ipScHcUwdlYB9vFnEi68hmh55M5a5e+oWvxMAEhAqErVSVulFb97/r5KQryOS1Xgghff8R7AOuEnvnmslQ5AAAA").unwrap(),
            params: Params::default(),
            change_vout: 0,
            receiver_inputs: vec![
                InputPair::new(
                    TxIn {
                        previous_output: OutPoint::from_str("7280a75c8ed4c73d9e7714a66c466ff95783f53e66a585eddd6a157f68b56d34:0").unwrap(),
                        ..Default::default()
                    }, Input {
                        witness_utxo: Some(TxOut {
                            value: Amount::from_sat(5_000_000_000),
                            script_pubkey: ScriptBuf::from_hex("0014916d91f1d3b77cb510331a37a0858e3356b69091").unwrap(),
                        }),
                        ..Default::default()
                    }, None)
                .unwrap()],
        };
        assert_eq!(
            p2wpkh_proposal.additional_input_weight().expect("should calculate input weight"),
            Weight::from_wu(272)
        );

        // Input weight for a single P2TR (taproot) receiver input
        let p2tr_proposal = ProvisionalProposal {
            original_psbt: Psbt::from_str("cHNidP8BAHECAAAAAU/CHxd1oi9Lq1xOD2GnHe0hsQdGJ2mkpYkmeasTj+w1AAAAAAD9////Am3KBSoBAAAAFgAUqJL/PDPnHeihhNhukTz8QEdZbZAQJwAAAAAAABYAFInyO0NQF7YR22Sm0YTPGm6yf19YAAAAAAABASsA8gUqAQAAACJRIGOPekNKFs9ASLj3FdlCLiou/jdPUegJGzlA111A80MAAQhCAUC3zX8eSeL8+bAo6xO0cpon83UsJdttiuwfMn/pBwub82rzMsoS6HZNXzg7hfcB3p1uj8JmqsBkZwm8k6fnU2peACICA+u+FjwmhEgWdjhEQbO49D0NG8iCYUoqhlfsj0LN7hiRGOcVI65UAACAAQAAgAAAAIABAAAAAAAAAAAA").unwrap(),
            payjoin_psbt: Psbt::from_str("cHNidP8BAJoCAAAAAk/CHxd1oi9Lq1xOD2GnHe0hsQdGJ2mkpYkmeasTj+w1AAAAAAD9////Fz+ELsYp/55j6+Jl2unG9sGvpHTiSyzSORBvtu1GEB4AAAAAAP3///8CM8oFKgEAAAAWABSokv88M+cd6KGE2G6RPPxAR1ltkBAZBioBAAAAFgAU68J5imRcKy3g5JCT3bEoP9IXEn0AAAAAAAEBKwDyBSoBAAAAIlEgY496Q0oWz0BIuPcV2UIuKi7+N09R6AkbOUDXXUDzQwAAAQErAPIFKgEAAAAiUSCfbbX+FHJbzC71eEFLsMjDouMJbu8ogeR0eNoNxMM9CwEIQwFBeyOLUebV/YwpaLTpLIaTXaSiPS7Dn6o39X4nlUzQLfb6YyvCAsLA5GTxo+Zb0NUINZ8DaRyUWknOpU/Jzuwn2gEAAAA=").unwrap(),
            params: Params::default(),
            change_vout: 0,
            receiver_inputs: vec![
                InputPair::new(
                    TxIn {
                        previous_output: OutPoint::from_str("1e1046edb66f1039d22c4be274a4afc1f6c6e9da65e2eb639eff29c62e843f17:0").unwrap(),
                        ..Default::default()
                    }, Input {
                        witness_utxo: Some(TxOut {
                            value: Amount::from_sat(5_000_000_000),
                            script_pubkey: ScriptBuf::from_hex("51209f6db5fe14725bcc2ef578414bb0c8c3a2e3096eef2881e47478da0dc4c33d0b").unwrap(),
                        }),
                        ..Default::default()
                    }, None)
                .unwrap()],
        };
        assert_eq!(
            p2tr_proposal.additional_input_weight().expect("should calculate input weight"),
            Weight::from_wu(230)
        );
    }

    #[test]
    fn test_pjos_disabled() {
        let mut proposal = unchecked_proposal_from_test_vector();
        proposal.params.output_substitution = OutputSubstitution::Disabled;
        let wants_outputs = wants_outputs_from_test_vector(proposal);
        let script_pubkey = &wants_outputs.original_psbt.unsigned_tx.output
            [wants_outputs.change_vout]
            .script_pubkey;

        let output_value =
            wants_outputs.original_psbt.unsigned_tx.output[wants_outputs.change_vout].value
                + Amount::ONE_SAT;
        let outputs = vec![TxOut { value: output_value, script_pubkey: script_pubkey.clone() }];
        let increased_amount =
            wants_outputs.clone().replace_receiver_outputs(outputs, script_pubkey.as_script());
        assert!(
            increased_amount.is_ok(),
            "Increasing the receiver output amount should always be allowed"
        );
        assert_ne!(wants_outputs.payjoin_psbt, increased_amount.unwrap().payjoin_psbt);

        let output_value =
            wants_outputs.original_psbt.unsigned_tx.output[wants_outputs.change_vout].value
                - Amount::ONE_SAT;
        let outputs = vec![TxOut { value: output_value, script_pubkey: script_pubkey.clone() }];
        let decreased_amount =
            wants_outputs.clone().replace_receiver_outputs(outputs, script_pubkey.as_script());
        assert_eq!(
            decreased_amount.unwrap_err(),
            OutputSubstitutionError::from(
                InternalOutputSubstitutionError::DecreasedValueWhenDisabled
            ),
            "Payjoin receiver amount has been decreased and should error"
        );

        let script = Script::new();
        let replace_receiver_script_pubkey = wants_outputs.substitute_receiver_script(script);
        assert_eq!(
            replace_receiver_script_pubkey.unwrap_err(),
            OutputSubstitutionError::from(
                InternalOutputSubstitutionError::ScriptPubKeyChangedWhenDisabled
            ),
            "Payjoin receiver script pubkey has been modified and should error"
        );
    }

    #[test]
    fn test_avoid_uih_one_output() {
        let proposal = unchecked_proposal_from_test_vector();
        let proposal_psbt = Psbt::from_str(RECEIVER_INPUT_CONTRIBUTION).unwrap();
        let input = InputPair::new(
            proposal_psbt.unsigned_tx.input[1].clone(),
            proposal_psbt.inputs[1].clone(),
            None,
        )
        .unwrap();
        let input_iter = [input].into_iter();
        let mut payjoin = wants_outputs_from_test_vector(proposal)
            .commit_outputs()
            .contribute_inputs(input_iter.clone())
            .expect("Failed to contribute inputs");

        payjoin.payjoin_psbt.outputs.pop();
        let avoid_uih = payjoin.avoid_uih(input_iter);
        assert_eq!(
            avoid_uih.unwrap_err(),
            SelectionError::from(InternalSelectionError::UnsupportedOutputLength),
            "Payjoin below minimum allowed outputs for avoid uih and should error"
        );
    }

    #[test]
    fn test_interleave_shuffle() {
        let mut original1 = vec![1, 2, 3];
        let mut original2 = original1.clone();
        let mut original3 = original1.clone();
        let mut new1 = vec![4, 5, 6];
        let mut new2 = new1.clone();
        let mut new3 = new1.clone();
        let mut rng1 = StdRng::seed_from_u64(123);
        let mut rng2 = StdRng::seed_from_u64(234);
        let mut rng3 = StdRng::seed_from_u64(345);
        // Operate on the same data multiple times with different RNG seeds.
        interleave_shuffle(&mut original1, &mut new1, &mut rng1);
        interleave_shuffle(&mut original2, &mut new2, &mut rng2);
        interleave_shuffle(&mut original3, &mut new3, &mut rng3);
        // The result should be different for each seed
        // and the relative ordering from `original` always preserved/
        assert_eq!(original1, vec![1, 6, 2, 5, 4, 3]);
        assert_eq!(original2, vec![1, 5, 4, 2, 6, 3]);
        assert_eq!(original3, vec![4, 5, 1, 2, 6, 3]);
    }

    /// Add keypath data to psbt to be prepared and verify it is excluded from the final PSBT
    /// See: <https://github.com/bitcoin/bips/blob/master/bip-0078.mediawiki#senders-payjoin-proposal-checklist>
    #[test]
    fn test_prepare_psbt_excludes_keypaths() {
        let proposal = unchecked_proposal_from_test_vector();
        let mut processed_psbt = proposal.psbt.clone();

        let secp = Secp256k1::new();
        let (_, pk) = secp.generate_keypair(&mut bitcoin::key::rand::thread_rng());
        let xpriv = Xpriv::new_master(Network::Bitcoin, &[]).expect("Could not generate new xpriv");
        let (x_only, _) = pk.x_only_public_key();

        processed_psbt.xpub.insert(
            Xpub::from_priv(&secp, &xpriv),
            (Fingerprint::default(), DerivationPath::default()),
        );

        for input in &mut processed_psbt.inputs {
            input.bip32_derivation.insert(pk, (Fingerprint::default(), DerivationPath::default()));
            input.tap_key_origins.insert(
                x_only,
                (
                    vec![TapLeafHash::from_script(&ScriptBuf::new(), LeafVersion::TapScript)],
                    (Fingerprint::default(), DerivationPath::default()),
                ),
            );
            input.tap_internal_key = Some(x_only);
        }

        for output in &mut processed_psbt.outputs {
            output.bip32_derivation.insert(pk, (Fingerprint::default(), DerivationPath::default()));
            output.tap_key_origins.insert(
                x_only,
                (
                    vec![TapLeafHash::from_script(&ScriptBuf::new(), LeafVersion::TapScript)],
                    (Fingerprint::default(), DerivationPath::default()),
                ),
            );
            output.tap_internal_key = Some(x_only);
        }

        let provisional = provisional_proposal_from_test_vector(proposal);
        let payjoin_proposal = provisional.prepare_psbt(processed_psbt);

        assert!(payjoin_proposal.payjoin_psbt.xpub.is_empty());

        for input in &payjoin_proposal.payjoin_psbt.inputs {
            assert!(input.bip32_derivation.is_empty());
            assert!(input.tap_key_origins.is_empty());
            assert!(input.tap_internal_key.is_none());
        }

        for output in &payjoin_proposal.payjoin_psbt.outputs {
            assert!(output.bip32_derivation.is_empty());
            assert!(output.tap_key_origins.is_empty());
            assert!(output.tap_internal_key.is_none());
        }
    }

    #[test]
    fn test_multiple_contribute_inputs() {
        let proposal = unchecked_proposal_from_test_vector();
        let wants_inputs = wants_outputs_from_test_vector(proposal).commit_outputs();
        let txout = TxOut {
            value: Amount::from_sat(123),
            script_pubkey: ScriptBuf::new_p2pkh(&PubkeyHash::from_byte_array(DUMMY20)),
        };
        let tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: LockTime::Seconds(Time::MIN),
            input: vec![],
            output: vec![txout.clone()],
        };
        let ot1 = OutPoint { txid: tx.compute_txid(), vout: 0 };
        let ot2 = OutPoint { txid: tx.compute_txid(), vout: 1 };

        let input_pair_1 = InputPair::new(
            TxIn { previous_output: ot1, sequence: Sequence::MAX, ..Default::default() },
            Input { witness_utxo: Some(txout.clone()), ..Default::default() },
            None,
        )
        .unwrap();
        let input_pair_2 = InputPair::new(
            TxIn { previous_output: ot2, sequence: Sequence::MAX, ..Default::default() },
            Input { witness_utxo: Some(txout), ..Default::default() },
            None,
        )
        .unwrap();

        let wants_inputs = wants_inputs.contribute_inputs(vec![input_pair_1.clone()]).unwrap();
        assert_eq!(wants_inputs.receiver_inputs.len(), 1);
        assert_eq!(wants_inputs.receiver_inputs[0], input_pair_1);
        // Contribute the same input again, and a new input.
        // TODO: if we ever decide to fix contribute duplicate inputs, we need to update this test.
        let wants_inputs = wants_inputs
            .contribute_inputs(vec![input_pair_2.clone(), input_pair_1.clone()])
            .unwrap();
        assert_eq!(wants_inputs.receiver_inputs.len(), 3);
        assert_eq!(wants_inputs.receiver_inputs[0], input_pair_1);
        assert_eq!(wants_inputs.receiver_inputs[1], input_pair_2);
        assert_eq!(wants_inputs.receiver_inputs[2], input_pair_1);
    }
}
