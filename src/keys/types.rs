use std::borrow::Cow;
use std::convert::Infallible;
use std::fmt::{self, Debug, Display, Formatter};
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::ptr;
use std::str::FromStr;

use indy_utils::keys::{EncodedVerKey, KeyType as IndyKeyAlg, PrivateKey, VerKey};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::error::Error;
use crate::types::{sorted_tags, EntryTag, SecretBytes};

/// Supported key algorithms
#[derive(Clone, Debug, PartialEq, Eq, Zeroize)]
pub enum KeyAlg {
    /// curve25519-based signature scheme
    ED25519,
    /// Unrecognized algorithm
    Other(String),
}

serde_as_str_impl!(KeyAlg);

impl KeyAlg {
    /// Get a reference to a string representing the `KeyAlg`
    pub fn as_str(&self) -> &str {
        match self {
            Self::ED25519 => "ed25519",
            Self::Other(other) => other.as_str(),
        }
    }
}

impl AsRef<str> for KeyAlg {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for KeyAlg {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "ed25519" => Self::ED25519,
            other => Self::Other(other.to_owned()),
        })
    }
}

impl Display for KeyAlg {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Categories of keys supported by the default KMS
#[derive(Clone, Debug, PartialEq, Eq, Zeroize)]
pub enum KeyCategory {
    /// A public key
    PublicKey,
    /// A combination of a public and private key
    KeyPair,
    /// An unrecognized key category
    Other(String),
}

impl KeyCategory {
    /// Get a reference to a string representing the `KeyCategory`
    pub fn as_str(&self) -> &str {
        match self {
            Self::PublicKey => "public",
            Self::KeyPair => "keypair",
            Self::Other(other) => other.as_str(),
        }
    }

    /// Convert the `KeyCategory` into an owned string
    pub fn into_string(self) -> String {
        match self {
            Self::Other(other) => other,
            _ => self.as_str().to_owned(),
        }
    }
}

serde_as_str_impl!(KeyCategory);

impl AsRef<str> for KeyCategory {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for KeyCategory {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "public" => Self::PublicKey,
            "keypair" => Self::KeyPair,
            other => Self::Other(other.to_owned()),
        })
    }
}

impl Display for KeyCategory {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parameters defining a stored key
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct KeyParams {
    /// The key algorithm
    pub alg: KeyAlg,

    /// Associated key metadata
    #[serde(default, rename = "meta", skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,

    /// An optional external reference for the key
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(
        default,
        rename = "pub",
        skip_serializing_if = "Option::is_none",
        with = "crate::serde_utils::as_base58"
    )]

    /// The associated public key in binary format
    pub pub_key: Option<Vec<u8>>,

    /// The associated private key in binary format
    #[serde(
        default,
        rename = "prv",
        skip_serializing_if = "Option::is_none",
        with = "crate::serde_utils::as_base58"
    )]
    pub prv_key: Option<SecretBytes>,
}

impl KeyParams {
    pub(crate) fn to_vec(&self) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(self)
            .map_err(|e| err_msg!(Unexpected, "Error serializing key params: {}", e))
    }

    pub(crate) fn from_slice(params: &[u8]) -> Result<KeyParams, Error> {
        let result = serde_json::from_slice(params)
            .map_err(|e| err_msg!(Unexpected, "Error deserializing key params: {}", e));
        result
    }
}

impl Drop for KeyParams {
    fn drop(&mut self) {
        self.zeroize()
    }
}

impl Zeroize for KeyParams {
    fn zeroize(&mut self) {
        self.prv_key.zeroize();
    }
}

/// A stored key entry
#[derive(Clone, Debug, Eq)]
pub struct KeyEntry {
    /// The category of the key entry (public or public/private pair)
    pub category: KeyCategory,
    /// The key entry identifier
    pub ident: String,
    /// The parameters defining the key
    pub params: KeyParams,
    /// Tags associated with the key entry record
    pub tags: Option<Vec<EntryTag>>,
}

impl KeyEntry {
    pub(crate) fn into_parts(self) -> (KeyCategory, String, KeyParams, Option<Vec<EntryTag>>) {
        let slf = ManuallyDrop::new(self);
        unsafe {
            (
                ptr::read(&slf.category),
                ptr::read(&slf.ident),
                ptr::read(&slf.params),
                ptr::read(&slf.tags),
            )
        }
    }

    /// Determine if a key entry refers to a local or external key
    pub fn is_local(&self) -> bool {
        self.params.reference.is_none()
    }

    /// Access the associated public key as an [`EncodedVerKey`]
    pub fn encoded_verkey(&self) -> Result<EncodedVerKey, Error> {
        Ok(self
            .verkey()?
            .as_base58()
            .map_err(err_map!(Unexpected, "Error encoding verkey"))?)
    }

    /// Access the associated public key as a [`VerKey`]
    pub fn verkey(&self) -> Result<VerKey, Error> {
        match (&self.params.alg, &self.params.pub_key) {
            (KeyAlg::ED25519, Some(pub_key)) => Ok(VerKey::new(pub_key, Some(IndyKeyAlg::ED25519))),
            (_, None) => Err(err_msg!(Input, "Undefined public key")),
            _ => Err(err_msg!(Unsupported, "Unsupported key algorithm")),
        }
    }

    /// Access the associated private key as a [`PrivateKey`]
    pub fn private_key(&self) -> Result<PrivateKey, Error> {
        match (&self.params.alg, &self.params.prv_key) {
            (KeyAlg::ED25519, Some(prv_key)) => {
                Ok(PrivateKey::new(prv_key, Some(IndyKeyAlg::ED25519)))
            }
            (_, None) => Err(err_msg!(Input, "Undefined private key")),
            _ => Err(err_msg!(Unsupported, "Unsupported key algorithm")),
        }
    }

    pub(crate) fn sorted_tags(&self) -> Option<Vec<&EntryTag>> {
        self.tags.as_ref().and_then(sorted_tags)
    }
}

impl PartialEq for KeyEntry {
    fn eq(&self, rhs: &Self) -> bool {
        self.category == rhs.category
            && self.ident == rhs.ident
            && self.params == rhs.params
            && self.sorted_tags() == rhs.sorted_tags()
    }
}

/// A possibly-empty password or key used to derive a store wrap key
#[derive(Clone)]
pub struct PassKey<'a>(Option<Cow<'a, str>>);

impl PassKey<'_> {
    /// Create a scoped reference to the passkey
    pub fn as_ref(&self) -> PassKey<'_> {
        PassKey(Some(Cow::Borrowed(&**self)))
    }

    pub(crate) fn is_none(&self) -> bool {
        self.0.is_none()
    }

    pub(crate) fn into_owned(self) -> PassKey<'static> {
        let mut slf = ManuallyDrop::new(self);
        let val = slf.0.take();
        PassKey(match val {
            None => None,
            Some(Cow::Borrowed(s)) => Some(Cow::Owned(s.to_string())),
            Some(Cow::Owned(s)) => Some(Cow::Owned(s)),
        })
    }
}

impl Debug for PassKey<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if cfg!(test) {
            f.debug_tuple("PassKey").field(&*self).finish()
        } else {
            f.debug_tuple("PassKey").field(&"<secret>").finish()
        }
    }
}

impl Default for PassKey<'_> {
    fn default() -> Self {
        Self(None)
    }
}

impl Deref for PassKey<'_> {
    type Target = str;

    fn deref(&self) -> &str {
        match self.0.as_ref() {
            None => "",
            Some(s) => s.as_ref(),
        }
    }
}

impl Drop for PassKey<'_> {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl<'a> From<&'a str> for PassKey<'a> {
    fn from(inner: &'a str) -> Self {
        Self(Some(Cow::Borrowed(inner)))
    }
}

impl From<String> for PassKey<'_> {
    fn from(inner: String) -> Self {
        Self(Some(Cow::Owned(inner)))
    }
}

impl<'a> From<Option<&'a str>> for PassKey<'a> {
    fn from(inner: Option<&'a str>) -> Self {
        Self(inner.map(Cow::Borrowed))
    }
}

impl<'a, 'b> PartialEq<PassKey<'b>> for PassKey<'a> {
    fn eq(&self, other: &PassKey<'b>) -> bool {
        &**self == &**other
    }
}
impl Eq for PassKey<'_> {}

impl Zeroize for PassKey<'_> {
    fn zeroize(&mut self) {
        match self.0.take() {
            Some(Cow::Owned(mut s)) => {
                s.zeroize();
            }
            _ => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_params_roundtrip() {
        let params = KeyParams {
            alg: KeyAlg::ED25519,
            metadata: Some("meta".to_string()),
            reference: None,
            pub_key: Some(vec![0, 0, 0, 0]),
            prv_key: Some(vec![1, 1, 1, 1].into()),
        };
        let enc_params = params.to_vec().unwrap();
        let p2 = KeyParams::from_slice(&enc_params).unwrap();
        assert_eq!(p2, params);
    }
}
