use bitcoin::{psbt, AddressType, TxIn, TxOut};
pub(crate) use error::InternalPayloadError;
pub use error::{Error, JsonError, OutputSubstitutionError, PayloadError, SelectionError};

pub use crate::psbt::PsbtInputError;
use crate::psbt::{InternalInputPair, InternalPsbtInputError};

mod error;
pub(crate) mod optional_parameters;

#[cfg(feature = "v1")]
pub mod v1;
#[cfg(not(feature = "v1"))]
pub(crate) mod v1;

#[cfg(feature = "v2")]
pub mod v2;

/// Helper to construct a pair of (txin, psbtin) with some built-in validation
/// Use with [`InputPair::new`] to contribute receiver inputs.
#[derive(Clone, Debug)]
pub struct InputPair {
    pub(crate) txin: TxIn,
    pub(crate) psbtin: psbt::Input,
}

impl InputPair {
    pub fn new(txin: TxIn, psbtin: psbt::Input) -> Result<Self, PsbtInputError> {
        let input_pair = Self { txin, psbtin };
        let raw = InternalInputPair::from(&input_pair);
        raw.validate_utxo(true)?;
        let address_type = raw.address_type().map_err(InternalPsbtInputError::AddressType)?;
        if address_type == AddressType::P2sh && input_pair.psbtin.redeem_script.is_none() {
            return Err(InternalPsbtInputError::NoRedeemScript.into());
        }
        Ok(input_pair)
    }

    pub(crate) fn previous_txout(&self) -> TxOut {
        InternalInputPair::from(self)
            .previous_txout()
            .expect("UTXO information should have been validated in InputPair::new")
            .clone()
    }
}

impl<'a> From<&'a InputPair> for InternalInputPair<'a> {
    fn from(pair: &'a InputPair) -> Self { Self { psbtin: &pair.psbtin, txin: &pair.txin } }
}
