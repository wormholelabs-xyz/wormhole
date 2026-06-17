//! Zero-copy, on-disk account layouts and the tags that discriminate them.

use bytemuck::{Pod, Zeroable};

use crate::error::GlobalAccountantError;
use crate::primitives::{Pubkey, Uint256};

/// Account-type tag stored at offset 0 of every program-owned PDA layout.
///
/// Seeds namespace writes on-chain but are not recoverable from
/// `getProgramAccounts`, so off-chain consumers discriminate account types with
/// a single `memcmp(offset 0, [tag])` filter. On-chain, the load helpers compare
/// the tag as defense-in-depth against a handler that forgets to re-derive a PDA.
///
/// Values are append-only (same discipline as [`GlobalAccountantError`]); never
/// renumber once shipped. `0` is reserved: a freshly allocated account is all
/// zeroes, so zeroed data must never parse as a valid tag. The NTT accountant
/// port extends this space (4+).
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountTag {
    PendingObservations = 1,
    Balance = 2,
    ChainRegistration = 3,
    Modification = 4,
}

/// `ModifyBalance` payload `kind` byte values. Any byte other than `Add` or
/// `Subtract` is rejected as `InvalidModificationKind`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModificationKind {
    Add = 1,
    Subtract = 2,
}

impl ModificationKind {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Add),
            2 => Some(Self::Subtract),
            _ => None,
        }
    }
}

/// Zero-copy layout for a per-`(chain, emitter, sequence)` pending-quorum PDA.
/// On-disk size is **76 bytes** (4-byte alignment, tag at offset 0).
///
/// | offset | size | field              |
/// |--------|------|--------------------|
/// | 0      | 1    | tag ([`AccountTag::PendingObservations`]) |
/// | 1      | 1    | _pad0              |
/// | 2      | 2    | chain              |
/// | 4      | 4    | guardian_set_index |
/// | 8      | 4    | signatures (u32 bitmap; bit N == guardian-index N signed) |
/// | 12     | 32   | digest             |
/// | 44     | 32   | payer              |
///
/// The 32-bit bitmap covers 32 guardian indices. A protocol move to >32
/// guardians requires widening the field and bumping the layout version.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct PendingObservationsLayout {
    /// Account-type tag; always [`AccountTag::PendingObservations`]. See [`Self::TAG`].
    pub tag: u8,
    /// Alignment padding; crate-private so callers go through `Zeroable`.
    pub(crate) _pad0: u8,
    pub chain: u16,
    pub guardian_set_index: u32,
    pub signatures: u32,
    pub digest: [u8; 32],
    pub payer: Pubkey,
}

impl PendingObservationsLayout {
    /// Byte length of the layout (also the rent-paying allocation size).
    pub const LEN: usize = core::mem::size_of::<Self>();

    /// Account-type tag stamped at offset 0. See [`AccountTag`].
    pub const TAG: u8 = AccountTag::PendingObservations as u8;

    /// Quorum threshold: 13 of 19 guardians — the Core Bridge
    /// `(len * 2) / 3 + 1` for len 19. Pinned, not derived from the live set.
    pub const QUORUM_THRESHOLD: u32 = 13;
}

const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(PendingObservationsLayout, tag) == 0);
    assert!(offset_of!(PendingObservationsLayout, chain) == 2);
    assert!(offset_of!(PendingObservationsLayout, guardian_set_index) == 4);
    assert!(offset_of!(PendingObservationsLayout, signatures) == 8);
    assert!(offset_of!(PendingObservationsLayout, digest) == 12);
    assert!(offset_of!(PendingObservationsLayout, payer) == 44);
    assert!(PendingObservationsLayout::LEN == 76);
};

/// Zero-copy balance account for a `(chain, token_chain, token_address)`
/// triple. On-disk size is **70 bytes** (tag at offset 0).
///
/// | offset | size | field         |
/// |--------|------|---------------|
/// | 0      | 1    | tag ([`AccountTag::Balance`]) |
/// | 1      | 1    | _pad0         |
/// | 2      | 2    | chain         |
/// | 4      | 2    | token_chain   |
/// | 6      | 32   | token_address |
/// | 38     | 32   | balance       |
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct BalanceAccountLayout {
    /// Account-type tag; always [`AccountTag::Balance`]. See [`Self::TAG`].
    pub tag: u8,
    /// Alignment padding; crate-private so callers go through `Zeroable`.
    pub(crate) _pad0: u8,
    /// Chain on which this balance is held.
    pub chain: u16,
    /// Native chain of the token.
    pub token_chain: u16,
    /// Token address on its native chain.
    pub token_address: [u8; 32],
    /// Current balance, 32-byte big-endian (matches the VAA `amount` encoding).
    pub balance: Uint256,
}

impl BalanceAccountLayout {
    pub const LEN: usize = core::mem::size_of::<Self>();

    /// Account-type tag stamped at offset 0. See [`AccountTag`].
    pub const TAG: u8 = AccountTag::Balance as u8;

    /// Apply a `lock_or_burn`: credits when `chain == token_chain` (native
    /// lock), debits otherwise (wrapped burn). Overflow/underflow surface as
    /// `BalanceOverflow`/`BalanceUnderflow`.
    pub fn lock_or_burn(&mut self, amount: Uint256) -> Result<(), GlobalAccountantError> {
        if self.chain == self.token_chain {
            self.balance = self
                .balance
                .checked_add(amount)
                .ok_or(GlobalAccountantError::BalanceOverflow)?;
        } else {
            self.balance = self
                .balance
                .checked_sub(amount)
                .ok_or(GlobalAccountantError::BalanceUnderflow)?;
        }
        Ok(())
    }

    /// Apply an `unlock_or_mint`: debits when `chain == token_chain` (native
    /// unlock), credits otherwise (wrapped mint). Symmetric to
    /// [`Self::lock_or_burn`].
    pub fn unlock_or_mint(&mut self, amount: Uint256) -> Result<(), GlobalAccountantError> {
        if self.chain == self.token_chain {
            self.balance = self
                .balance
                .checked_sub(amount)
                .ok_or(GlobalAccountantError::BalanceUnderflow)?;
        } else {
            self.balance = self
                .balance
                .checked_add(amount)
                .ok_or(GlobalAccountantError::BalanceOverflow)?;
        }
        Ok(())
    }

    /// Raw `balance += amount` for the governance `modify_balance` path (no
    /// native/wrapped dispatch). Overflow surfaces as `ModifyBalanceOverflow`.
    pub fn raw_add(&mut self, amount: Uint256) -> Result<(), GlobalAccountantError> {
        self.balance = self
            .balance
            .checked_add(amount)
            .ok_or(GlobalAccountantError::ModifyBalanceOverflow)?;
        Ok(())
    }

    /// Raw `balance -= amount` for the governance `modify_balance` path.
    /// Underflow surfaces as `ModifyBalanceUnderflow`.
    pub fn raw_sub(&mut self, amount: Uint256) -> Result<(), GlobalAccountantError> {
        self.balance = self
            .balance
            .checked_sub(amount)
            .ok_or(GlobalAccountantError::ModifyBalanceUnderflow)?;
        Ok(())
    }
}

// Compile-time pins for the balance layout.
const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(BalanceAccountLayout, tag) == 0);
    assert!(offset_of!(BalanceAccountLayout, chain) == 2);
    assert!(offset_of!(BalanceAccountLayout, token_chain) == 4);
    assert!(offset_of!(BalanceAccountLayout, token_address) == 6);
    assert!(offset_of!(BalanceAccountLayout, balance) == 38);
    assert!(BalanceAccountLayout::LEN == 70);
};

/// Zero-copy per-chain Token Bridge emitter registration. One PDA per chain at
/// `(b"chain_registration", chain_be)`, holding the canonical emitter address.
/// Written only by `register_chain`; re-registration with a higher-sequence VAA
/// overwrites the emitter (supports emitter rotation).
///
/// | offset | size | field           |
/// |--------|------|-----------------|
/// | 0      | 1    | tag ([`AccountTag::ChainRegistration`]) |
/// | 1      | 1    | _pad0           |
/// | 2      | 2    | chain           |
/// | 4      | 28   | _padding        |
/// | 32     | 32   | emitter_address |
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct ChainRegistrationLayout {
    /// Account-type tag; always [`AccountTag::ChainRegistration`]. See [`Self::TAG`].
    pub tag: u8,
    /// Alignment padding; crate-private so callers go through `Zeroable`.
    pub(crate) _pad0: u8,
    /// Wormhole chain ID this PDA registers (mirrors the seed bytes).
    pub chain: u16,
    /// Reserved; crate-private so callers go through `Zeroable`.
    pub(crate) _padding: [u8; 28],
    /// Canonical Token Bridge emitter address on `chain`.
    pub emitter_address: [u8; 32],
}

impl ChainRegistrationLayout {
    pub const LEN: usize = core::mem::size_of::<Self>();

    /// Account-type tag stamped at offset 0. See [`AccountTag`].
    pub const TAG: u8 = AccountTag::ChainRegistration as u8;
}

const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(ChainRegistrationLayout, tag) == 0);
    assert!(offset_of!(ChainRegistrationLayout, chain) == 2);
    assert!(offset_of!(ChainRegistrationLayout, _padding) == 4);
    assert!(offset_of!(ChainRegistrationLayout, emitter_address) == 32);
    assert!(ChainRegistrationLayout::LEN == 64);
};

/// Zero-copy per-modification audit-log PDA. Each `modify_balance` lazy-inits
/// one PDA at `(b"modification", payload_sequence_be)`. A second VAA with the
/// same sequence collides on this address and is rejected with
/// `DuplicateModification` — this is the governance-path replay protection.
///
/// | offset | size | field         |
/// |--------|------|---------------|
/// | 0      | 1    | tag ([`AccountTag::Modification`]) |
/// | 1      | 1    | kind          |
/// | 2      | 2    | chain_id      |
/// | 4      | 2    | token_chain   |
/// | 6      | 2    | _pad0         |
/// | 8      | 8    | sequence      |
/// | 16     | 32   | token_address |
/// | 48     | 32   | amount        |
/// | 80     | 32   | reason        |
///
/// Total: 112 bytes (multiple of 8 for `Pod` alignment). Small fields are
/// clustered ahead of the 8-aligned `sequence` so the tag sits at offset 0.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct ModificationLayout {
    /// Account-type tag; always [`AccountTag::Modification`]. See [`Self::TAG`].
    pub tag: u8,
    /// `1` for `Add`, `2` for `Subtract` (see [`ModificationKind`]).
    pub kind: u8,
    /// Chain whose balance was modified.
    pub chain_id: u16,
    /// Native chain of the modified token.
    pub token_chain: u16,
    /// Alignment padding ahead of `sequence`; crate-private so callers go
    /// through `Zeroable`.
    pub(crate) _pad0: [u8; 2],
    /// Modification's own sequence (distinct from the VAA emitter sequence).
    pub sequence: u64,
    /// Token address on its native chain.
    pub token_address: [u8; 32],
    /// Modification amount, big-endian 256-bit unsigned integer.
    pub amount: Uint256,
    /// Free-form reason, 32-byte right-padded ASCII. Audit-trail only.
    pub reason: [u8; 32],
}

impl ModificationLayout {
    pub const LEN: usize = core::mem::size_of::<Self>();

    /// Account-type tag stamped at offset 0. See [`AccountTag`].
    pub const TAG: u8 = AccountTag::Modification as u8;
}

const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(ModificationLayout, tag) == 0);
    assert!(offset_of!(ModificationLayout, kind) == 1);
    assert!(offset_of!(ModificationLayout, chain_id) == 2);
    assert!(offset_of!(ModificationLayout, token_chain) == 4);
    assert!(offset_of!(ModificationLayout, sequence) == 8);
    assert!(offset_of!(ModificationLayout, token_address) == 16);
    assert!(offset_of!(ModificationLayout, amount) == 48);
    assert!(offset_of!(ModificationLayout, reason) == 80);
    assert!(ModificationLayout::LEN == 112);
};

#[cfg(test)]
mod tests {
    use super::*;

    // ---- AccountTag retrofit tests ----

    #[test]
    fn account_tag_values_pinned() {
        // Append-only: never renumber once shipped.
        assert_eq!(AccountTag::PendingObservations as u8, 1);
        assert_eq!(AccountTag::Balance as u8, 2);
        assert_eq!(AccountTag::ChainRegistration as u8, 3);
        assert_eq!(AccountTag::Modification as u8, 4);
        // Each layout's TAG const mirrors its AccountTag value.
        assert_eq!(PendingObservationsLayout::TAG, AccountTag::PendingObservations as u8);
        assert_eq!(BalanceAccountLayout::TAG, AccountTag::Balance as u8);
        assert_eq!(
            ChainRegistrationLayout::TAG,
            AccountTag::ChainRegistration as u8
        );
        assert_eq!(
            ModificationLayout::TAG,
            AccountTag::Modification as u8
        );
    }

    #[test]
    fn tag_at_offset_zero_for_all_layouts() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(PendingObservationsLayout, tag), 0);
        assert_eq!(offset_of!(BalanceAccountLayout, tag), 0);
        assert_eq!(offset_of!(ChainRegistrationLayout, tag), 0);
        assert_eq!(offset_of!(ModificationLayout, tag), 0);
    }

    #[test]
    fn zeroed_layout_is_not_a_valid_tag() {
        // A freshly allocated (all-zero) account must not parse as any type:
        // tag 0 is reserved, distinct from every AccountTag value.
        assert_eq!(<PendingObservationsLayout as Zeroable>::zeroed().tag, 0);
        assert_eq!(<BalanceAccountLayout as Zeroable>::zeroed().tag, 0);
        assert_eq!(<ChainRegistrationLayout as Zeroable>::zeroed().tag, 0);
        assert_eq!(<ModificationLayout as Zeroable>::zeroed().tag, 0);
        for tag in [
            AccountTag::PendingObservations,
            AccountTag::Balance,
            AccountTag::ChainRegistration,
            AccountTag::Modification,
        ] {
            assert_ne!(tag as u8, 0);
        }
    }

    // ---- BalanceAccountLayout tests ----

    #[test]
    fn balance_layout_size_pinned() {
        // 70 bytes — tag (1) + _pad0 (1) + chain (2) + token_chain (2) +
        // token_address (32) + balance (32). Grew from 68 to 70 when the
        // offset-0 account tag was retrofitted.
        assert_eq!(BalanceAccountLayout::LEN, 70);
    }

    #[test]
    fn balance_layout_uint256_offsets_pinned() {
        // Runtime mirror of the const-assert block above.
        use core::mem::offset_of;
        assert_eq!(offset_of!(BalanceAccountLayout, tag), 0);
        assert_eq!(offset_of!(BalanceAccountLayout, chain), 2);
        assert_eq!(offset_of!(BalanceAccountLayout, token_chain), 4);
        assert_eq!(offset_of!(BalanceAccountLayout, token_address), 6);
        assert_eq!(offset_of!(BalanceAccountLayout, balance), 38);
    }

    #[test]
    fn balance_layout_is_pod_friendly() {
        let mut token_address = [0u8; 32];
        for (i, b) in token_address.iter_mut().enumerate() {
            *b = i as u8;
        }
        let original = BalanceAccountLayout {
            tag: BalanceAccountLayout::TAG,
            _pad0: 0,
            chain: 1,
            token_chain: 2,
            token_address,
            balance: Uint256::from_u128(0xcafe_babe),
        };
        let bytes = bytemuck::bytes_of(&original);
        assert_eq!(bytes.len(), BalanceAccountLayout::LEN);
        let copy: &BalanceAccountLayout = bytemuck::from_bytes(bytes);
        assert_eq!(&original, copy);
    }

    // ---- PendingObservationsLayout tests ----

    #[test]
    fn pending_layout_size_pinned() {
        // Runtime mirror of the const-assert above (incl. 2 bytes tail padding).
        assert_eq!(PendingObservationsLayout::LEN, 76);
    }

    #[test]
    fn pending_layout_offsets_pinned() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(PendingObservationsLayout, tag), 0);
        assert_eq!(offset_of!(PendingObservationsLayout, chain), 2);
        assert_eq!(offset_of!(PendingObservationsLayout, guardian_set_index), 4);
        assert_eq!(offset_of!(PendingObservationsLayout, signatures), 8);
        assert_eq!(offset_of!(PendingObservationsLayout, digest), 12);
        assert_eq!(offset_of!(PendingObservationsLayout, payer), 44);
    }

    #[test]
    fn pending_layout_is_pod_friendly() {
        let mut digest = [0u8; 32];
        for (i, b) in digest.iter_mut().enumerate() {
            *b = i as u8;
        }
        let original = PendingObservationsLayout {
            tag: PendingObservationsLayout::TAG,
            _pad0: 0,
            chain: 1,
            guardian_set_index: 0x0BAD_CAFE,
            signatures: 0x0000_1FFFu32, // 13 low bits set
            digest,
            payer: [0xAA; 32],
        };
        let bytes = bytemuck::bytes_of(&original);
        let copy: &PendingObservationsLayout = bytemuck::from_bytes(bytes);
        assert_eq!(&original, copy);
        assert_eq!(PendingObservationsLayout::LEN, bytes.len());
    }

    #[test]
    fn balance_layout_balance_encodes_big_endian_on_disk() {
        // The on-disk balance bytes must be the big-endian encoding, so a VAA
        // `amount` slice copies in without byte-order conversion.
        let original = BalanceAccountLayout {
            tag: BalanceAccountLayout::TAG,
            _pad0: 0,
            chain: 0,
            token_chain: 0,
            token_address: [0u8; 32],
            balance: Uint256::from_u128(0x1234_5678),
        };
        let bytes = bytemuck::bytes_of(&original);
        let balance_slice = &bytes[38..70];
        let mut expected = [0u8; 32];
        expected[28] = 0x12;
        expected[29] = 0x34;
        expected[30] = 0x56;
        expected[31] = 0x78;
        assert_eq!(balance_slice, &expected);
    }

    // ---- lock_or_burn / unlock_or_mint tests ----

    fn balance_with(chain: u16, token_chain: u16, balance: Uint256) -> BalanceAccountLayout {
        // Recognisable token-address pattern; irrelevant to the arithmetic.
        let mut token_address = [0u8; 32];
        token_address[0] = 0x62;
        token_address[31] = 0x61;
        BalanceAccountLayout {
            tag: BalanceAccountLayout::TAG,
            _pad0: 0,
            chain,
            token_chain,
            token_address,
            balance,
        }
    }

    #[test]
    fn lock_or_burn_native_chain_credits() {
        // chain == token_chain ⇒ native-side credit (500 + 200 = 700).
        let mut acc = balance_with(0xbae2, 0xbae2, Uint256::from_u128(500));
        acc.lock_or_burn(Uint256::from_u128(200)).unwrap();
        assert_eq!(acc.balance, Uint256::from_u128(700));
    }

    #[test]
    fn lock_or_burn_wrapped_chain_debits() {
        // chain != token_chain ⇒ wrapped-side debit (500 - 200 = 300).
        let mut acc = balance_with(0xcae8, 0xbae2, Uint256::from_u128(500));
        acc.lock_or_burn(Uint256::from_u128(200)).unwrap();
        assert_eq!(acc.balance, Uint256::from_u128(300));
    }

    #[test]
    fn lock_or_burn_wrapped_chain_underflow_rejects() {
        // Underflow ⇒ BalanceUnderflow.
        let mut acc = balance_with(0xcae8, 0xbae2, Uint256::ZERO);
        let err = acc.lock_or_burn(Uint256::from_u128(200)).unwrap_err();
        assert_eq!(err, GlobalAccountantError::BalanceUnderflow);
        assert_eq!(acc.balance, Uint256::ZERO, "balance unchanged on error");
    }

    #[test]
    fn lock_or_burn_native_chain_overflow_rejects() {
        // Overflow ⇒ BalanceOverflow.
        let mut acc = balance_with(0xbae2, 0xbae2, Uint256::MAX);
        let err = acc.lock_or_burn(Uint256::from_u128(200)).unwrap_err();
        assert_eq!(err, GlobalAccountantError::BalanceOverflow);
        assert_eq!(acc.balance, Uint256::MAX, "balance unchanged on error");
    }

    #[test]
    fn unlock_or_mint_native_chain_debits() {
        // chain == token_chain ⇒ native-side debit (500 - 200 = 300).
        let mut acc = balance_with(0xbae2, 0xbae2, Uint256::from_u128(500));
        acc.unlock_or_mint(Uint256::from_u128(200)).unwrap();
        assert_eq!(acc.balance, Uint256::from_u128(300));
    }

    #[test]
    fn unlock_or_mint_native_chain_underflow_rejects() {
        // Underflow ⇒ BalanceUnderflow.
        let mut acc = balance_with(0xbae2, 0xbae2, Uint256::ZERO);
        let err = acc.unlock_or_mint(Uint256::from_u128(200)).unwrap_err();
        assert_eq!(err, GlobalAccountantError::BalanceUnderflow);
        assert_eq!(acc.balance, Uint256::ZERO);
    }

    #[test]
    fn unlock_or_mint_wrapped_chain_credits() {
        // chain != token_chain ⇒ wrapped-side credit (500 + 200 = 700).
        let mut acc = balance_with(0xcae8, 0xbae2, Uint256::from_u128(500));
        acc.unlock_or_mint(Uint256::from_u128(200)).unwrap();
        assert_eq!(acc.balance, Uint256::from_u128(700));
    }

    #[test]
    fn unlock_or_mint_wrapped_chain_overflow_rejects() {
        // Overflow ⇒ BalanceOverflow.
        let mut acc = balance_with(0xcae8, 0xbae2, Uint256::MAX);
        let err = acc.unlock_or_mint(Uint256::from_u128(200)).unwrap_err();
        assert_eq!(err, GlobalAccountantError::BalanceOverflow);
        assert_eq!(acc.balance, Uint256::MAX);
    }
}
