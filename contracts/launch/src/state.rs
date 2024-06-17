use cosmwasm_std::Addr;
use cw_storage_plus::{Item, Map};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use membrane::{launch::Config, types::{UserRatio, Lockdrop, LockedUser}};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct LaunchAddrs {
    pub osmosis_proxy: Addr,
    pub oracle: Addr,
    pub staking: Addr,
    pub vesting: Addr,
    pub governance: Addr,
    pub positions: Addr,
    pub stability_pool: Addr,
    pub liq_queue: Addr,
    pub liquidity_check: Addr,
    pub tema_auction: Addr,    
    pub discount_vault: Addr,
    pub system_discounts: Addr,
}

pub const CONFIG: Item<Config> = Item::new("config");

//Lockdrop
pub const LOCKDROP: Item<Lockdrop> = Item::new("lockdrop");
pub const LOCKED_USERS: Map<Addr, LockedUser> = Map::new("locked_users");
pub const INCENTIVE_RATIOS: Item<Vec<UserRatio>> = Item::new("incentive_ratios");

//Launch
pub const ADDRESSES: Item<LaunchAddrs> = Item::new("addresses");
pub const OSMO_POOL_ID: Item<u64> = Item::new("osmo_pool");
pub const TEMA_POOL: Item<u64> = Item::new("tema_pool");