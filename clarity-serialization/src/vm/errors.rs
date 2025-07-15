// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020 Stacks Open Internet Foundation
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use std::{error, fmt};

pub use crate::vm::analysis::errors::CheckErrors;
use crate::vm::costs::CostErrors;

#[derive(Debug)]
pub struct IncomparableError<T> {
    pub err: T,
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Error {
    /// UncheckedErrors are errors that *should* be caught by the
    ///   TypeChecker and other check passes. Test executions may
    ///   trigger these errors.
    Unchecked(CheckErrors),
    Interpreter(InterpreterError),
    Runtime(RuntimeErrorType),
}

/// InterpreterErrors are errors that *should never* occur.
/// Test executions may trigger these errors.
#[derive(Debug, PartialEq)]
pub enum InterpreterError {
    // BadSender(Value),
    // BadSymbolicRepresentation(String),
    // InterpreterError(String),
    // UninitializedPersistedVariable,
    // FailedToConstructAssetTable,
    // FailedToConstructEventBatch,
    // BadFileName,
    // FailedToCreateDataDirectory,
    // MarfFailure(String),
    FailureConstructingTupleWithType,
    FailureConstructingListWithType,
    // InsufficientBalance,
    // CostContractLoadFailure,
    // DBError(String),
    Expect(String),
}

/// RuntimeErrors are errors that smart contracts are expected
///   to be able to trigger during execution (e.g., arithmetic errors)
#[derive(Debug, PartialEq)]
pub enum RuntimeErrorType {
    // error in parsing types
    ParseError(String),
    BadTypeConstruction,
    NotImplemented,
    BadNameValue(&'static str, String),
}

pub type InterpreterResult<R> = Result<R, Error>;

impl<T> PartialEq<IncomparableError<T>> for IncomparableError<T> {
    fn eq(&self, _other: &IncomparableError<T>) -> bool {
        false
    }
}

impl PartialEq<Error> for Error {
    fn eq(&self, other: &Error) -> bool {
        match (self, other) {
            (Error::Runtime(x), Error::Runtime(y)) => x == y,
            (Error::Unchecked(x), Error::Unchecked(y)) => x == y,
            (Error::Interpreter(x), Error::Interpreter(y)) => x == y,
            _ => false,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Runtime(err) => {
                write!(f, "{err}")?;
                Ok(())
            }
            _ => write!(f, "{self:?}"),
        }
    }
}

impl fmt::Display for RuntimeErrorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        None
    }
}

impl error::Error for RuntimeErrorType {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        None
    }
}

// impl From<ParseError> for Error {
//     fn from(err: ParseError) -> Self {
//         match &err.err {
//             ParseErrors::InterpreterFailure => Error::from(InterpreterError::Expect(
//                 "Unexpected interpreter failure during parsing".into(),
//             )),
//             _ => Error::from(RuntimeErrorType::ASTError(err)),
//         }
//     }
// }

impl From<CostErrors> for Error {
    fn from(err: CostErrors) -> Self {
        match err {
            CostErrors::InterpreterFailure => Error::from(InterpreterError::Expect(
                "Interpreter failure during cost calculation".into(),
            )),
            CostErrors::Expect(s) => Error::from(InterpreterError::Expect(format!(
                "Interpreter failure during cost calculation: {s}"
            ))),
        }
    }
}

impl From<RuntimeErrorType> for Error {
    fn from(err: RuntimeErrorType) -> Self {
        Error::Runtime(err)
    }
}

impl From<CheckErrors> for Error {
    fn from(err: CheckErrors) -> Self {
        Error::Unchecked(err)
    }
}

impl From<InterpreterError> for Error {
    fn from(err: InterpreterError) -> Self {
        Error::Interpreter(err)
    }
}

// #[cfg(test)]
// mod test {
//     use super::*;

//     #[test]
//     fn equality() {
//         assert_eq!(
//             Error::Interpreter(InterpreterError::InterpreterError("".to_string())),
//             Error::Interpreter(InterpreterError::InterpreterError("".to_string()))
//         );
//     }
// }
