use cosmwasm_std::{
    entry_point, to_binary, Binary, Decimal, Deps, DepsMut, Env, MessageInfo, WasmMsg,
    Response, StdResult, Uint128, Reply, StdError, CosmosMsg, SubMsg, Addr, coin, attr, Storage,
};
use cw2::set_contract_version;

use membrane::helpers::{withdrawal_msg, get_contract_balances};
use membrane::launch::{Config, ExecuteMsg, InstantiateMsg, QueryMsg, UpdateConfig};
use membrane::math::{decimal_division, decimal_multiplication};
use membrane::staking::ExecuteMsg as StakingExecuteMsg;
use membrane::osmosis_proxy::ExecuteMsg as OPExecuteMsg;
use membrane::types::{AssetInfo, Asset, UserRatio, Lockdrop, LockedUser, Lock};

use osmosis_std::types::cosmos::base::v1beta1::Coin;
use osmosis_std::types::osmosis::gamm::poolmodels::balancer::v1beta1::MsgCreateBalancerPool;
use osmosis_std::types::osmosis::gamm::poolmodels::stableswap::v1beta1::{MsgCreateStableswapPool, PoolParams as SSPoolParams};
use osmosis_std::types::osmosis::gamm::v1beta1::PoolParams;
use osmosis_std::types::osmosis::gamm::v1beta1::PoolAsset;


use crate::error::ContractError;
use crate::state::{CONFIG, ADDRESSES, LaunchAddrs, CREDIT_POOL_IDS, LOCKDROP, INCENTIVE_RATIOS, CreditPools};
use crate::reply::{handle_auction_reply, handle_cdp_reply, handle_create_denom_reply, handle_gov_reply, handle_lc_reply, handle_lq_reply, handle_op_reply, handle_oracle_reply, handle_sp_reply, handle_stableswap_reply, handle_staking_reply, handle_vesting_reply};

// Contract name and version used for migration.
const CONTRACT_NAME: &str = "launch";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

//Reply ID
pub const OSMOSIS_PROXY_REPLY_ID: u64 = 1;
pub const ORACLE_REPLY_ID: u64 = 2;
pub const STAKING_REPLY_ID: u64 = 3;
pub const VESTING_REPLY_ID: u64 = 4;
pub const GOVERNANCE_REPLY_ID: u64 = 5;
pub const POSITIONS_REPLY_ID: u64 = 6;
pub const STABILITY_POOL_REPLY_ID: u64 = 7;
pub const LIQ_QUEUE_REPLY_ID: u64 = 8;
pub const LIQUIDITY_CHECK_REPLY_ID: u64 = 9;
pub const DEBT_AUCTION_REPLY_ID: u64 = 10;
pub const STABLESWAP_REPLY_ID: u64 = 11;
pub const CREATE_DENOM_REPLY_ID: u64 = 12;
pub const SYSTEM_DISCOUNTS_REPLY_ID: u64 = 13;
pub const DISCOUNT_VAULT_REPLY_ID: u64 = 14;
pub const BALANCER_POOL_REPLY_ID: u64 = 15;

//Constants
pub const SECONDS_PER_DAY: u64 = 86_400u64;

pub fn instantiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {

    //Need to send 20 OSMO for CreateDenom Msgs
    if info.funds[0].amount != Uint128::new(20_000_00) && info.funds[0].denom != String::from("uosmo"){ return Err(ContractError::NeedOsmo {}) }

    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    let config = Config {
        mbrn_denom: String::from(""),
        credit_denom: String::from(""),
        labs_addr: deps.api.addr_validate(&msg.labs_addr)?,
        apollo_router: deps.api.addr_validate(&msg.apollo_router)?,
        mbrn_launch_amount: Uint128::new(25_000_000_000_000),
        osmosis_proxy_id: msg.osmosis_proxy_id,
        oracle_id: msg.oracle_id,
        staking_id: msg.staking_id,
        vesting_id: msg.vesting_id,
        governance_id: msg.governance_id,
        positions_id: msg.positions_id,
        stability_pool_id: msg.stability_pool_id,
        liq_queue_id: msg.liq_queue_id,
        liquidity_check_id: msg.liquidity_check_id,
        mbrn_auction_id: msg.mbrn_auction_id,
        margin_proxy_id: msg.margin_proxy_id,
        system_discounts_id: msg.system_discounts_id,
        discount_vault_id: msg.discount_vault_id,
        atom_denom: String::from("ibc/27394FB092D2ECCD56123C74F36E4C1F926001CEADA9CA97EA622B25F41E5EB2"),
        osmo_denom: String::from("uosmo"),
        usdc_denom: String::from(""),  //axl wrapped usdc
        atomosmo_pool_id: 1,
        osmousdc_pool_id: 678, //axl wrapped usdc
    };
    CONFIG.save(deps.storage, &config)?;

    ADDRESSES.save(deps.storage, &LaunchAddrs {
        osmosis_proxy: Addr::unchecked(""),
        oracle: Addr::unchecked(""),
        staking: Addr::unchecked(""),
        vesting: Addr::unchecked(""),
        governance: Addr::unchecked(""),
        positions: Addr::unchecked(""),
        stability_pool: Addr::unchecked(""),
        liq_queue: Addr::unchecked(""),
        liquidity_check: Addr::unchecked(""),
        mbrn_auction: Addr::unchecked(""),
        discount_vault: Addr::unchecked(""),
        system_discounts: Addr::unchecked(""),
    })?;

    let msg = CosmosMsg::Wasm(WasmMsg::Instantiate { 
        admin: Some(env.clone().contract.address.to_string()),
        code_id: config.clone().osmosis_proxy_id,
        msg: to_binary(&{})?,
        funds: vec![],
        label: String::from("osmosis_proxy") 
    });
    let sub_msg = SubMsg::reply_on_success(msg, OSMOSIS_PROXY_REPLY_ID);

    //Instantiate Lockdrop 
    let lockdrop = Lockdrop {
        locked_users: vec![],
        num_of_incentives: Uint128::new(25_000_000_000_000),
        locked_asset: AssetInfo::NativeToken { denom: String::from("uosmo") },
        lock_up_ceiling: 90,
        deposit_end: env.block.time.seconds() + (5 * SECONDS_PER_DAY),
        withdrawal_end: env.block.time.seconds() + (7 * SECONDS_PER_DAY),
        launched: false,
    };
    LOCKDROP.save(deps.storage, &lockdrop)?;

    //Instantiate Incentive Ratios
    INCENTIVE_RATIOS.save(deps.storage, &vec![])?;

    //Instantiate Credit Pools
    CREDIT_POOL_IDS.save(deps.storage, 
        &CreditPools {
            stableswap: 0,
            atom: 0,
            osmo: 0,
        }
    )?;

    Ok(Response::new()
        .add_submessage(sub_msg)
        .add_attribute("config", format!("{:?}", config))
        .add_attribute("contract_address", env.contract.address)
    )
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::Lock { lock_up_duration } => lock(deps, env, info, lock_up_duration),
        ExecuteMsg::Withdraw { withdrawal_amount, lock_up_duration } => withdraw(deps, env, info, withdrawal_amount, lock_up_duration),
        ExecuteMsg::Claim { } => claim(deps, env, info),
        ExecuteMsg::Launch{ } => end_of_launch(deps, env),
        ExecuteMsg::UpdateConfig(update) => update_config(deps, info, update),
    }
}

/// Deposit OSMO into the lockdrop & elect to lock MBRN rewards for a certain duration
fn lock(    
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    lock_up_duration: u64,
) -> Result<Response, ContractError>{
    let mut lockdrop = LOCKDROP.load(deps.storage)?;

    //Assert Lockdrop is in deposit period
    if env.block.time.seconds() > lockdrop.deposit_end { return Err(ContractError::DepositsOver {  }) }
    //Validate lockup duration
    if lock_up_duration > lockdrop.lock_up_ceiling {  return Err(ContractError::CustomError { val: String::from("Can't lock that long")}) }

    let valid_asset = validate_lockdrop_asset(info.clone(), lockdrop.clone().locked_asset)?;

    //Find & add to User
    if let Some((i, _lock_slot)) = lockdrop.clone().locked_users
        .into_iter()
        .enumerate()
        .find(|(_i, user)| user.user == info.clone().sender.to_string()){
        
        lockdrop.locked_users[i].deposits.push(
            Lock { 
                deposit: valid_asset.amount, 
                lock_up_duration: lock_up_duration.clone(),
            }
        );

    } else {
        //Add a User
        let user = LockedUser { 
            user: info.clone().sender.to_string(), 
            deposits: vec![Lock { 
                deposit: valid_asset.amount, 
                lock_up_duration: lock_up_duration.clone(),
            }],
            total_tickets: Uint128::zero(),
            incentives_withdrawn: Uint128::zero(),
        };
            
        lockdrop.locked_users.push(user);

    } 

    //Save Lockdrop
    LOCKDROP.save(deps.storage, &lockdrop)?;

    Ok(Response::new()
        .add_attributes(vec![
            attr("method", "deposit"),
            attr("user", info.clone().sender),
            attr("lock_up_duration", lock_up_duration.to_string()),
            attr("deposit", valid_asset.to_string()),
        ]))
}

/// Withdraw OSMO from the lockdrop during the withdrawal period
fn withdraw(    
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    mut withdrawal_amount: Uint128,
    lock_up_duration: u64,
) -> Result<Response, ContractError>{
    let mut lockdrop = LOCKDROP.load(deps.storage)?;

    //Assert Lockdrop is in withdraw period
    if env.block.time.seconds() < lockdrop.deposit_end || env.block.time.seconds() > lockdrop.withdrawal_end { return Err(ContractError::WithdrawalsOver {  }) }

    let initial_withdraw_amount = withdrawal_amount;

    //Find & remove from LockedUser
    if let Some((i, _lock_slot)) = lockdrop.clone().locked_users
        .into_iter()
        .enumerate()
        .find(|(_i, user)| user.user == info.clone().sender.to_string()){
        
            lockdrop.locked_users[i].deposits = lockdrop.clone().locked_users[i].clone().deposits
                .into_iter()
                .map(|mut deposit| {
                    if deposit.lock_up_duration == lock_up_duration {

                        if deposit.deposit >= withdrawal_amount {
                            deposit.deposit -= withdrawal_amount;
                            withdrawal_amount = Uint128::zero();
    
                            deposit
                        } else {
                            withdrawal_amount -= deposit.deposit;
                            deposit.deposit = Uint128::zero();
    
                            deposit
                        }

                    } else { deposit }                 
                    
                    
                })
                .collect::<Vec<Lock>>()
                .into_iter()
                .filter(|deposit| deposit.deposit != Uint128::zero())
                .collect::<Vec<Lock>>();

            if !withdrawal_amount.is_zero() {
                return Err(ContractError::CustomError { val: format!("This user only owns {} of the locked asset in this lockup duration: {}, retry withdrawal at or below that amount", initial_withdraw_amount - withdrawal_amount, lock_up_duration) })
            }

    } else {
        return Err(ContractError::NotAUser {})
    }

    //Save Lockdrop
    LOCKDROP.save(deps.storage, &lockdrop)?;

    //Create Withdraw Msg
    let msg = withdrawal_msg(
        Asset {
            info: lockdrop.clone().locked_asset,
            amount: initial_withdraw_amount.clone(),            
    }, info.clone().sender)?;

    Ok(Response::new()
        .add_message(msg)
        .add_attributes(vec![
            attr("method", "withdraw"),
            attr("user", info.clone().sender),
            attr("lock_up_duration", lock_up_duration.to_string()),
            attr("withdraw", initial_withdraw_amount),
        ]))
}

/// Claim unlocked MBRN rewards
fn claim(    
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError>{
    let mut lockdrop = LOCKDROP.load(deps.storage)?;

    //Assert lockdrop has ended
    if env.block.time.seconds() <= lockdrop.withdrawal_end {
        return Err(ContractError::CustomError { val: String::from("Lockdrop hasn't ended yet") })
    }

    let addrs = ADDRESSES.load(deps.storage)?;
    let config = CONFIG.load(deps.storage)?;

    //Only run the ticket calculation once 
    let mut user_ratios = INCENTIVE_RATIOS.load(deps.storage)?;
    
    if user_ratios.is_empty(){
        calc_ticket_distribution(deps.storage, &mut lockdrop)?;

        user_ratios = INCENTIVE_RATIOS.load(deps.storage)?;
    }
    
    //Claim any unlocked incentives
    let incentives = get_user_incentives(
        user_ratios,
        info.sender.to_string(),
        lockdrop.num_of_incentives,
    )?;
    
    let mut withdrawable_tickets = Uint128::zero();
    let amount_to_mint: Uint128;
    //Find withdrawable tickets
    if let Some((i, user)) = lockdrop.clone().locked_users.into_iter().enumerate().find(|(_i, user)| user.user == info.clone().sender){
        let time_since_lockdrop_end = env.block.time.seconds() - lockdrop.withdrawal_end;       

        for (_i, deposit) in user.clone().deposits.into_iter().enumerate() {
            //Unlock any deposits that have passed their lock duration
            if time_since_lockdrop_end > deposit.lock_up_duration * SECONDS_PER_DAY {
                withdrawable_tickets += deposit.deposit * Uint128::from((deposit.lock_up_duration + 1) as u128);
            }
        }

        //Calc ratio of incentives to unlock
        let ratio_of_unlock = decimal_division(
            Decimal::from_ratio(withdrawable_tickets, Uint128::one()), 
            Decimal::from_ratio(user.total_tickets, Uint128::one()))?;

        let unlocked_incentives = ratio_of_unlock * incentives;

        //Calc amount available to mint
        amount_to_mint = unlocked_incentives - user.incentives_withdrawn;
        //Update incentives withdraw
        lockdrop.locked_users[i].incentives_withdrawn += amount_to_mint;

    } else {
        return Err(ContractError::NotAUser {})
    }    
    //Save updated incentive tally
    LOCKDROP.save(deps.storage, &lockdrop)?;

    let attrs = vec![
        attr("method", "claim"),
        attr("minted_incentives", amount_to_mint),
    ];

    //Create mint & stake msgs if there are incentives to withdraw
    if !amount_to_mint.is_zero(){

        let mint_msg = CosmosMsg::Wasm(WasmMsg::Execute { 
            contract_addr: addrs.osmosis_proxy.to_string(), 
            msg: to_binary(&OPExecuteMsg::MintTokens { 
                denom: config.clone().mbrn_denom, 
                amount: amount_to_mint.clone(), 
                mint_to_address: info.clone().sender.to_string(), 
            })?, 
            funds: vec![] 
        });

        let stake_msg = CosmosMsg::Wasm(WasmMsg::Execute { 
            contract_addr: addrs.staking.to_string(), 
            msg: to_binary(&StakingExecuteMsg::Stake { user: Some(info.clone().sender.to_string()) })?, 
            funds: vec![coin(amount_to_mint.into(), config.clone().mbrn_denom)] 
        });

        Ok(Response::new()
            .add_attributes(attrs)
            .add_messages(vec![mint_msg, stake_msg])
        )
    } else {
        Ok(Response::new()
            .add_attributes(attrs))
    }
    
}

/// Return the amount of incentives a user is entitled to
fn get_user_incentives(
    user_ratios: Vec<UserRatio>,
    user: String,
    total_incentives: Uint128,
) -> StdResult<Uint128>{

    let incentives: Uint128 = match user_ratios.clone().into_iter().enumerate().find(|(_i, user_ratio)| user_ratio.user.to_string() == user){
        Some((_i, user)) => {

            decimal_multiplication(
                user.ratio, 
                Decimal::from_ratio(total_incentives, Uint128::one())
            )? * Uint128::one()
        },
        None => {
            return Err(StdError::GenericErr { msg: String::from("User didn't participate in the lockdrop") })
        },
    };

    Ok(incentives)
}

/// Calculate the ratio of incentives each user is entitled to
fn calc_ticket_distribution(
    storage: &mut dyn Storage,
    lockdrop: &mut Lockdrop,
) -> StdResult<()>{
    let mut error: Option<StdError> = None;

    let user_totals = lockdrop.clone().locked_users
        .into_iter()
        .map(|user| {
            let total_tickets: Uint128 = user.deposits
                .into_iter()
                .map(|deposit| deposit.deposit * Uint128::from(deposit.lock_up_duration + 1) )
                .collect::<Vec<Uint128>>()
                .into_iter()
                .sum();

            (user.user, total_tickets)
        })
        .collect::<Vec<(String, Uint128)>>();

    //Set each user's total_tickets
    for (i, total) in user_totals.clone().into_iter().enumerate(){
        lockdrop.locked_users[i].total_tickets = total.1;
    }

    let total_tickets: Uint128 = user_totals.clone()
        .into_iter()
        .map(|user| user.1)
        .collect::<Vec<Uint128>>()
        .into_iter()
        .sum();

    let user_ratios: Vec<UserRatio> = user_totals.clone()
        .into_iter()
        .map(|user| {
            let ratio = decimal_division(
                Decimal::from_ratio(user.1, Uint128::one()),
                Decimal::from_ratio(total_tickets, Uint128::one()),
            ).unwrap_or_else(|e| {
                error = Some(e);
                Decimal::zero()
            });

            UserRatio { user: Addr::unchecked(user.0), ratio }
        })
        .collect::<Vec<UserRatio>>();

    //Save user incentive ratios
    INCENTIVE_RATIOS.save(storage, &user_ratios)
}

/// Validate that the lockdrop asset is present in the message
fn validate_lockdrop_asset(info: MessageInfo, lockdrop_asset: AssetInfo) -> StdResult<Asset>{
    if let Some(lockdrop_asset) = info.clone().funds
        .into_iter()
        .find(|coin| coin.denom == lockdrop_asset.to_string()){

        Ok(Asset { 
            info: AssetInfo::NativeToken { denom: lockdrop_asset.denom }, 
            amount: lockdrop_asset.amount })
    } else {
        return Err(StdError::GenericErr { msg: format!("No valid lockdrop asset, looking for {}", lockdrop_asset) })
    }
}

/// Update contract configuration
fn update_config(
    deps: DepsMut,
    info: MessageInfo,
    update: UpdateConfig,
) -> Result<Response, ContractError> {
    let mut config = CONFIG.load(deps.storage)?;

    //Assert authority
    if info.sender != config.clone().labs_addr {
        return Err(ContractError::Unauthorized {});
    }

    if let Some(credit_denom) = update.credit_denom {
        config.credit_denom = credit_denom;
    }
    if let Some(mbrn_denom) = update.mbrn_denom {
        config.mbrn_denom = mbrn_denom;
    }
    if let Some(osmo_denom) = update.osmo_denom {
        config.osmo_denom = osmo_denom;
    }
    if let Some(usdc_denom) = update.usdc_denom {
        config.usdc_denom = usdc_denom;
    }

    //Save Config
    CONFIG.save(deps.storage, &config)?;

    Ok(Response::new().add_attribute("new_config", format!("{:?}", config)))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => to_binary(&CONFIG.load(deps.storage)?),
        QueryMsg::Lockdrop {} => to_binary(&LOCKDROP.load(deps.storage)?),
        QueryMsg::IncentiveDistribution {} => to_binary(&INCENTIVE_RATIOS.load(deps.storage)?),
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn reply(deps: DepsMut, env: Env, msg: Reply) -> StdResult<Response> {
    match msg.id {
        OSMOSIS_PROXY_REPLY_ID => handle_op_reply(deps, env, msg),
        ORACLE_REPLY_ID => handle_oracle_reply(deps, env, msg),
        STAKING_REPLY_ID => handle_staking_reply(deps, env, msg),
        VESTING_REPLY_ID => handle_vesting_reply(deps, env, msg),
        GOVERNANCE_REPLY_ID => handle_gov_reply(deps, env, msg),
        POSITIONS_REPLY_ID => handle_cdp_reply(deps, env, msg),
        STABILITY_POOL_REPLY_ID => handle_sp_reply(deps, env, msg),
        LIQ_QUEUE_REPLY_ID => handle_lq_reply(deps, env, msg),
        LIQUIDITY_CHECK_REPLY_ID => handle_lc_reply(deps, env, msg),
        DEBT_AUCTION_REPLY_ID => handle_auction_reply(deps, env, msg),
        STABLESWAP_REPLY_ID => handle_stableswap_reply(deps, env, msg),
        CREATE_DENOM_REPLY_ID => handle_create_denom_reply(deps, env, msg),
        id => Err(StdError::generic_err(format!("invalid reply id: {}", id))),
    }
}

/// This gets called at the end of the lockdrop.
/// Create MBRN & CDT pools and deposit into MBRN/OSMO pool.
pub fn end_of_launch(
    deps: DepsMut,
    env: Env,
) -> Result<Response, ContractError>{
    let mut lockdrop = LOCKDROP.load(deps.storage)?;

    //Assert launch hasn't happened yet, don't want this called twice
    if lockdrop.launched { return Err(ContractError::LaunchHappened {  }) }
    
    //Toggle launched and save
    lockdrop.launched = true;
    LOCKDROP.save(deps.storage, &lockdrop)?;

    //Assert Lockdrop withdraw period has ended
    if !(env.block.time.seconds() > lockdrop.withdrawal_end) { return Err(ContractError::LockdropOngoing {  }) }

    let config = CONFIG.load(deps.storage)?;
    let addrs = ADDRESSES.load(deps.storage)?;
    let mut msgs: Vec<CosmosMsg> = vec![];
    let mut sub_msgs: Vec<SubMsg> = vec![];

    //Get uosmo contract balance
    let uosmo_balance = get_contract_balances(deps.querier, env.clone(), vec![AssetInfo::NativeToken { denom: String::from("uosmo") }])?[0];
    //Make sure to deduct the amount of OSMO used to create Pools. Contract balance - 100uosmo * 4
    let uosmo_pool_delegation_amount = (uosmo_balance - Uint128::new(400_000_000)).to_string();

    //Mint MBRN for LP
    let msg = OPExecuteMsg::MintTokens { 
        denom: config.clone().mbrn_denom, 
        amount: config.clone().mbrn_launch_amount, 
        mint_to_address: env.clone().contract.address.to_string(),
    };
    let msg = CosmosMsg::Wasm(WasmMsg::Execute { 
        contract_addr: addrs.clone().osmosis_proxy.to_string(), 
        msg: to_binary(&msg)?, 
        funds: vec![], 
    });
    msgs.push(msg);
    
    //Create & deposit into MBRN-OSMO LP 
    let msg = MsgCreateBalancerPool {
        sender: env.contract.address.to_string(),
        pool_params: Some(PoolParams {
            swap_fee: String::from("0.2"),
            exit_fee: String::from("0"),
            smooth_weight_change_params: None,
        }),
        pool_assets: vec![
            PoolAsset { 
                token: Some(Coin { denom: config.clone().mbrn_denom, amount: config.clone().mbrn_launch_amount.to_string() }), 
                weight: String::from("50") 
            },
            PoolAsset { 
                token: Some(Coin { denom: config.clone().osmo_denom, amount: uosmo_pool_delegation_amount }), 
                weight: String::from("50") 
            }
        ],
        future_pool_governor: addrs.clone().governance.to_string(),
    };
    msgs.push(msg.into());
    //Create 3 CDT pools
    //OSMO
    let msg: CosmosMsg = MsgCreateBalancerPool {
        sender: env.contract.address.to_string(),
        pool_params: Some(PoolParams {
            swap_fee: String::from("0.2"),
            exit_fee: String::from("0"),
            smooth_weight_change_params: None,
        }),
        pool_assets: vec![
            PoolAsset { 
                token: Some(Coin { denom: config.clone().credit_denom, amount: "0".to_string() }), 
                weight: String::from("50") 
            },
            PoolAsset { 
                token: Some(Coin { denom: config.clone().osmo_denom, amount: "0".to_string() }), 
                weight: String::from("50") 
            }
        ],
        future_pool_governor: addrs.clone().governance.to_string(),
    }.into();
    let sub_msg = SubMsg::reply_on_success(msg, BALANCER_POOL_REPLY_ID);
    sub_msgs.push(sub_msg);
    //ATOM
    let msg: CosmosMsg = MsgCreateBalancerPool {
        sender: env.contract.address.to_string(),
        pool_params: Some(PoolParams {
            swap_fee: String::from("0.2"),
            exit_fee: String::from("0"),
            smooth_weight_change_params: None,
        }),
        pool_assets: vec![
            PoolAsset { 
                token: Some(Coin { denom: config.clone().credit_denom, amount: "0".to_string() }), 
                weight: String::from("50") 
            },
            PoolAsset { 
                token: Some(Coin { denom: config.clone().atom_denom, amount: "0".to_string() }), 
                weight: String::from("50") 
            }
        ],
        future_pool_governor: addrs.clone().governance.to_string(),
    }.into();
    let sub_msg = SubMsg::reply_on_success(msg, BALANCER_POOL_REPLY_ID);
    sub_msgs.push(sub_msg);
    //USDC Stableswap
    let msg: CosmosMsg = MsgCreateStableswapPool {
        sender: env.contract.address.to_string(),
        pool_params: Some(SSPoolParams {
            swap_fee: String::from("0.05"),
            exit_fee: String::from("0"),
        }),
        initial_pool_liquidity: vec![
            Coin { denom: config.clone().credit_denom, amount: "0".to_string() },
            Coin { denom: config.clone().usdc_denom, amount: "0".to_string() },
        ],
        future_pool_governor: addrs.clone().governance.to_string(),
    }.into();
    let sub_msg = SubMsg::reply_on_success(msg, STABLESWAP_REPLY_ID);
    sub_msgs.push(sub_msg);

    Ok(Response::new()
        .add_messages(msgs)
        .add_submessages(sub_msgs)
    )
}





