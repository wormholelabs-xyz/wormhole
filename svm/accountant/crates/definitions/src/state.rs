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

/// Pending-quorum PDA for one `(chain, emitter, sequence)`. 88 bytes, 4-byte aligned.
///
/// | offset | size | field              |
/// |--------|------|--------------------|
/// | 0      | 1    | tag ([`AccountTag::PendingObservations`]) |
/// | 1      | 1    | _pad0              |
/// | 2      | 2    | chain              |
/// | 4      | 4    | guardian_set_index |
/// | 8      | 16   | signatures (128-bit bitmap as 4 LE `u32` words; bit N == guardian-index N signed) |
/// | 24     | 32   | digest             |
/// | 56     | 32   | payer              |
///
/// The bitmap caps the guardian set at [`Self::MAX_GUARDIANS`], as wormchain's `u128`
/// `Data.signatures` does. `[u32; 4]` keeps 4-byte alignment; `u128` would need 16.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct PendingObservationsLayout {
    /// Always [`AccountTag::PendingObservations`].
    pub tag: u8,
    pub(crate) _pad0: u8,
    pub chain: u16,
    pub guardian_set_index: u32,
    pub signatures: [u32; 4],
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
            signatures: [0; 4],
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

    /// Bitmap capacity; equals wormchain's `u128`.
    pub const MAX_GUARDIANS: u32 = 128;

    /// `None` when `index >= MAX_GUARDIANS`.
    pub fn has_signature(&self, index: u8) -> Option<bool> {
        let (word, bit) = Self::bit_position(index)?;
        Some(self.signatures[word] & bit != 0)
    }

    /// Set the bit for `index`. `None` when `index >= MAX_GUARDIANS`.
    pub fn set_signature(&mut self, index: u8) -> Option<()> {
        let (word, bit) = Self::bit_position(index)?;
        self.signatures[word] |= bit;
        Some(())
    }

    pub fn num_signatures(&self) -> u32 {
        self.signatures.iter().map(|w| w.count_ones()).sum()
    }

    fn bit_position(index: u8) -> Option<(usize, u32)> {
        if (index as u32) >= Self::MAX_GUARDIANS {
            return None;
        }
        Some(((index / 32) as usize, 1u32 << (index % 32)))
    }
}

const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(PendingObservationsLayout, tag) == 0);
    assert!(offset_of!(PendingObservationsLayout, chain) == 2);
    assert!(offset_of!(PendingObservationsLayout, guardian_set_index) == 4);
    assert!(offset_of!(PendingObservationsLayout, signatures) == 8);
    assert!(offset_of!(PendingObservationsLayout, digest) == 24);
    assert!(offset_of!(PendingObservationsLayout, payer) == 56);
    assert!(PendingObservationsLayout::LEN == 88);
    assert!(PendingObservationsLayout::MAX_GUARDIANS == 32 * 4);
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
/// `register_chain` writes it. A later VAA overwrites the emitter only when its governance
/// sequence is above `governance_sequence`; all `RegisterChain` VAAs share the governance
/// emitter's sequence space, so a higher sequence is the newer registration.
///
/// | offset | size | field               |
/// |--------|------|---------------------|
/// | 0      | 1    | tag ([`AccountTag::ChainRegistration`]) |
/// | 1      | 1    | _pad0               |
/// | 2      | 2    | chain               |
/// | 4      | 8    | governance_sequence |
/// | 12     | 20   | _padding            |
/// | 32     | 32   | emitter_address     |
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct ChainRegistrationLayout {
    /// Always [`AccountTag::ChainRegistration`].
    pub tag: u8,
    pub(crate) _pad0: u8,
    /// Registered Wormhole chain ID; equals the seed value.
    pub chain: u16,
    /// Little-endian sequence of the governance VAA that wrote this record.
    pub governance_sequence: [u8; 8],
    pub(crate) _padding: [u8; 20],
    /// Token Bridge emitter address on `chain`.
    pub emitter_address: [u8; 32],
}

impl ChainRegistrationLayout {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub const TAG: u8 = AccountTag::ChainRegistration as u8;

    pub fn new(chain: u16, emitter_address: [u8; 32], governance_sequence: u64) -> Self {
        Self {
            tag: Self::TAG,
            _pad0: 0,
            chain,
            governance_sequence: governance_sequence.to_le_bytes(),
            _padding: [0; 20],
            emitter_address,
        }
    }

    pub fn governance_sequence(&self) -> u64 {
        u64::from_le_bytes(self.governance_sequence)
    }
}

const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(ChainRegistrationLayout, tag) == 0);
    assert!(offset_of!(ChainRegistrationLayout, chain) == 2);
    assert!(offset_of!(ChainRegistrationLayout, governance_sequence) == 4);
    assert!(offset_of!(ChainRegistrationLayout, _padding) == 12);
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
    fn account_tags_are_pinned_and_zeroed_layout_is_invalid() {
        let cases: [(&str, u8, u8, u8); 4] = [
            (
                "pending",
                AccountTag::PendingObservations as u8,
                PendingObservationsLayout::TAG,
                <PendingObservationsLayout as Zeroable>::zeroed().tag,
            ),
            (
                "balance",
                AccountTag::Balance as u8,
                BalanceAccountLayout::TAG,
                <BalanceAccountLayout as Zeroable>::zeroed().tag,
            ),
            (
                "chain_registration",
                AccountTag::ChainRegistration as u8,
                ChainRegistrationLayout::TAG,
                <ChainRegistrationLayout as Zeroable>::zeroed().tag,
            ),
            (
                "modify_balance",
                AccountTag::ModifyBalance as u8,
                ModifyBalanceLayout::TAG,
                <ModifyBalanceLayout as Zeroable>::zeroed().tag,
            ),
        ];
        for (i, (name, tag, layout_tag, zeroed_tag)) in cases.iter().enumerate() {
            assert_eq!(*tag, i as u8 + 1, "{name} tag value");
            assert_eq!(*layout_tag, *tag, "{name} layout tag");
            assert_eq!(*zeroed_tag, 0, "{name} zeroed tag");
        }
    }

    #[test]
    fn balance_encodes_big_endian_on_disk() {
        let layout = BalanceAccountLayout::new(1, 2, [0x62; 32], Uint256::from_u128(0x1234_5678));
        let bytes = bytemuck::bytes_of(&layout);
        let copy: &BalanceAccountLayout = bytemuck::from_bytes(bytes);
        assert_eq!(copy, &layout);
        let mut expected = [0u8; 32];
        expected[28..].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(&bytes[38..70], &expected);
    }

    #[test]
    fn lock_and_unlock_table() {
        use GlobalAccountantError as E;
        const NATIVE: u16 = 0xbae2;
        const WRAPPED: u16 = 0xcae8;
        type Case = (&'static str, u16, bool, Uint256, Result<Uint256, E>);
        let amount = Uint256::from_u128(200);
        let cases: [Case; 8] = [
            (
                "lock native credits",
                NATIVE,
                true,
                Uint256::from_u128(500),
                Ok(Uint256::from_u128(700)),
            ),
            (
                "lock wrapped debits",
                WRAPPED,
                true,
                Uint256::from_u128(500),
                Ok(Uint256::from_u128(300)),
            ),
            (
                "lock wrapped underflows",
                WRAPPED,
                true,
                Uint256::ZERO,
                Err(E::BalanceUnderflow),
            ),
            (
                "lock native overflows",
                NATIVE,
                true,
                Uint256::MAX,
                Err(E::BalanceOverflow),
            ),
            (
                "unlock native debits",
                NATIVE,
                false,
                Uint256::from_u128(500),
                Ok(Uint256::from_u128(300)),
            ),
            (
                "unlock native underflows",
                NATIVE,
                false,
                Uint256::ZERO,
                Err(E::BalanceUnderflow),
            ),
            (
                "unlock wrapped credits",
                WRAPPED,
                false,
                Uint256::from_u128(500),
                Ok(Uint256::from_u128(700)),
            ),
            (
                "unlock wrapped overflows",
                WRAPPED,
                false,
                Uint256::MAX,
                Err(E::BalanceOverflow),
            ),
        ];
        for (name, chain, lock, start, expected) in cases {
            let mut acc = BalanceAccountLayout::new(chain, NATIVE, [0x62; 32], start);
            let result = if lock {
                acc.lock_or_burn(amount)
            } else {
                acc.unlock_or_mint(amount)
            };
            match expected {
                Ok(balance) => {
                    assert_eq!(result, Ok(()), "{name}");
                    assert_eq!(acc.balance, balance, "{name}");
                }
                Err(err) => {
                    assert_eq!(result, Err(err), "{name}");
                    assert_eq!(acc.balance, start, "{name} balance unchanged");
                }
            }
        }
    }
}
