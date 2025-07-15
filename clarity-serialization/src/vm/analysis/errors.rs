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

use crate::types::{TupleTypeSignature, TypeSignature, Value};
use crate::vm::costs::CostErrors;

pub enum TypeImplError {
    SupertypeTooLarge,
    ListTypesMustMatch,
    TypeValueError(TypeSignature, Value),
    NoSuchTupleField(String, TupleTypeSignature),
}

#[derive(Debug, PartialEq)]
pub enum CheckErrors {
    // // cost checker errors
    // CostOverflow,
    // CostBalanceExceeded(ExecutionCost, ExecutionCost),
    // MemoryBalanceExceeded(u64, u64),
    // CostComputationFailed(String),
    ValueTooLarge,
    ValueOutOfBounds,
    TypeSignatureTooDeep,
    // ExpectedName,
    SupertypeTooLarge,

    // unexpected interpreter behavior
    Expects(String),
    // // match errors
    // BadMatchOptionSyntax(Box<CheckErrors>),
    // BadMatchResponseSyntax(Box<CheckErrors>),
    // BadMatchInput(TypeSignature),

    // // list typing errors
    // UnknownListConstructionFailure,
    ListTypesMustMatch,
    // ConstructedListTooLarge,

    // // simple type expectation mismatch
    TypeError(TypeSignature, TypeSignature),
    // TypeLiteralError(TypeSignature, TypeSignature),
    TypeValueError(TypeSignature, Value),

    // NoSuperType(TypeSignature, TypeSignature),
    // InvalidTypeDescription,
    // UnknownTypeName(String),

    // // union type mismatch
    // UnionTypeError(Vec<TypeSignature>, TypeSignature),
    // UnionTypeValueError(Vec<TypeSignature>, Value),

    // ExpectedLiteral,
    // ExpectedOptionalType(TypeSignature),
    // ExpectedResponseType(TypeSignature),
    // ExpectedOptionalOrResponseType(TypeSignature),
    // ExpectedOptionalValue(Value),
    // ExpectedResponseValue(Value),
    // ExpectedOptionalOrResponseValue(Value),
    // CouldNotDetermineResponseOkType,
    // CouldNotDetermineResponseErrType,
    CouldNotDetermineSerializationType,
    // UncheckedIntermediaryResponses,

    // CouldNotDetermineMatchTypes,
    CouldNotDetermineType,
    // // Checker runtime failures
    // TypeAlreadyAnnotatedFailure,
    // TypeAnnotationExpectedFailure,
    // CheckerImplementationFailure,

    // // Assets
    // BadTokenName,
    // DefineFTBadSignature,
    // DefineNFTBadSignature,
    // NoSuchNFT(String),
    // NoSuchFT(String),

    // BadTransferSTXArguments,
    // BadTransferFTArguments,
    // BadTransferNFTArguments,
    // BadMintFTArguments,
    // BadBurnFTArguments,

    // // tuples
    // BadTupleFieldName,
    // ExpectedTuple(TypeSignature),
    NoSuchTupleField(String, TupleTypeSignature),
    EmptyTuplesNotAllowed,
    // BadTupleConstruction,
    // TupleExpectsPairs,

    // // variables
    // NoSuchDataVariable(String),

    // // data map
    // BadMapName,
    // NoSuchMap(String),

    // // defines
    // DefineFunctionBadSignature,
    // BadFunctionName,
    // BadMapTypeDefinition,
    // PublicFunctionMustReturnResponse(TypeSignature),
    // DefineVariableBadSignature,
    // ReturnTypesMustMatch(TypeSignature, TypeSignature),

    // CircularReference(Vec<String>),

    // // contract-call errors
    // NoSuchContract(String),
    // NoSuchPublicFunction(String, String),
    // PublicFunctionNotReadOnly(String, String),
    // ContractAlreadyExists(String),
    // ContractCallExpectName,
    // ExpectedCallableType(TypeSignature),

    // // get-block-info? errors
    // NoSuchBlockInfoProperty(String),
    // NoSuchBurnBlockInfoProperty(String),
    // NoSuchStacksBlockInfoProperty(String),
    // NoSuchTenureInfoProperty(String),
    // GetBlockInfoExpectPropertyName,
    // GetBurnBlockInfoExpectPropertyName,
    // GetStacksBlockInfoExpectPropertyName,
    // GetTenureInfoExpectPropertyName,
    NameAlreadyUsed(String),
    // ReservedWord(String),

    // // expect a function, or applying a function to a list
    // NonFunctionApplication,
    // ExpectedListApplication,
    // ExpectedSequence(TypeSignature),
    // MaxLengthOverflow,

    // // let syntax
    // BadLetSyntax,

    // // generic binding syntax
    // BadSyntaxBinding,
    // BadSyntaxExpectedListOfPairs,

    // MaxContextDepthReached,
    // UndefinedFunction(String),
    // UndefinedVariable(String),

    // // argument counts
    // RequiresAtLeastArguments(usize, usize),
    // RequiresAtMostArguments(usize, usize),
    // IncorrectArgumentCount(usize, usize),
    // IfArmsMustMatch(TypeSignature, TypeSignature),
    // MatchArmsMustMatch(TypeSignature, TypeSignature),
    // DefaultTypesMustMatch(TypeSignature, TypeSignature),
    // TooManyExpressions,
    // IllegalOrUnknownFunctionApplication(String),
    // UnknownFunction(String),

    // // traits
    // NoSuchTrait(String, String),
    // TraitReferenceUnknown(String),
    // TraitMethodUnknown(String, String),
    // ExpectedTraitIdentifier,
    // ImportTraitBadSignature,
    // TraitReferenceNotAllowed,
    // BadTraitImplementation(String, String),
    // DefineTraitBadSignature,
    // DefineTraitDuplicateMethod(String),
    // UnexpectedTraitOrFieldReference,
    // TraitBasedContractCallInReadOnly,
    // ContractOfExpectsTrait,
    // IncompatibleTrait(TraitIdentifier, TraitIdentifier),

    // strings
    InvalidCharactersDetected,
    InvalidUTF8Encoding,
    // // secp256k1 signature
    // InvalidSecp65k1Signature,

    // WriteAttemptedInReadOnly,
    // AtBlockClosureMustBeReadOnly,

    // // time checker errors
    // ExecutionTimeExpired,
}

#[derive(Debug, PartialEq)]
pub struct CheckError {
    pub err: CheckErrors,
}

// impl CheckErrors {
//     /// Does this check error indicate that the transaction should be
//     /// rejected?
//     pub fn rejectable(&self) -> bool {
//         matches!(
//             self,
//             CheckErrors::SupertypeTooLarge | CheckErrors::Expects(_)
//         )
//     }
// }

impl CheckError {
    pub fn new(err: CheckErrors) -> CheckError {
        CheckError { err }
    }
}

impl fmt::Display for CheckErrors {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.err)?;
        Ok(())
    }
}

// impl From<CostErrors> for CheckError {
//     fn from(err: CostErrors) -> Self {
//         CheckError::from(CheckErrors::from(err))
//     }
// }

impl From<CostErrors> for CheckErrors {
    fn from(err: CostErrors) -> Self {
        match err {
            // CostErrors::CostOverflow => CheckErrors::CostOverflow,
            // CostErrors::CostBalanceExceeded(a, b) => CheckErrors::CostBalanceExceeded(a, b),
            // CostErrors::MemoryBalanceExceeded(a, b) => CheckErrors::MemoryBalanceExceeded(a, b),
            // CostErrors::CostComputationFailed(s) => CheckErrors::CostComputationFailed(s),
            // CostErrors::CostContractLoadFailure => {
            //     CheckErrors::CostComputationFailed("Failed to load cost contract".into())
            // }
            CostErrors::InterpreterFailure => {
                CheckErrors::Expects("Unexpected interpreter failure in cost computation".into())
            }
            CostErrors::Expect(s) => CheckErrors::Expects(s),
            // CostErrors::ExecutionTimeExpired => CheckErrors::ExecutionTimeExpired,
        }
    }
}

// impl error::Error for CheckError {
//     fn source(&self) -> Option<&(dyn error::Error + 'static)> {
//         None
//     }
// }

impl error::Error for CheckErrors {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        None
    }
}

// impl From<CheckErrors> for CheckError {
//     fn from(err: CheckErrors) -> Self {
//         CheckError::new(err)
//     }
// }

// pub fn check_argument_count<T>(expected: usize, args: &[T]) -> Result<(), CheckErrors> {
//     if args.len() != expected {
//         Err(CheckErrors::IncorrectArgumentCount(expected, args.len()))
//     } else {
//         Ok(())
//     }
// }

// pub fn check_arguments_at_least<T>(expected: usize, args: &[T]) -> Result<(), CheckErrors> {
//     if args.len() < expected {
//         Err(CheckErrors::RequiresAtLeastArguments(expected, args.len()))
//     } else {
//         Ok(())
//     }
// }

// pub fn check_arguments_at_most<T>(expected: usize, args: &[T]) -> Result<(), CheckErrors> {
//     if args.len() > expected {
//         Err(CheckErrors::RequiresAtMostArguments(expected, args.len()))
//     } else {
//         Ok(())
//     }
// }
