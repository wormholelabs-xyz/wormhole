use global_accountant_definitions::{TransceiverHubLayout, TransceiverPeerLayout};
use solana_account::Account;

use super::ids::program_id;

pub fn hub_account(layout: &TransceiverHubLayout) -> Account {
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

pub fn peer_account(layout: &TransceiverPeerLayout) -> Account {
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}
