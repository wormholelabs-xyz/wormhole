//! Zero-copy account layouts and their type tags.

use bytemuck::{Pod, Zeroable};

use crate::error::GlobalAccountantError;
use crate::primitives::{Pubkey, Uint256};

/// Account-type tag at offset 0 of every program-owned PDA. Off-chain readers
/// filter with `memcmp(offset 0, [tag])`; on-chain loaders check it after PDA derivation.
///
/// Append-only; do not renumber. `0` is reserved because a fresh account is all zeroes.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountTag {
    PendingObservations = 1,
    Balance = 2,
    ChainRegistration = 3,
    ModifyBalance = 4,
}

/// `ModifyBalance` payload `kind` byte. Other values raise `InvalidModificationKind`.
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

/// Pending-quorum PDA for one `(chain, emitter, sequence)`. 76 bytes, 4-byte aligned.
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
/// The bitmap caps the guardian set at 32; a larger set needs a new layout version.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct PendingObservationsLayout {
    /// Always [`AccountTag::PendingObservations`].
    pub tag: u8,
    pub(crate) _pad0: u8,
    pub chain: u16,
    pub guardian_set_index: u32,
    pub signatures: u32,
    pub digest: [u8; 32],
    pub payer: Pubkey,
}

pub trait AccountLayout: Pod {
    const TAG: u8;
    const LEN: usize = core::mem::size_of::<Self>();

    fn tag(&self) -> u8;
}

impl AccountLayout for PendingObservationsLayout {
    const TAG: u8 = Self::TAG;

    fn tag(&self) -> u8 {
        self.tag
    }
}

impl AccountLayout for BalanceAccountLayout {
    const TAG: u8 = Self::TAG;

    fn tag(&self) -> u8 {
        self.tag
    }
}

impl AccountLayout for ChainRegistrationLayout {
    const TAG: u8 = Self::TAG;

    fn tag(&self) -> u8 {
        self.tag
    }
}

impl AccountLayout for ModifyBalanceLayout {
    const TAG: u8 = Self::TAG;

    fn tag(&self) -> u8 {
        self.tag
    }
}

impl PendingObservationsLayout {
    /// Fresh record with no signatures.
    pub fn new(chain: u16, guardian_set_index: u32, digest: [u8; 32], payer: Pubkey) -> Self {
        Self {
            tag: Self::TAG,
            _pad0: 0,
            chain,
            guardian_set_index,
            signatures: 0,
            digest,
            payer,
        }
    }

    /// Layout length; also the allocation size.
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub const TAG: u8 = AccountTag::PendingObservations as u8;

    /// Quorum `(2N / 3) + 1`, as in the Core Bridge and the Verify VAA Shim.
    /// Callers read `N` from the `GuardianSet` account on every observation.
    pub const fn quorum_for(num_guardians: u32) -> u32 {
        (num_guardians * 2) / 3 + 1
    }
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

/// Balance account for one `(chain, token_chain, token_address)`. 70 bytes.
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
    /// Always [`AccountTag::Balance`].
    pub tag: u8,
    pub(crate) _pad0: u8,
    /// Chain that holds this balance.
    pub chain: u16,
    /// Native chain of the token.
    pub token_chain: u16,
    /// Token address on its native chain.
    pub token_address: [u8; 32],
    pub balance: Uint256,
}

impl BalanceAccountLayout {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn new(chain: u16, token_chain: u16, token_address: [u8; 32], balance: Uint256) -> Self {
        Self {
            tag: Self::TAG,
            _pad0: 0,
            chain,
            token_chain,
            token_address,
            balance,
        }
    }

    pub const TAG: u8 = AccountTag::Balance as u8;

    /// Native (`chain == token_chain`): credit. Wrapped: debit.
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

    /// Native (`chain == token_chain`): debit. Wrapped: credit.
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

    /// `balance += amount` for `modify_balance`; overflow is `ModifyBalanceOverflow`.
    pub fn raw_add(&mut self, amount: Uint256) -> Result<(), GlobalAccountantError> {
        self.balance = self
            .balance
            .checked_add(amount)
            .ok_or(GlobalAccountantError::ModifyBalanceOverflow)?;
        Ok(())
    }

    /// `balance -= amount` for `modify_balance`; underflow is `ModifyBalanceUnderflow`.
    pub fn raw_sub(&mut self, amount: Uint256) -> Result<(), GlobalAccountantError> {
        self.balance = self
            .balance
            .checked_sub(amount)
            .ok_or(GlobalAccountantError::ModifyBalanceUnderflow)?;
        Ok(())
    }
}

const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(BalanceAccountLayout, tag) == 0);
    assert!(offset_of!(BalanceAccountLayout, chain) == 2);
    assert!(offset_of!(BalanceAccountLayout, token_chain) == 4);
    assert!(offset_of!(BalanceAccountLayout, token_address) == 6);
    assert!(offset_of!(BalanceAccountLayout, balance) == 38);
    assert!(BalanceAccountLayout::LEN == 70);
};

/// Token Bridge emitter registration, one PDA per chain at `(b"chain_registration", chain_be)`.
/// `register_chain` writes it; a later VAA overwrites the emitter.
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
    /// Always [`AccountTag::ChainRegistration`].
    pub tag: u8,
    pub(crate) _pad0: u8,
    /// Registered Wormhole chain ID; equals the seed value.
    pub chain: u16,
    pub(crate) _padding: [u8; 28],
    /// Token Bridge emitter address on `chain`.
    pub emitter_address: [u8; 32],
}

impl ChainRegistrationLayout {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub const TAG: u8 = AccountTag::ChainRegistration as u8;

    pub fn new(chain: u16, emitter_address: [u8; 32]) -> Self {
        Self {
            tag: Self::TAG,
            _pad0: 0,
            chain,
            _padding: [0; 28],
            emitter_address,
        }
    }
}

const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(ChainRegistrationLayout, tag) == 0);
    assert!(offset_of!(ChainRegistrationLayout, chain) == 2);
    assert!(offset_of!(ChainRegistrationLayout, _padding) == 4);
    assert!(offset_of!(ChainRegistrationLayout, emitter_address) == 32);
    assert!(ChainRegistrationLayout::LEN == 64);
};

/// Audit-log PDA at `(b"modify_balance", payload_sequence_be)`, created by `modify_balance`.
/// A second VAA with the same sequence fails with `DuplicateModifyBalance`.
///
/// | offset | size | field         |
/// |--------|------|---------------|
/// | 0      | 1    | tag ([`AccountTag::ModifyBalance`]) |
/// | 1      | 1    | kind          |
/// | 2      | 2    | chain_id      |
/// | 4      | 2    | token_chain   |
/// | 6      | 2    | _pad0         |
/// | 8      | 8    | sequence      |
/// | 16     | 32   | token_address |
/// | 48     | 32   | amount        |
/// | 80     | 32   | reason        |
///
/// Total: 112 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct ModifyBalanceLayout {
    /// Always [`AccountTag::ModifyBalance`].
    pub tag: u8,
    /// See [`ModificationKind`].
    pub kind: u8,
    /// Chain of the modified balance.
    pub chain_id: u16,
    /// Native chain of the token.
    pub token_chain: u16,
    pub(crate) _pad0: [u8; 2],
    /// Modification sequence from the payload; distinct from the VAA sequence.
    pub sequence: u64,
    /// Token address on its native chain.
    pub token_address: [u8; 32],
    pub amount: Uint256,
    /// Right-padded ASCII reason; audit only.
    pub reason: [u8; 32],
}

impl ModifyBalanceLayout {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub const TAG: u8 = AccountTag::ModifyBalance as u8;

    pub fn new(
        kind: ModificationKind,
        chain_id: u16,
        token_chain: u16,
        sequence: u64,
        token_address: [u8; 32],
        amount: Uint256,
        reason: [u8; 32],
    ) -> Self {
        Self {
            tag: Self::TAG,
            kind: kind as u8,
            chain_id,
            token_chain,
            _pad0: [0; 2],
            sequence,
            token_address,
            amount,
            reason,
        }
    }
}

const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(ModifyBalanceLayout, tag) == 0);
    assert!(offset_of!(ModifyBalanceLayout, kind) == 1);
    assert!(offset_of!(ModifyBalanceLayout, chain_id) == 2);
    assert!(offset_of!(ModifyBalanceLayout, token_chain) == 4);
    assert!(offset_of!(ModifyBalanceLayout, sequence) == 8);
    assert!(offset_of!(ModifyBalanceLayout, token_address) == 16);
    assert!(offset_of!(ModifyBalanceLayout, amount) == 48);
    assert!(offset_of!(ModifyBalanceLayout, reason) == 80);
    assert!(ModifyBalanceLayout::LEN == 112);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_tag_values_pinned() {
        assert_eq!(AccountTag::PendingObservations as u8, 1);
        assert_eq!(AccountTag::Balance as u8, 2);
        assert_eq!(AccountTag::ChainRegistration as u8, 3);
        assert_eq!(AccountTag::ModifyBalance as u8, 4);
        assert_eq!(
            PendingObservationsLayout::TAG,
            AccountTag::PendingObservations as u8
        );
        assert_eq!(BalanceAccountLayout::TAG, AccountTag::Balance as u8);
        assert_eq!(
            ChainRegistrationLayout::TAG,
            AccountTag::ChainRegistration as u8
        );
        assert_eq!(ModifyBalanceLayout::TAG, AccountTag::ModifyBalance as u8);
    }

    #[test]
    fn tag_at_offset_zero_for_all_layouts() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(PendingObservationsLayout, tag), 0);
        assert_eq!(offset_of!(BalanceAccountLayout, tag), 0);
        assert_eq!(offset_of!(ChainRegistrationLayout, tag), 0);
        assert_eq!(offset_of!(ModifyBalanceLayout, tag), 0);
    }

    #[test]
    fn zeroed_layout_is_not_a_valid_tag() {
        assert_eq!(<PendingObservationsLayout as Zeroable>::zeroed().tag, 0);
        assert_eq!(<BalanceAccountLayout as Zeroable>::zeroed().tag, 0);
        assert_eq!(<ChainRegistrationLayout as Zeroable>::zeroed().tag, 0);
        assert_eq!(<ModifyBalanceLayout as Zeroable>::zeroed().tag, 0);
        for tag in [
            AccountTag::PendingObservations,
            AccountTag::Balance,
            AccountTag::ChainRegistration,
            AccountTag::ModifyBalance,
        ] {
            assert_ne!(tag as u8, 0);
        }
    }

    #[test]
    fn balance_layout_size_pinned() {
        assert_eq!(BalanceAccountLayout::LEN, 70);
    }

    #[test]
    fn balance_layout_uint256_offsets_pinned() {
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

    #[test]
    fn pending_layout_size_pinned() {
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

    fn balance_with(chain: u16, token_chain: u16, balance: Uint256) -> BalanceAccountLayout {
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
        let mut acc = balance_with(0xbae2, 0xbae2, Uint256::from_u128(500));
        acc.lock_or_burn(Uint256::from_u128(200)).unwrap();
        assert_eq!(acc.balance, Uint256::from_u128(700));
    }

    #[test]
    fn lock_or_burn_wrapped_chain_debits() {
        let mut acc = balance_with(0xcae8, 0xbae2, Uint256::from_u128(500));
        acc.lock_or_burn(Uint256::from_u128(200)).unwrap();
        assert_eq!(acc.balance, Uint256::from_u128(300));
    }

    #[test]
    fn lock_or_burn_wrapped_chain_underflow_rejects() {
        let mut acc = balance_with(0xcae8, 0xbae2, Uint256::ZERO);
        let err = acc.lock_or_burn(Uint256::from_u128(200)).unwrap_err();
        assert_eq!(err, GlobalAccountantError::BalanceUnderflow);
        assert_eq!(acc.balance, Uint256::ZERO, "balance unchanged on error");
    }

    #[test]
    fn lock_or_burn_native_chain_overflow_rejects() {
        let mut acc = balance_with(0xbae2, 0xbae2, Uint256::MAX);
        let err = acc.lock_or_burn(Uint256::from_u128(200)).unwrap_err();
        assert_eq!(err, GlobalAccountantError::BalanceOverflow);
        assert_eq!(acc.balance, Uint256::MAX, "balance unchanged on error");
    }

    #[test]
    fn unlock_or_mint_native_chain_debits() {
        let mut acc = balance_with(0xbae2, 0xbae2, Uint256::from_u128(500));
        acc.unlock_or_mint(Uint256::from_u128(200)).unwrap();
        assert_eq!(acc.balance, Uint256::from_u128(300));
    }

    #[test]
    fn unlock_or_mint_native_chain_underflow_rejects() {
        let mut acc = balance_with(0xbae2, 0xbae2, Uint256::ZERO);
        let err = acc.unlock_or_mint(Uint256::from_u128(200)).unwrap_err();
        assert_eq!(err, GlobalAccountantError::BalanceUnderflow);
        assert_eq!(acc.balance, Uint256::ZERO);
    }

    #[test]
    fn unlock_or_mint_wrapped_chain_credits() {
        let mut acc = balance_with(0xcae8, 0xbae2, Uint256::from_u128(500));
        acc.unlock_or_mint(Uint256::from_u128(200)).unwrap();
        assert_eq!(acc.balance, Uint256::from_u128(700));
    }

    #[test]
    fn unlock_or_mint_wrapped_chain_overflow_rejects() {
        let mut acc = balance_with(0xcae8, 0xbae2, Uint256::MAX);
        let err = acc.unlock_or_mint(Uint256::from_u128(200)).unwrap_err();
        assert_eq!(err, GlobalAccountantError::BalanceOverflow);
        assert_eq!(acc.balance, Uint256::MAX);
    }
}
