use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::ops::Deref;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Base64Bytes(Bytes);

impl Base64Bytes {
    pub fn into_inner(self) -> Vec<u8> {
        self.0.to_vec()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for Base64Bytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(Bytes::from(bytes))
    }
}

impl From<&[u8]> for Base64Bytes {
    fn from(bytes: &[u8]) -> Self {
        Self(Bytes::copy_from_slice(bytes))
    }
}

impl FromStr for Base64Bytes {
    type Err = base64::DecodeError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        STANDARD
            .decode(input.as_bytes())
            .map(|bytes| Self(Bytes::from(bytes)))
    }
}

impl AsRef<[u8]> for Base64Bytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Deref for Base64Bytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Display for Base64Bytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&STANDARD.encode(self.0.as_ref()))
    }
}

impl Serialize for Base64Bytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(self.0.as_ref()))
    }
}

impl<'de> Deserialize<'de> for Base64Bytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let bytes = STANDARD
            .decode(encoded.as_bytes())
            .map_err(serde::de::Error::custom)?;
        Ok(Self(Bytes::from(bytes)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_base64() {
        let bytes = Base64Bytes::from(vec![1, 2, 3]);
        assert_eq!(serde_json::to_string(&bytes).unwrap(), "\"AQID\"");
        assert_eq!(
            serde_json::from_str::<Base64Bytes>("\"AQID\"").unwrap(),
            bytes
        );
    }

    #[test]
    fn rejects_invalid_base64() {
        assert!(serde_json::from_str::<Base64Bytes>("\"***\"").is_err());
    }
}
