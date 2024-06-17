use cosmwasm_std::{OverflowError, StdError};
use thiserror::Error;

/// ## Description
/// This enum describes Assembly contract errors!
#[derive(Error, Debug, PartialEq)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("Unauthorized")]
    Unauthorized {},

    #[error("Need 20_000_000 ufury to instantiate")]
    NeedOsmo {},

    #[error("Deposit period over")]
    DepositsOver {},

    #[error("Withdrawal period over")]
    WithdrawalsOver {},

    #[error("The lockdrop hasn't ended")]
    LockdropOngoing {},

    #[error("The launch has previously been initiated")]
    LaunchHappened {},

    #[error("No user funds in the contract")]
    NotAUser {},

    #[error("Custom Error val: {val}")]
    CustomError { val: String },

}

impl From<OverflowError> for ContractError {
    fn from(o: OverflowError) -> Self {
        StdError::from(o).into()
    }
}
