//! The consent token.
//!
//! Single use, in memory only, minted by the gate and by nothing else. It is neither
//! `Clone` nor serialisable: either would void "spent on first use" and "does not survive
//! the process" without a runtime test noticing. Every execution entry point takes it by
//! value, so a second use is a compile error (`E0382`), and `crates/core/src/lib.rs` pins
//! that with `compile_fail` doctests.

use std::sync::Arc;

use super::request::Request;

/// Identity of one token, for the gate's spent set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TokenId(pub(crate) u64);

/// Whether the token came from a dialog or from policy. The executor cannot tell the two
/// apart and does not need to; the record does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The user answered a dialog.
    Consent,
    /// Policy auto-ran the request; no presenter was asked.
    Policy,
}

/// Proof that the gate approved exactly the [`Request`] it holds.
#[derive(Debug)]
pub struct ConsentToken {
    pub(crate) id: TokenId,
    pub(crate) request: Arc<Request>,
    pub(crate) origin: Origin,
}

impl ConsentToken {
    /// Only [`super::Consent`] calls this.
    pub(crate) fn mint(id: TokenId, request: Arc<Request>, origin: Origin) -> Self {
        ConsentToken {
            id,
            request,
            origin,
        }
    }

    pub fn request(&self) -> &Request {
        &self.request
    }

    pub fn origin(&self) -> Origin {
        self.origin
    }
}

/// A token the gate has redeemed: checked, spent, and ready for exactly one execution.
/// Only the entry points in `crate::execute` receive one, and only from
/// [`super::Consent::redeem`].
#[derive(Debug)]
pub struct Approved {
    pub(crate) request: Arc<Request>,
    pub(crate) origin: Origin,
}

impl Approved {
    pub fn request(&self) -> &Request {
        &self.request
    }

    pub fn origin(&self) -> Origin {
        self.origin
    }
}
