use std::io;

use serde::{de::DeserializeOwned, Serialize};

pub type EncodeResult = Result<Vec<u8>, io::Error>;
pub type DecodeResult<T> = Result<T, io::Error>;

// encode a type to byte array
pub trait Encode {
    fn encode(&self) -> EncodeResult;
}

// decode from byte array
pub trait Decode: Sized {
    fn decode(data: &[u8]) -> DecodeResult<Self>;
}

impl<T: Serialize> Encode for T {
    fn encode(&self) -> EncodeResult {
        bincode::serialize(&self)
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "Failed to serialize"))
    }
}

impl<T> Decode for T
where
    T: DeserializeOwned,
{
    fn decode(data: &[u8]) -> DecodeResult<Self> {
        bincode::deserialize(data)
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "Failed to deserialize"))
    }
}

pub fn to_base64<T: ?Sized + AsRef<[u8]>>(input: &T) -> String {
    base64::encode(input)
}

pub fn from_base64(s: &str) -> Result<Vec<u8>, Box<std::error::Error>> {
    base64::decode(s).map_err(|err| err.into())
}

pub fn from_base64_buf(s: &str, buffer: &mut Vec<u8>) -> Result<(), Box<std::error::Error>> {
    base64::decode_config_buf(s, base64::STANDARD, buffer).map_err(|err| err.into())
}
