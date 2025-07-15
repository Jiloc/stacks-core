use crate::vm::errors::Error;

pub trait ClaritySerializable {
    fn serialize(&self) -> String;
}

pub trait ClarityDeserializable<T> {
    fn deserialize(json: &str) -> Result<T, Error>;
}

impl ClaritySerializable for String {
    fn serialize(&self) -> String {
        self.into()
    }
}

impl ClarityDeserializable<String> for String {
    fn deserialize(serialized: &str) -> Result<String, Error> {
        Ok(serialized.into())
    }
}

/// Macro to implement ClaritySerializable and ClarityDeserializable for types
/// that can be serialized/deserialized using serde_json
#[macro_export]
macro_rules! clarity_serializable {
    ($Name:ident) => {
        impl ClaritySerializable for $Name {
            fn serialize(&self) -> String {
                serde_json::to_string(self).expect("Failed to serialize vm.Value")
            }
        }
        impl ClarityDeserializable<$Name> for $Name {
            fn deserialize(json: &str) -> Result<Self> {
                let mut deserializer = serde_json::Deserializer::from_str(&json);
                // serde's default 128 depth limit can be exhausted
                //  by a 64-stack-depth AST, so disable the recursion limit
                deserializer.disable_recursion_limit();
                // use stacker to prevent the deserializer from overflowing.
                //  this will instead spill to the heap
                let deserializer = serde_stacker::Deserializer::new(&mut deserializer);
                Deserialize::deserialize(deserializer).map_err(|_| {
                    InterpreterError::Expect("Failed to deserialize vm.Value".into()).into()
                })
            }
        }
    };
}
