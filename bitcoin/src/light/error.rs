use crate::{error::ElectrsError, secp256k1::Error as Secp256k1Error, util::address::Error as BitcoinAddressError};
use std::sync::PoisonError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Cannot use invalid prevout")]
    InvalidPrevOut,
    #[error("Cannot construct address")]
    InvalidAddress,
    #[error("No private key found for address")]
    NoPrivateKey,
    #[error("Not enough inputs to fund transaction")]
    NotEnoughInputs,
    #[error("No change address available")]
    NoChangeAddress,
    #[error("Cannot open key store")]
    CannotOpenKeyStore,

    #[error("Secp256k1Error: {0}")]
    Secp256k1Error(#[from] Secp256k1Error),
    #[error("BitcoinAddressError: {0}")]
    BitcoinAddressError(#[from] BitcoinAddressError),

    #[error("ElectrsError: {0}")]
    ElectrsError(#[from] ElectrsError),
}

impl<T> From<PoisonError<T>> for Error {
    fn from(_: PoisonError<T>) -> Self {
        Self::CannotOpenKeyStore
    }
}
