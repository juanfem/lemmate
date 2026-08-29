//! Stable identifiers. Notes are identified by ULID; paths are mutable attributes (SPEC §4.4).

use std::fmt;
use std::str::FromStr;

use ulid::Ulid;

use crate::error::{Error, Result};

macro_rules! ulid_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub Ulid);

        impl $name {
            pub fn new() -> Self {
                Self(Ulid::generate())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = Error;
            fn from_str(s: &str) -> Result<Self> {
                Ulid::from_string(s).map(Self).map_err(|e| Error::Id(format!("{s}: {e}")))
            }
        }
    };
}

ulid_id!(NoteId);
ulid_id!(VaultId);

/// Identifies a CRDT document in the update log: either a note doc or the vault doc (SPEC §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocId {
    Note(NoteId),
    Vault(VaultId),
}

impl fmt::Display for DocId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocId::Note(id) => write!(f, "{id}"),
            DocId::Vault(id) => write!(f, "vault:{id}"),
        }
    }
}

impl FromStr for DocId {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.strip_prefix("vault:") {
            Some(rest) => rest.parse().map(DocId::Vault),
            None => s.parse().map(DocId::Note),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_id_round_trips() {
        let n = DocId::Note(NoteId::new());
        let v = DocId::Vault(VaultId::new());
        assert_eq!(n.to_string().parse::<DocId>().unwrap(), n);
        assert_eq!(v.to_string().parse::<DocId>().unwrap(), v);
        assert!(v.to_string().starts_with("vault:"));
        assert!("nope".parse::<DocId>().is_err());
    }
}
