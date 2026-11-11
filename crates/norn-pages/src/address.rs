//! FORK-ADDRESS-1: keyed per-tenant content addresses.
//!
//! A tenant's page and node addresses are a keyed MAC (keyed BLAKE3) of the
//! content under that tenant's key, so addresses are never comparable across
//! tenants: storage accounting, latency and upload responses cannot reveal
//! whether another tenant holds the same page. Dedupe is therefore within one
//! tenant only, across all of its cells, forks and worlds.
//!
//! The key is derived from a tenant secret with BLAKE3 `derive_key` under a
//! fixed context string. The secret and the key live in the host's secret
//! store and are never written to a cell file or a page. Rotating a tenant's
//! secret changes every address, so dedupe restarts for that tenant; old and
//! new addresses never alias.

use std::fmt;

/// The fixed `derive_key` context for tenant page-address keys. Changing it
/// changes every address.
pub const KEY_CONTEXT: &str = "norn-pages 2026-11-11 tenant page address key v1";

/// A tenant's page-address key. Its bytes are never displayed.
#[derive(Clone)]
pub struct TenantKey([u8; 32]);

impl TenantKey {
    /// Derive the address key from a tenant secret held by the host's secret
    /// store.
    #[must_use]
    pub fn derive(tenant_secret: &[u8]) -> Self {
        Self(blake3::derive_key(KEY_CONTEXT, tenant_secret))
    }

    pub(crate) fn address(&self, domain: Domain, parts: &[&[u8]]) -> Address {
        let mut hasher = blake3::Hasher::new_keyed(&self.0);
        hasher.update(&[domain.tag()]);
        for part in parts {
            hasher.update(part);
        }
        Address(*hasher.finalize().as_bytes())
    }
}

impl fmt::Debug for TenantKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TenantKey(..)")
    }
}

/// What an address names; part of the MAC input, so a page and a node with
/// equal bytes never share an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Domain {
    Page,
    Node,
}

impl Domain {
    const fn tag(self) -> u8 {
        match self {
            Self::Page => 0,
            Self::Node => 1,
        }
    }
}

/// A keyed content address: 32 bytes of keyed BLAKE3.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address(pub(crate) [u8; 32]);

impl Address {
    /// The address bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(crate) fn shard(&self, shards: usize) -> usize {
        usize::from(self.0.first().copied().unwrap_or(0))
            .checked_rem(shards)
            .unwrap_or(0)
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0.iter().take(8) {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
