use std::str::FromStr;
use std::vec;

use cosmwasm_std::{
    attr, to_binary, Addr, Api, BankMsg, Coin, CosmosMsg, Decimal, DepsMut, Env, MessageInfo,
    QuerierWrapper, QueryRequest, Response, StdError, StdResult, Storage, SubMsg, Uint128, WasmMsg,
    WasmQuery,
};

use membrane::helpers::{validate_position_owner, asset_to_coin, withdrawal_msg, get_contract_balances};
use membrane::cdp::{Config, EditBasket};
use membrane::oracle::{AssetResponse, PriceResponse};
use membrane::liq_queue::ExecuteMsg as LQ_ExecuteMsg;
use membrane::liquidity_check::ExecuteMsg as LiquidityExecuteMsg;
use membrane::staking::{ExecuteMsg as Staking_ExecuteMsg, QueryMsg as Staking_QueryMsg, Config as Staking_Config};
use membrane::oracle::{ExecuteMsg as OracleExecuteMsg, QueryMsg as OracleQueryMsg};
use membrane::osmosis_proxy::{ExecuteMsg as OsmoExecuteMsg, QueryMsg as OsmoQueryMsg };
use membrane::stability_pool::ExecuteMsg as SP_ExecuteMsg;
use membrane::math::{decimal_division, decimal_multiplication, Uint256, decimal_subtraction};
use membrane::types::{
    cAsset, Asset, AssetInfo, AssetOracleInfo, Basket, LiquidityInfo, Position, PoolStateResponse,
    SupplyCap, UserInfo, PoolType, RedemptionInfo, PositionRedemption, PoolInfo, LPAssetInfo
};

use crate::query::{get_cAsset_ratios, get_avg_LTV, insolvency_check};
use crate::rates::accrue;
use crate::risk_engine::update_basket_tally;
use crate::state::{get_target_position, update_position, update_position_claims, ClosePositionPropagation, CollateralVolatility, Timer, BASKET, CLOSE_POSITION, FREEZE_TIMER, REDEMPTION_OPT_IN, STORED_PRICES, VOLATILITY};
use crate::{
    state::{
        WithdrawPropagation, CONFIG, POSITIONS, LIQUIDATION, WITHDRAW,
    },
    ContractError,
};

//Liquidation reply ids
pub const LIQ_QUEUE_REPLY_ID: u64 = 1u64;
pub const SP_REPLY_ID: u64 = 2u64;
pub const USER_SP_REPAY_REPLY_ID: u64 = 3u64;

pub const WITHDRAW_REPLY_ID: u64 = 4u64;
pub const BAD_DEBT_REPLY_ID: u64 = 999999u64;


//Constants
const MAX_POSITIONS_AMOUNT: u32 = 3;


/// Deposit collateral to existing position. New or existing collateral.
/// Anyone can deposit, to any position. Owner restrictions for withdrawals.
pub fn deposit(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    position_owner: Option<String>,
    position_id: Option<Uint128>,
    cAssets: Vec<cAsset>,
) -> Result<Response, ContractError> {    
    let config = CONFIG.load(deps.storage)?;
    let valid_owner_addr = validate_position_owner(deps.api, info, position_owner)?;
    let mut basket: Basket = BASKET.load(deps.storage)?;
    
    //Check if frozen
    if basket.frozen { return Err(ContractError::Frozen {  }) }

    //Set deposit_amounts to double check state storage 
    let deposit_amounts: Vec<Uint128> = cAssets.clone()
        .into_iter()
        .map(|cAsset| cAsset.asset.amount)
        .collect::<Vec<Uint128>>();

    //Initialize positions_prev_collateral & position_info for deposited assets
    //Used for to double check state storage
    let mut positions_prev_collateral = vec![];
    let position_info: UserInfo;

    if let Ok(mut positions) = POSITIONS.load(deps.storage, valid_owner_addr.clone()){

        //Enforce max positions
        if positions.len() >= MAX_POSITIONS_AMOUNT as usize {
            return Err(ContractError::MaxPositionsReached {});
        }

        //Add collateral to the position_id or Create a new position 
        if let Some(position_id) = position_id {
            //Find the position
            if let Some((position_index, mut position)) = positions.clone()
                .into_iter()
                .enumerate()
                .find(|(_i, position)| position.position_id == position_id){

                //Store position_info for reply
                position_info = UserInfo {
                    position_id,
                    position_owner: valid_owner_addr.to_string(),
                };

                for deposit in cAssets.clone(){
                    //Search for cAsset in the position 
                    if let Some((collateral_index, cAsset)) = position.clone().collateral_assets
                        .into_iter()
                        .enumerate()
                        .find(|(_i, cAsset)| cAsset.asset.info.equal(&deposit.asset.info)){
                        //Store positions_prev_collateral
                        positions_prev_collateral.push(cAsset.clone().asset);

                        //Add to existing cAsset
                        position.collateral_assets[collateral_index].asset.amount += deposit.asset.amount;

                    } else { //Add new cAsset object to position
                        position.collateral_assets.push( deposit.clone() );

                        let placeholder_asset = Asset {
                            amount: Uint128::zero(),
                            ..deposit.clone().asset
                        };
                        //Store positions_prev_collateral
                        positions_prev_collateral.push(placeholder_asset.clone());

                    }
                }
                //Set updated position
                positions[position_index] = position.clone();
                
                //Accrue
                accrue(
                    deps.storage,
                    deps.querier,
                    env.clone(),
                    config.clone(),
                    &mut position,
                    &mut basket,
                    valid_owner_addr.to_string(),
                    true,
                )?;
                //Save Updated Vec<Positions> for the user
                POSITIONS.save(deps.storage, valid_owner_addr, &positions)?;

                if !position.credit_amount.is_zero() {
                    //Update Supply caps
                    update_basket_tally(
                        deps.storage, 
                        deps.querier, 
                        env.clone(), 
                        &mut basket, 
                        cAssets.clone(),
                        position.clone().collateral_assets,
                        true,
                        config.clone(),
                        false,
                    )?;
                }
                //Save Basket
                BASKET.save(deps.storage, &basket)?;

            } else {                
                //If position_ID is passed but no position is found, Error. 
                //In case its a mistake, don't want to add assets to a new position.
                return Err(ContractError::NonExistentPosition { id: position_id });
            }
        } else { //If user doesn't pass an ID, we create a new position
            let (new_position_info, new_position) = create_position_in_deposit(
                deps.storage,
                deps.querier,
                env,
                config.clone(),
                valid_owner_addr.clone(),
                cAssets.clone(),
                &mut basket
            )?;

            //Update position_info for state check
            position_info = new_position_info;

            //Add new position to the user's Vec<Positions>
            POSITIONS.update(
                deps.storage,
                valid_owner_addr,
                |positions| -> StdResult<_> {
                    let mut positions = positions.unwrap_or_default();
                    positions.push(new_position);
                    Ok(positions)
                },
            )?;
        }
    } else { //No existing positions loaded so new Vec<Position> is created
        let (new_position_info, new_position) = create_position_in_deposit(
            deps.storage,
            deps.querier,
            env,
            config.clone(),
            valid_owner_addr.clone(),
            cAssets.clone(),
            &mut basket
        )?;

        //Update position_info for state check
        position_info = new_position_info;

        //Add new Vec of Positions to state under the user
        POSITIONS.save(
            deps.storage,
            valid_owner_addr,
            &vec![new_position],
        )?;
    }

    //Double check State storage
    check_deposit_state(deps.storage, deps.api, positions_prev_collateral, deposit_amounts, position_info.clone())?;    

    Ok(Response::new().add_attributes(vec![
        attr("method", "deposit"),
        attr("position_owner", position_info.position_owner),
        attr("position_id", position_info.position_id),
        attr("assets", format!("{:?}", cAssets.into_iter().map(|a|a.asset).collect::<Vec<Asset>>())),
    ]))
}

/// Function used to create & save a position, then update state.
/// This is a helper function to reduce the size of the deposit function.
fn create_position_in_deposit(
    storage: &mut dyn Storage,
    querier: QuerierWrapper,
    env: Env,
    config: Config,
    valid_owner_addr: Addr,
    cAssets: Vec<cAsset>,
    basket: &mut Basket,
) -> Result<(UserInfo, Position), ContractError> {
    let mut new_position = create_position(cAssets, basket)?;

    //Store position_info for reply
    let position_info = UserInfo {
        position_id: new_position.clone().position_id,
        position_owner: valid_owner_addr.to_string(),
    };

    //Accrue, mainly for repayment price
    accrue(
        storage,
        querier,
        env,
        config.clone(),
        &mut new_position,
        basket,
        valid_owner_addr.to_string(),
        true,
    )?;
    //Save Basket
    BASKET.save(storage, basket)?;

    Ok((position_info, new_position))
}

/// Function used to validate the state of the contract after a deposit
fn check_deposit_state(
    storage: &mut dyn Storage,  
    api: &dyn Api,   
    positions_prev_collateral: Vec<Asset>, //Amount of collateral in the position before the deposit
    deposit_amounts: Vec<Uint128>,
    position_info: UserInfo,
) -> Result<(), ContractError>{
    let (_i, target_position) = get_target_position(
        storage, 
        api.addr_validate(&position_info.position_owner)?, 
        position_info.position_id
    )?;

    for (i, asset) in positions_prev_collateral.clone().into_iter().enumerate(){

        if let Some(cAsset) = target_position.clone().collateral_assets
            .into_iter()
            .find(|cAsset| cAsset.asset.info.equal(&asset.info)){

            //Assert cAsset total is equal to the amount deposited + prev_asset_amount
            if cAsset.asset.amount != asset.amount + deposit_amounts[i] {
                return Err(ContractError::CustomError { val: String::from("Conditional 1: Possible state error") })
            }
        }
    }

    //If a deposit to a new position, asset amounts should be exactly what was deposited
    if positions_prev_collateral == vec![] {
        for (i, cAsset) in target_position.collateral_assets.into_iter().enumerate() {
            if cAsset.asset.amount != deposit_amounts[i] {
                return Err(ContractError::CustomError { val: String::from("Deposit Conditional 2: Possible state error") })
            }
        }
    }

    Ok(())
}

/// Withdraws assets from a position.
/// Validates withdraw amount & updates state.
pub fn withdraw(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    position_id: Uint128,
    cAssets: Vec<cAsset>,
    send_to: Option<String>,
) -> Result<Response, ContractError> {
    let config: Config = CONFIG.load(deps.storage)?;
    let mut basket: Basket = BASKET.load(deps.storage)?;
    let mut msgs = vec![];

    //Check if frozen
    if basket.frozen { return Err(ContractError::Frozen {  }) }

    //Set recipient
    let mut recipient = info.clone().sender;
    if let Some(string) = send_to.clone() {
        recipient = deps.api.addr_validate(&string)?;
    } 

    //Set position owner
    let mut valid_position_owner = info.clone().sender;

    //If the contract is withdrawing for a user (i.e. ClosePosition), set the position owner to the recipient
    if info.sender == env.contract.address && send_to.is_some(){
        valid_position_owner = recipient.clone();
    }

    //This forces withdrawals to be done by the info.sender
    let (position_index, mut target_position) = get_target_position(deps.storage, valid_position_owner.clone(), position_id)?;
    //Accrue interest
    accrue(
        deps.storage,
        deps.querier,
        env.clone(),
        config.clone(),
        &mut target_position,
        &mut basket,
        valid_position_owner.to_string(),
        false,
    )?;

    //For supply cap updates
    let mut tally_update_list: Vec<cAsset> = vec![];

    //Set withdrawal prop variables for state checks
    let mut prop_assets = vec![];
    let mut withdraw_amounts: Vec<Uint128> = vec![];

    //For Withdraw Msg
    let mut withdraw_coins: Vec<Coin> = vec![];

    //Check for expunged assets and assert they are being withdrawn
    check_for_expunged(target_position.clone().collateral_assets, cAssets.clone(), basket.clone() )?;

    //Attempt to withdraw each cAsset
    for cAsset in cAssets.clone() {
        let withdraw_asset = cAsset.asset;             

        //Find cAsset in target_position
        if let Some((collateral_index, position_collateral)) = target_position.clone().collateral_assets
            .into_iter()
            .enumerate()
            .find(|(_i, cAsset)| cAsset.asset.info.equal(&withdraw_asset.info)){
            //If the cAsset is found in the position, attempt withdrawal
            //Cant withdraw more than the positions amount
            if withdraw_asset.amount > position_collateral.asset.amount {
                return Err(ContractError::InvalidWithdrawal {});
            } else {
                //Now that its a valid withdrawal and debt has accrued, we can add to tally_update_list
                //This will be used to keep track of Basket supply caps
                tally_update_list.push(cAsset {
                    asset: withdraw_asset.clone(),
                    ..position_collateral.clone()
                });

                //Withdraw Prop: Push the initial asset
                prop_assets.push(position_collateral.clone().asset);

                //Update cAsset data to account for the withdrawal
                let leftover_amount = position_collateral.asset.amount - withdraw_asset.amount;               

                //Delete asset from the position if the amount is being fully withdrawn, otherwise edit.
                if leftover_amount != Uint128::new(0u128) {
                    target_position.collateral_assets[collateral_index].asset.amount = leftover_amount;
                } else {
                    target_position.collateral_assets.remove(collateral_index);
                }

                //If resulting LTV makes the position insolvent, error. If not construct withdrawal_msg
                //This is taking max_borrow_LTV so users can't max borrow and then withdraw to get a higher initial LTV
                let (insolvency_res, _) = insolvency_check(
                    deps.storage,
                    env.clone(),
                    deps.querier,
                    Some(basket.clone()),
                    target_position.clone().collateral_assets,
                    target_position.clone().credit_amount,
                    basket.clone().credit_price,
                    true,
                    config.clone(),
                )?;
                if insolvency_res.0 {
                    return Err(ContractError::PositionInsolvent { insolvency_res });
                } else {
                    //Update Position list
                    POSITIONS.update(deps.storage, valid_position_owner.clone(), |positions: Option<Vec<Position>>| -> Result<Vec<Position>, ContractError>{

                        let mut updating_positions = positions.unwrap_or_else(|| vec![]);

                        //If new position isn't empty, update
                        if !check_for_empty_position(target_position.clone().collateral_assets){
                            updating_positions[position_index] = target_position.clone();
                        } else { // remove position that was withdrawn from
                            updating_positions.remove(position_index);
                        }

                        Ok( updating_positions )                    
                    })?;
                    //load to check if positions list is fully empty
                    let positions = POSITIONS.load(deps.storage, valid_position_owner.clone())?;
                    //Delete if empty
                    if positions.is_empty(){
                        POSITIONS.remove(deps.storage, valid_position_owner.clone());
                    }

                }
                
                //Push withdraw asset to list for withdraw prop
                withdraw_amounts.push(withdraw_asset.clone().amount);

                //Add to native token send list
                if let AssetInfo::NativeToken { denom: _ } = withdraw_asset.clone().info {
                    //Push to withdraw_coins
                    withdraw_coins.push(asset_to_coin(withdraw_asset)?);
                }
            }
        }         
    };
    
    //Push aggregated native coin withdrawal
    if withdraw_coins != vec![] {
        let message = CosmosMsg::Bank(BankMsg::Send {
            to_address: recipient.to_string(),
            amount: withdraw_coins,
        });
        msgs.push(SubMsg::reply_on_success(message, WITHDRAW_REPLY_ID));
    }

    //Update supply cap tallies
    if !target_position.clone().credit_amount.is_zero(){        
        //Update basket supply cap tallies after the full withdrawal to improve UX by smoothing debt_cap restrictions
        update_basket_tally(
            deps.storage,
            deps.querier,
            env.clone(),
            &mut basket,
            tally_update_list,
            target_position.clone().collateral_assets,
            false,
            config.clone(),
            false,
        )?;
    } 
    //Save updated repayment price and asset tallies
    BASKET.save(deps.storage, &basket)?;
    
    //Set Withdrawal_Prop
    let prop_assets_info: Vec<AssetInfo> = prop_assets
        .clone()
        .into_iter()
        .map(|asset| asset.info)
        .collect::<Vec<AssetInfo>>();
    
    let withdrawal_prop = WithdrawPropagation {
        positions_prev_collateral: prop_assets,
        withdraw_amounts,
        contracts_prev_collateral_amount: get_contract_balances(
            deps.querier,
            env,
            prop_assets_info,
        )?,
        position_info: UserInfo {
            position_id,
            position_owner: info.sender.to_string(),
        },
    };
    WITHDRAW.save(deps.storage, &withdrawal_prop)?;

    Ok(Response::new()
        .add_attributes(vec![
            attr("method", "withdraw"),
            attr("position_id", position_id),
            attr("assets", format!("{:?}", cAssets)),
        ])
        .add_submessages(msgs))
}

/// Use credit to repay outstanding debt in a Position.
/// Validates repayment & updates state.
/// Note: Excess repayment defaults to the sending address.
pub fn repay(
    storage: &mut dyn Storage,
    querier: QuerierWrapper,
    api: &dyn Api,
    env: Env,
    info: MessageInfo,
    position_id: Uint128,
    position_owner: Option<String>,
    credit_asset: Asset,
    send_excess_to: Option<String>,
) -> Result<Response, ContractError> {
    let config: Config = CONFIG.load(storage)?;
    let mut basket: Basket = BASKET.load(storage)?;

    //Check if frozen
    if basket.frozen { return Err(ContractError::Frozen {  }) }

    //Validate position owner 
    let valid_owner_addr = validate_position_owner(api, info.clone(), position_owner)?;
    
    //Get target_position
    let (position_index, mut target_position) = get_target_position(storage, valid_owner_addr.clone(), position_id)?;

    //SP accrues external before calling repay, so we only accrue if the sender isn't the SP
    if info.sender != config.clone().stability_pool.unwrap_or(Addr::unchecked("")){   
        //Accrue interest
        accrue(
            storage,
            querier,
            env.clone(),
            config.clone(),
            &mut target_position,
            &mut basket,
            valid_owner_addr.to_string(),
            false,
        )?;
    }

    //Set prev_credit_amount for state checks
    let prev_credit_amount = target_position.credit_amount;
    
    let mut messages = vec![];
    let mut excess_repayment = Uint128::zero();

    //Repay amount sent
    target_position.credit_amount = match target_position.credit_amount.checked_sub(credit_asset.amount){
        Ok(difference) => difference,
        Err(_err) => {
            //Set excess_repayment
            excess_repayment = credit_asset.amount - target_position.credit_amount;
            
            Uint128::zero()
        },
    };

    //Update Supply caps if this clears all debt
    if target_position.credit_amount.is_zero(){
        update_basket_tally(
            storage, 
            querier, 
            env.clone(), 
            &mut basket, 
            target_position.collateral_assets.clone(),
            target_position.clone().collateral_assets,
            false,
            config.clone(),
            false,
        )?;
    }

    //Position's resulting debt can't be below minimum without being fully repaid
    if basket.clone().credit_price.get_value(target_position.credit_amount)? < Decimal::from_ratio(config.debt_minimum, Uint128::one())
        && !target_position.credit_amount.is_zero(){
        //Router contract, Stability Pool & Liquidation Queue are allowed to.
        //Router: We rather $1 of bad debt than $2000 and bad debt comes from swap slippage
        //SP & LQ: If the resulting debt is below the minimum, the whole loan is liquidated so it won't be under the minimum by the end of the liquidation process
        let mut let_pass = false;
        if let Some(router) = config.clone().dex_router {
            if info.sender == router { let_pass = true; }
        }
        if let Some(stability_pool) = config.clone().stability_pool {
            if info.sender == stability_pool { let_pass = true; }
        }
        if let Some(liq_queue) = basket.clone().liq_queue {
            if info.sender == liq_queue { let_pass = true; }
        }
        if !let_pass {
            return Err(ContractError::BelowMinimumDebt { minimum: config.debt_minimum, debt: basket.clone().credit_price.get_value(target_position.credit_amount)?.to_uint_floor() });
        }
        //This would also pass for ClosePosition, but since spread is added to collateral amount this should never happen
        //Even if it does, the subsequent withdrawal would then error
    }
    
    //To indicate removed positions during ClosePosition
    let mut removed = false;
    //Update Position
    POSITIONS.update(storage, valid_owner_addr.clone(), |positions: Option<Vec<Position>>| -> Result<Vec<Position>, ContractError> {
        let mut updating_positions = positions.unwrap_or_else(|| vec![]);

        //If new position isn't empty, update
        if !check_for_empty_position(updating_positions[position_index].clone().collateral_assets){
            updating_positions[position_index] = target_position.clone();
        } else { // remove repaying position
            updating_positions.remove(position_index);
            removed = true;
        }
        
        Ok(updating_positions)
    })?;

    //Burn repayment & send revenue to stakers
    let burn_and_rev_msgs = credit_burn_rev_msg(
        config.clone(),
        env.clone(),
        Asset {
            amount: credit_asset.clone().amount - excess_repayment,
            ..credit_asset.clone()
        },
        &mut basket,
    )?;
    messages.extend(burn_and_rev_msgs);

    //Send back excess repayment, defaults to the repaying address
    if !excess_repayment.is_zero() {
        if let Some(addr) = send_excess_to {
            let valid_addr = api.addr_validate(&addr)?;

            let msg = withdrawal_msg(Asset {
                amount: excess_repayment,
                ..basket.clone().credit_asset
            }, valid_addr )?;

            messages.push(msg);
        } else {
            let msg = withdrawal_msg(Asset {
                amount: excess_repayment,
                ..basket.clone().credit_asset
            }, info.sender )?;

            messages.push(msg);
        }                                
    }

    //Subtract paid debt from Basket
    basket.credit_asset.amount = match basket.credit_asset.amount.checked_sub(credit_asset.amount - excess_repayment){
        Ok(difference) => difference,
        Err(_err) => Uint128::zero(),
    };

    //Save updated repayment price and debts
    BASKET.save(storage, &basket)?;

    if !removed {
        //Check that state was saved correctly
        check_repay_state(
            storage,
            credit_asset.amount - excess_repayment, 
            prev_credit_amount, 
            position_id, 
            valid_owner_addr
        )?;
    }
    
    Ok(Response::new()
        .add_messages(messages)
        .add_attributes(vec![
            attr("method", "repay"),
            attr("position_id", position_id),
            attr("loan_amount", target_position.credit_amount),
    ]))
}

/// Asserts valid state after repay()
fn check_repay_state(
    storage: &mut dyn Storage,
    repay_amount: Uint128,
    prev_credit_amount: Uint128,
    position_id: Uint128,
    position_owner: Addr,
) -> Result<(), ContractError>{

    //Get target_position
    let (_i, target_position) = get_target_position(storage, position_owner, position_id)?;

    //If repay amount should've 0'd the position's debt and it didn't error
    if repay_amount >= prev_credit_amount && target_position.credit_amount != Uint128::zero(){ 
        return Err(ContractError::CustomError { val: String::from("Conditional 1: Possible state error") })
    } else {
        //Assert that the stored credit_amount is equal to the origin - what was repayed
        if target_position.credit_amount != prev_credit_amount - repay_amount {
            return Err(ContractError::CustomError { val: String::from("Conditional 2: Possible state error") })
        }
    }

    Ok(())
}

/// This is what the stability pool contract calls to repay for a liquidation and get its collateral distribution
pub fn liq_repay(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    mut credit_asset: Asset,
) -> Result<Response, ContractError> {
    //Fetch liquidation info and state propagation
    let mut liquidation_propagation = LIQUIDATION.load(deps.storage)?;    
    let config = liquidation_propagation.clone().config;
    let mut basket = liquidation_propagation.clone().basket;

    //Can only be called by the SP contract
    if config.stability_pool.is_none() || info.sender != config.clone().stability_pool.unwrap_or_else(|| Addr::unchecked("")){
        return Err(ContractError::Unauthorized { owner: config.owner.to_string() });
    }
    //This position has collateral & credit_amount updated in the liquidation process...
    // from LQ replies && fee handling
    let mut target_position = liquidation_propagation.clone().target_position;
    
    let mut messages = vec![];
    let mut excess_repayment = Uint128::zero();
    //Update credit amount in target_position to account for SP's repayment
    target_position.credit_amount = match target_position.credit_amount.checked_sub(credit_asset.amount){
        Ok(difference) => {
            //LQ rounding errors can cause the repay_amount to be 1e-6 off
            if difference == Uint128::one(){
                Uint128::zero()
            } else {
                difference
            }
        },
        Err(_err) => {
            //Send the excess repayment back to the SP
            excess_repayment = credit_asset.amount - target_position.credit_amount;

            let excess_repayment_msg = withdrawal_msg(
                Asset {
                    amount: excess_repayment,
                    ..basket.clone().credit_asset
                },
                config.clone().stability_pool.unwrap_or_else(|| Addr::unchecked("")),
            )?;
            //Update credit_asset amount so its correct for the burn
            credit_asset.amount = target_position.credit_amount;

            //Add msg
            messages.push(excess_repayment_msg);

            Uint128::zero()
        },
    };
    
    //Burn repayment & send revenue to stakers
    let burn_and_rev_msgs = credit_burn_rev_msg(
        config.clone(),
        env.clone(),
        credit_asset.clone(),
        &mut basket,
    )?;
    messages.extend(burn_and_rev_msgs);

    //Subtract paid debt from Basket
    basket.credit_asset.amount = match basket.credit_asset.amount.checked_sub(credit_asset.amount){
        Ok(difference) => difference,
        Err(_err) => return Err(ContractError::CustomError { val: String::from("Repay amount is greater than Basket credit amount in liq_repay") }),
    };
   
    //Set collateral_assets
    let collateral_assets = target_position.clone().collateral_assets;

    //Get position's cAsset ratios
    let (cAsset_ratios, cAsset_prices) = (liquidation_propagation.clone().cAsset_ratios, liquidation_propagation.clone().cAsset_prices);

    let repay_value = basket.clone().credit_price.get_value(credit_asset.amount)?;

    //Add repay amount && user_repay_amount to total repaid
    //This makes the assumption that if the SP liquidation is successful, the user_repay_amount was too
    liquidation_propagation.total_repaid +=  Decimal::from_ratio(credit_asset.amount, Uint128::new(1u128));

    //Error if the caller fee is more than the total repaid value
    let repaid_value = basket.clone().credit_price.get_value(liquidation_propagation.clone().total_repaid.to_uint_floor())?;
    if liquidation_propagation.clone().caller_fee_value_paid > repaid_value {
        return Err(ContractError::CustomError { val: String::from("Caller fee is greater than total repaid value") });
    }

    //Stability Pool receives pro rata assets
    //Add distribute messages to the message builder, so the contract knows what to do with the received funds
    let mut distribution_assets = vec![];

    let mut coins: Vec<Coin> = vec![];    

    //Get SP liq fee
    let sp_liq_fee = liquidation_propagation.sp_liq_fee;

    //Calculate distribution of assets to send from the repaid position
    for (num, cAsset) in collateral_assets.into_iter().enumerate() {

        let collateral_repay_value = decimal_multiplication(repay_value, cAsset_ratios[num])?;
        let collateral_repay_amount = cAsset_prices[num].get_amount(collateral_repay_value)?;

        //Add fee %
        let collateral_w_fee = collateral_repay_amount * (sp_liq_fee+Decimal::one());

        //Set distribution asset
        let distribution_asset: Asset = Asset {
            amount: collateral_w_fee,
            ..cAsset.clone().asset
        };
        
        //Remove collateral from user's position claims
        target_position.collateral_assets[num].asset.amount -= collateral_w_fee;
        liquidation_propagation.liquidated_assets.push(
            cAsset {
                asset: distribution_asset.clone(),
                ..cAsset.clone()
            }
        );

        //SP Distribution needs list of cAsset's and is pulling the amount from the Asset object
        distribution_assets.push(distribution_asset.clone());
        coins.push(asset_to_coin(distribution_asset)?);
    }

    if target_position.credit_amount.is_zero(){                
        //Remove position's assets from Supply caps 
        update_basket_tally(
            deps.storage, 
            deps.querier, 
            env.clone(), 
            &mut basket,
            [target_position.clone().collateral_assets, liquidation_propagation.clone().liquidated_assets].concat(),
            target_position.clone().collateral_assets,
            false, 
            config.clone(),
            true,
        )?;
    } else {
        //Remove liquidated assets from Supply caps
        update_basket_tally(
            deps.storage, 
            deps.querier, 
            env.clone(), 
            &mut basket,
            liquidation_propagation.liquidated_assets,
            target_position.clone().collateral_assets,
            false,
            config.clone(),
            true,
        )?;
    }

    //Update position
    update_position(deps.storage, liquidation_propagation.position_owner, target_position)?;
    //Update Basket
    BASKET.save(deps.storage, &basket)?;

    //Adds Native token distribution msg to messages
    let distribution_msg = SP_ExecuteMsg::Distribute {
        distribution_assets: distribution_assets.clone(),
        distribution_asset_ratios: cAsset_ratios, //The distributions are based off cAsset_ratios so they shouldn't change
        distribute_for: credit_asset.amount,
    };
    //Build the Execute msg w/ the full list of native tokens
    let msg = CosmosMsg::Wasm(WasmMsg::Execute {
        contract_addr: config.stability_pool.unwrap_or_else(|| Addr::unchecked("")).to_string(),
        msg: to_binary(&distribution_msg)?,
        funds: coins,
    });
    messages.push(msg);
    
    Ok(Response::new()
        .add_messages(messages)
        .add_attribute("method", "liq_repay")
        .add_attribute("distribution_assets", format!("{:?}", distribution_assets))
        .add_attribute("distribute_for", credit_asset.amount)
        .add_attribute("excess", excess_repayment))
}

/// Increase debt of a position.
/// Accrue and validate credit amount.
/// Check for insolvency & update basket debt tally.
pub fn increase_debt(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    position_id: Uint128,
    amount: Option<Uint128>,
    LTV: Option<Decimal>,
    mint_to_addr: Option<String>,
) -> Result<Response, ContractError> {
    let config: Config = CONFIG.load(deps.storage)?;
    let mut basket: Basket = BASKET.load(deps.storage)?;

    //Check if frozen
    if basket.frozen { return Err(ContractError::Frozen {  }) }

    //Get Target position
    let (position_index, mut target_position) = get_target_position(deps.storage, info.clone().sender, position_id)?;

    //Accrue interest
    accrue(
        deps.storage,
        deps.querier,
        env.clone(),
        config.clone(),
        &mut target_position,
        &mut basket,
        info.sender.to_string(),
        false,
    )?;

    //Set prev_credit_amount
    let prev_credit_amount = target_position.credit_amount;
    let prev_basket_credit = basket.credit_asset.amount;

    //Update Supply caps if this is the first debt taken out
    if prev_credit_amount.is_zero() {
        update_basket_tally(
            deps.storage, 
            deps.querier, 
            env.clone(), 
            &mut basket, 
            target_position.collateral_assets.clone(),
            target_position.clone().collateral_assets,
            true,
            config.clone(),
            false,
        )?;
    }

    //Set amount
    let amount = match amount {
        Some(amount) => amount,
        None => {
            if let Some(LTV) = LTV {
                get_amount_from_LTV(deps.storage, deps.querier, env.clone(), config.clone(), target_position.clone(), basket.clone(), LTV)?
            } else {
                return Err(ContractError::CustomError { val: String::from("If amount isn't passed, LTV must be passed") })
            }            
        }
    };

    //Add new credit_amount
    target_position.credit_amount += amount;

    //Test for minimum debt requirements
    if  basket.clone().credit_price.get_value(target_position.credit_amount)? < Decimal::from_ratio(config.debt_minimum, Uint128::new(1u128))
    {        
        return Err(ContractError::BelowMinimumDebt { minimum: config.debt_minimum, debt: basket.clone().credit_price.get_value(target_position.credit_amount)?.to_uint_floor() });
    }

    let message: CosmosMsg;

    //Can't take credit before an oracle is set
    if basket.oracle_set {
        //If resulting LTV makes the position insolvent, error. If not construct mint msg
        let (insolvency_res, _) = insolvency_check(
            deps.storage,
            env.clone(),
            deps.querier,
            Some(basket.clone()),
            target_position.clone().collateral_assets,
            target_position.credit_amount,
            basket.clone().credit_price,
            true,
            config.clone(),
        )?;

        if insolvency_res.0 {
            return Err(ContractError::PositionInsolvent { insolvency_res });
        } else {
            //Set recipient
            let recipient = {
                if let Some(mint_to) = mint_to_addr {
                    deps.api.addr_validate(&mint_to)?
                } else {
                    info.clone().sender
                }
            };
            message = credit_mint_msg(
                config.clone(),
                Asset {
                    amount,
                    ..basket.clone().credit_asset
                },
                recipient,
            )?;

            //Add credit amount to the position
            //Update Position
            POSITIONS.update(deps.storage, info.clone().sender, |positions: Option<Vec<Position>>| -> Result<Vec<Position>, ContractError> {
                let mut updating_positions = positions.unwrap_or_else(|| vec![]);
                updating_positions[position_index] = target_position.clone();

                Ok(updating_positions)
            })?;

            //Add new debt to Basket
            basket.credit_asset.amount += amount;
            
            //Save updated repayment price and debts
            BASKET.save(deps.storage, &basket)?;
        }
    } else {
        return Err(ContractError::NoRepaymentPrice {});
    }

    //Check state changes
    check_debt_increase_state(
        deps.storage, 
        amount, 
        prev_credit_amount,
        prev_basket_credit,
        position_id, 
        info.sender,
    )?;

    let response = Response::new()
        .add_message(message)
        .add_attribute("method", "increase_debt")
        .add_attribute("position_id", position_id.to_string())
        .add_attribute("total_loan", target_position.credit_amount.to_string())
        .add_attribute("increased_by", amount.to_string());

    Ok(response)
}

/// Asserts valid state after increase_debt()
fn check_debt_increase_state(
    storage: &mut dyn Storage,
    increase_amount: Uint128,
    prev_credit_amount: Uint128,
    prev_basket_credit: Uint128,
    position_id: Uint128,
    position_owner: Addr,  
) -> Result<(), ContractError>{
    
    //Get target_position & Basket
    let (_i, target_position) = get_target_position(storage, position_owner, position_id)?;
    let basket = BASKET.load(storage)?;

    //Assert that credit_amount is equal to the origin + what was added
    if target_position.credit_amount != prev_credit_amount + increase_amount {
        return Err(ContractError::CustomError { val: String::from("Conditional 1: increase_debt() state error found, saved credit_amount != desired.") })
    }
    //Assert that credit_amount is equal to the origin + what was added
    if basket.credit_asset.amount != prev_basket_credit + increase_amount {
        return Err(ContractError::CustomError { val: String::from("Conditional 2: increase_debt() state error found, saved credit_amount != desired.") })
    }


    Ok(())
}

/// Edit and Enable debt token Redemption for any address-owned Positions
pub fn edit_redemption_info(
    deps: DepsMut, 
    info: MessageInfo,
    // Position IDs to edit
    mut position_ids: Vec<Uint128>,
    // Add or remove redeemability
    redeemable: Option<bool>,
    // Edit premium on the redeemed collateral.
    // Can't set a 100% premium, as that would be a free loan repayment.
    updated_premium: Option<u128>,
    // Edit Max loan repayment %
    max_loan_repayment: Option<Decimal>,    
    // Restricted collateral assets.
    // These aren't used for redemptions.
    restricted_collateral_assets: Option<Vec<String>>,
) -> Result<Response, ContractError>{
    //Check for valid premium
    if let Some(premium) = updated_premium {
        if premium > 99u128 {
            return Err(ContractError::CustomError { val: String::from("Premium can't be greater than 99") })
        }
    }

    //Check for valid max_loan_repayment
    if let Some(max_loan_repayment) = max_loan_repayment {
        if max_loan_repayment > Decimal::one() {
            return Err(ContractError::CustomError { val: String::from("Max loan repayment can't be greater than 100%") })
        }
    }

    //Position IDs must be specified & unique
    if position_ids.is_empty() {
        return Err(ContractError::CustomError { val: String::from("Position IDs must be specified") })
    } else {
        for id in position_ids.clone() {
            if position_ids.iter().filter(|&n| *n == id).count() > 1 {
                return Err(ContractError::CustomError { val: String::from("Position IDs must be unique") })
            }
        }
    }

    //////Additions//////
    //Add PositionRedemption objects under the user in the desired premium while skipping duplicates, if redeemable is true or None
    if (redeemable.is_some() && redeemable.unwrap_or_else(|| false)) || redeemable.is_none(){
        if let Some(updated_premium) = updated_premium {                
            //Load premium we are adding to 
            match REDEMPTION_OPT_IN.load(deps.storage, updated_premium){
                Ok(mut users_of_premium)=> {
                    //If the user already has a PositionRedemption, add the Position to the list
                    if let Some ((user_index, mut user_positions)) = users_of_premium.clone().into_iter().enumerate().find(|(_, user)| user.position_owner == info.sender){
                        //Iterate through the Position IDs
                        for id in position_ids.clone() {
                            //If the Position ID is not in the list, add it
                            if !user_positions.position_infos.iter().any(|position| position.position_id == id){

                                //Get target_position
                                let target_position = match get_target_position(deps.storage, info.sender.clone(), id){
                                    Ok((_, pos)) => pos,
                                    Err(_e) => return Err(ContractError::CustomError { val: String::from("User does not own this position id") })
                                };

                                user_positions.position_infos.push(PositionRedemption {
                                    position_id: id,
                                    remaining_loan_repayment: max_loan_repayment.unwrap_or(Decimal::one()) * target_position.credit_amount,
                                    restricted_collateral_assets: restricted_collateral_assets.clone().unwrap_or(vec![]),
                                });
                            }

                            //Remove the Position ID from the list, don't want to edit newly added RedemptionInfo
                            position_ids.retain(|&x| x != id);
                        }

                        //Update the PositionRedemption
                        users_of_premium[user_index] = user_positions;

                        //Save the updated list
                        REDEMPTION_OPT_IN.save(deps.storage, updated_premium, &users_of_premium)?;
                    } //Add user to the premium state
                    else {                            
                        //Create new RedemptionInfo
                        let new_redemption_info = create_redemption_info(
                            deps.storage,
                            position_ids.clone(), 
                            max_loan_repayment.clone(), 
                            info.clone().sender,
                            restricted_collateral_assets.clone().unwrap_or(vec![]),
                        )?;

                        //Add the new RedemptionInfo to the list
                        users_of_premium.push(new_redemption_info);

                        //Save the updated list
                        REDEMPTION_OPT_IN.save(deps.storage, updated_premium, &users_of_premium)?;
                    }
                },
                //If no users, create a new list
                Err(_err) => {
                    //Create new RedemptionInfo
                    let new_redemption_info = create_redemption_info(
                        deps.storage,
                        position_ids.clone(), 
                        max_loan_repayment.clone(), 
                        info.clone().sender,
                        restricted_collateral_assets.clone().unwrap_or(vec![]),
                    )?;

                    //Save the new RedemptionInfo
                    REDEMPTION_OPT_IN.save(deps.storage, updated_premium, &vec![new_redemption_info])?;
                },
            };
        } else if (redeemable.is_some() && redeemable.unwrap_or_else(|| false)) && updated_premium.is_none(){
            return Err(ContractError::CustomError { val: String::from("Can't set redeemable to true without specifying a premium") })
        }
    } 

    //////Edits and Removals//////
    //Parse through premium range to look for the Position IDs
    for premium in 0..100u128 {
        //Load premium we are editing
        let mut users_of_premium: Vec<RedemptionInfo> = match REDEMPTION_OPT_IN.load(deps.storage, premium){
            Ok(list)=> list,
            Err(_err) => vec![], //If no users, return empty vec
        };

        //Query for Users in the premium as long as there are Position IDs left to find && there are users in the premium
        if !position_ids.is_empty() && !users_of_premium.is_empty(){      
            
            //Iterate through users to find the Positions
            if let Some ((user_index, mut user_positions)) = users_of_premium.clone().into_iter().enumerate().find(|(_, user)| user.position_owner == info.sender){
                for id in position_ids.clone() {
                    //If the Position ID is in the list, edit, update and remove from the list
                    if let Some((position_index, _)) = user_positions.clone().position_infos.clone().into_iter().enumerate().find(|(_, position)| position.position_id == id){

                        //Edit or Remove the Position from redeemability
                        if let Some(redeemable) = redeemable {
                            if !redeemable {
                                user_positions.position_infos.remove(position_index);

                                //If the user has no more positions, remove them from the premium
                                if user_positions.position_infos.is_empty() {
                                    users_of_premium.remove(user_index);
                                    
                                    //Save the updated list
                                    REDEMPTION_OPT_IN.save(deps.storage, premium, &users_of_premium)?;
                                    break;
                                }
                            }
                        }
                        
                        //Update maximum loan repayment
                        if let Some(max_loan_repayment) = max_loan_repayment {
                            //Get target_position
                            let target_position = match get_target_position(deps.storage, info.sender.clone(), id){
                                Ok((_, pos)) => pos,
                                Err(_e) => return Err(ContractError::CustomError { val: String::from("User does not own this position id") })
                            };

                            user_positions.position_infos[position_index].remaining_loan_repayment = max_loan_repayment * target_position.credit_amount;
                        }

                        //To switch premiums we remove it from the list, it should've been added to its new list beforehand
                        if let Some(updated_premium) = updated_premium {  
                            if updated_premium != premium {
                                user_positions.position_infos.remove(position_index);

                                //If the user has no more positions, remove them from the premium
                                if user_positions.position_infos.is_empty() {
                                    users_of_premium.remove(user_index);
                                    
                                    //Save the updated list
                                    REDEMPTION_OPT_IN.save(deps.storage, premium, &users_of_premium)?;
                                    break;
                                }
                            }   
                        }
                        
                        //Update restricted collateral assets
                        if let Some(restricted_assets) = restricted_collateral_assets.clone() {
                            //Map collateral assets to String
                            let basket = BASKET.load(deps.storage)?;
                            let collateral = basket.collateral_types.iter().map(|asset| asset.asset.info.to_string()).collect::<Vec<String>>();

                            //If all restricted assets are valid, swap objects
                            if restricted_assets.iter().all(|asset| collateral.contains(asset)) {
                                user_positions.position_infos[position_index].restricted_collateral_assets = restricted_assets.clone();
                            } else {
                                return Err(ContractError::CustomError { val: String::from("Invalid restricted asset, only the position's collateral assets are viable to restrict") })
                            }
                        }

                        //Update the Position
                        users_of_premium[user_index] = user_positions.clone();

                        //Save the updated list
                        REDEMPTION_OPT_IN.save(deps.storage, premium, &users_of_premium)?;

                        //Remove the Position ID from the list
                        position_ids = position_ids
                            .clone()
                            .into_iter()
                            .filter(|stored_id| stored_id != id)
                            .collect::<Vec<Uint128>>();
                    }
                }
            }
        }
    }


    Ok(Response::new().add_attributes(vec![
        attr("method", "edit_redemption_info"),
        attr("positions_not_edited", format!("{:?}", position_ids))
    ]))
}

fn create_redemption_info(
    storage: &dyn Storage,
    position_ids: Vec<Uint128>,
    max_loan_repayment: Option<Decimal>,
    position_owner: Addr,
    restricted_collateral_assets: Vec<String>,
) -> StdResult<RedemptionInfo>{
    //Create list of PositionRedemptions
    let mut position_infos = vec![];
    
    for id in position_ids.clone(){
        //Get target_position
        let target_position = match get_target_position(storage, position_owner.clone(), id){
            Ok((_, pos)) => pos,
            Err(_e) => return Err(StdError::GenericErr { msg: String::from("User does not own this position id") })
        };

        //Add PositionRedemption to list
        position_infos.push(PositionRedemption {
            position_id: id,
            remaining_loan_repayment: max_loan_repayment.unwrap_or(Decimal::one()) * target_position.credit_amount,
            restricted_collateral_assets: restricted_collateral_assets.clone(),
        });
    }

    Ok(RedemptionInfo { 
        position_owner, 
        position_infos 
    })
}

/// Redeem the debt token for collateral for Positions that have opted in 
/// The premium is set by the Position owner, ex: 1% premium = vault is buying CDT at 99% of the peg price
pub fn redeem_for_collateral(    
    deps: DepsMut, 
    env: Env,
    info: MessageInfo,
    max_collateral_premium: u128,
) -> Result<Response, ContractError>{
    //Load State
    let config: Config = CONFIG.load(deps.storage)?;
    let basket: Basket = BASKET.load(deps.storage)?;

    let mut credit_amount;
    let mut redeemable_credit = Decimal::zero();
    let mut collateral_sends: Vec<Asset> = vec![];
    
    //Validate asset 
    if info.clone().funds.len() != 1 || info.clone().funds[0].denom != basket.credit_asset.info.to_string(){
        return Err(ContractError::CustomError { val: String::from("Must send only the Basket's debt token") })
    } else {
        credit_amount = Decimal::from_ratio(Uint128::from(info.clone().funds[0].amount), Uint128::one());
    }
    //Set initial credit amount
    let initial_credit_amount = credit_amount.clone();

    //Set premium range
    for premium in 0..=max_collateral_premium {
        //Calc discount ratio
        //(100%-premium)
        let discount_ratio = decimal_subtraction(
            Decimal::one(), 
            Decimal::percent(premium as u64)
        )?;

        //Loop until all credit is redeemed
        if !credit_amount.is_zero(){
            
            //Query for Users in the premium 
            let mut users_of_premium: Vec<RedemptionInfo> = match REDEMPTION_OPT_IN.load(deps.storage, premium){
                Ok(list)=> list,
                Err(_err) => vec![], //If no users, return empty vec
            };

            //Parse thru Users
            for (user_index, mut user) in users_of_premium.clone().into_iter().enumerate() {
                //Parse thru Positions
                for (pos_rdmpt_index, position_redemption_info) in user.clone().position_infos.into_iter().enumerate() {
                    //Query for user Positions in the premium
                    let (_i, mut target_position) = get_target_position(
                        deps.storage, 
                        user.clone().position_owner, 
                        position_redemption_info.position_id
                    )?;                    

                    //Remove restricted collateral assets from target_position.collateral_assets
                    for restricted_asset in position_redemption_info.restricted_collateral_assets {
                        target_position.collateral_assets = target_position.collateral_assets.clone()
                            .into_iter()
                            .filter(|asset| asset.asset.info.to_string() != restricted_asset)
                            .collect::<Vec<cAsset>>();
                    }
                    if target_position.collateral_assets.is_empty() {
                        continue;
                    }

                    //Get cAsset ratios
                    let (cAsset_ratios, cAsset_prices) = get_cAsset_ratios(
                        deps.storage,
                        env.clone(),
                        deps.querier,
                        target_position.clone().collateral_assets,
                        config.clone(),
                        Some(basket.clone()),
                    )?;

                    //Calc amount of credit that can be redeemed
                    redeemable_credit = Decimal::min(
                        Decimal::from_ratio(position_redemption_info.remaining_loan_repayment, Uint128::one()),
                        credit_amount
                    );
                    //Subtract redeemable from credit_amount 
                    credit_amount = decimal_subtraction(credit_amount, redeemable_credit)?;
                    //Subtract redeemable from remaining_loan_repayment
                    user.position_infos[pos_rdmpt_index].remaining_loan_repayment = 
                        position_redemption_info.remaining_loan_repayment - 
                        redeemable_credit.to_uint_floor();

                    /////Set and Save user info with updated remaining_loan_repayment////
                    //If remaining_loan_repayment is zero, remove PositionRedemption from user
                    if user.position_infos[pos_rdmpt_index].remaining_loan_repayment.is_zero() {
                        //Remove PositionRedemption from user
                        user.position_infos.remove(pos_rdmpt_index);
                        //Remove user if no more PositionRedemptions
                        if user.position_infos.is_empty() {
                            users_of_premium.remove(user_index);
                        }
                    }
                    //Update user
                    if !users_of_premium.is_empty() {
                        users_of_premium[user_index] = user.clone();
                    }
                    
                    REDEMPTION_OPT_IN.save(deps.storage, premium, &users_of_premium)?;

                    // Calc credit_value
                    //redeemable_credit * credit_price
                    let credit_value =  basket.clone().credit_price.get_value(redeemable_credit.to_uint_floor())?;
                    // Calc redeemable value
                    //credit_value * discount_ratio 
                    let redeemable_value = decimal_multiplication(
                        credit_value, 
                        discount_ratio
                    )?;

                    //Calc collateral to send for each cAsset
                    for (i, cAsset) in target_position.collateral_assets.iter().enumerate() {
                        //Calc collateral to send
                        let value_to_send = decimal_multiplication(
                            redeemable_value, 
                            cAsset_ratios[i]
                        )?;
                        let collateral_to_send = cAsset_prices[i].get_amount(value_to_send)?;

                        //Add to send list
                        if let Some(asset) = collateral_sends.iter_mut().find(|a| a.info == cAsset.asset.info) {
                            asset.amount += collateral_to_send.clone();
                        } else {
                            collateral_sends.push(Asset {
                                info: cAsset.asset.info.clone(),
                                amount: collateral_to_send.clone(),
                            });
                        }
                        
                        //Update Position totals
                        update_position_claims(
                            deps.storage, 
                            deps.querier, 
                            env.clone(), 
                            config.clone(),
                            position_redemption_info.position_id, 
                            user.clone().position_owner, 
                            cAsset.asset.info.clone(), 
                            collateral_to_send
                        )?;
                    }

                    //Reload target_position
                    let (_i, mut target_position) = get_target_position(
                        deps.storage, 
                        user.clone().position_owner, 
                        position_redemption_info.position_id
                    )?;

                    //Set position.credit_amount
                    target_position.credit_amount -= redeemable_credit.to_uint_floor();

                    //Update position.credit_amount
                    update_position(
                        deps.storage, 
                        user.clone().position_owner, 
                        target_position.clone()
                    )?;
                }
            }
        }
    }

    if credit_amount == initial_credit_amount {
        return Err(ContractError::CustomError { val: String::from("No collateral to redeem with at this max premium") })
    }

    //Convert collateral_sends to coins
    let mut coins: Vec<Coin> = vec![];
    for asset in collateral_sends {
        coins.push(asset_to_coin(asset)?)
    }

    let mut messages: Vec<CosmosMsg> = vec![];
    //Send collateral to sender
    let collateral_msg = BankMsg::Send {
        to_address: info.clone().sender.to_string(),
        amount: coins.clone(),
    };
    messages.push(collateral_msg.into());

    //Burn redeemed credit
    if let Some(addr) = config.osmosis_proxy {
        if !redeemable_credit.to_uint_floor().is_zero() {    
            //Create burn msg
            let burn_message = CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: addr.to_string(),
                msg: to_binary(&OsmoExecuteMsg::BurnTokens {
                    denom: basket.credit_asset.info.to_string(),
                    amount: redeemable_credit.to_uint_floor(),
                    burn_from_address: env.contract.address.to_string(),
                })?,
                funds: vec![],
            });
            messages.push(burn_message);
        }
    }

    //If there is excess credit, send it back to sender
    if !credit_amount.is_zero() {
        let credit_msg = BankMsg::Send {
            to_address: info.clone().sender.to_string(),
            amount: vec![Coin {
                denom: basket.credit_asset.info.to_string(),
                amount: credit_amount.to_uint_floor(),
            }],
        };
        messages.push(credit_msg.into());

        return Ok(Response::new()
            .add_messages(messages)
            .add_attributes(vec![
                attr("action", "redeem_for_collateral"),
                attr("sender", info.clone().sender),
                attr("redeemed_collateral", format!("{:?}", coins)),
                attr("excess_credit", format!("{:?}", credit_amount)),
            ])
        )
    }

    //Response
    Ok(Response::new()
        .add_messages(messages)
        .add_attributes(vec![
        attr("action", "redeem_for_collateral"),
        attr("sender", info.clone().sender),
        attr("redeemed_collateral", format!("{:?}", coins)),
        ])
    )
}

/// Create the contract's Basket.
/// Validates params.
 pub fn create_basket(
    deps: DepsMut,
    info: MessageInfo,
    env: Env,
    basket_id: Uint128,
    collateral_types: Vec<cAsset>,
    credit_asset: Asset,
    credit_price: Decimal,
    base_interest_rate: Option<Decimal>,
    credit_pool_infos: Vec<PoolType>,
    liq_queue: Option<String>,
) -> Result<Response, ContractError> {
    let config: Config = CONFIG.load(deps.storage)?;

    //Only contract owner can create new baskets. This will likely be governance.
    if info.sender != config.owner {
        return Err(ContractError::NotContractOwner {});
    }
    //One basket per contract
    if let Ok(_basket) = BASKET.load(deps.storage){
        return Err(ContractError::CustomError { val: String::from("Only one basket per contract") })
    }

    let mut new_assets = collateral_types.clone();
    let mut collateral_supply_caps = vec![];
    let mut msgs: Vec<CosmosMsg> = vec![];

    let mut new_liq_queue: Option<Addr> = None;
    if liq_queue.is_some() {
        new_liq_queue = Some(deps.api.addr_validate(&liq_queue.clone().unwrap_or_else(|| String::from("")))?);
    }

    //Minimum viable cAsset parameters
    for (i, asset) in collateral_types.iter().enumerate() {
        new_assets[i].asset.amount = Uint128::zero();
        new_assets[i].rate_index = Decimal::one();

        if asset.max_borrow_LTV >= asset.max_LTV
            && asset.max_borrow_LTV
                >= Decimal::from_ratio(Uint128::new(100u128), Uint128::new(1u128))
        {
            return Err(ContractError::CustomError {
                val: String::from("Max borrow LTV can't be greater or equal to max_LTV nor equal to 100"),
            });
        }

        //No LPs initially. Their pool asset's need to already be added as collateral so they can't come first.
        if asset.pool_info.is_some() {
            return Err(ContractError::CustomError {
                val: String::from("Can't add an LP when creating a basket"),
            });
        } else {
            //Asserting the Collateral Asset has an oracle
            if config.clone().oracle_contract.is_some() {
                //Query Asset Oracle
                deps.querier
                    .query::<Vec<AssetResponse>>(&QueryRequest::Wasm(WasmQuery::Smart {
                        contract_addr: config.clone().oracle_contract.unwrap_or_else(|| Addr::unchecked("")).to_string(),
                        msg: to_binary(&OracleQueryMsg::Assets {
                            asset_infos: vec![asset.clone().asset.info],
                        })?,
                    }))?;

                //If it errors it means the oracle doesn't exist
            } else {
                return Err(ContractError::CustomError {
                    val: String::from("Need to setup oracle contract before adding assets"),
                });
            }

            //Create Liquidation Queue for basket assets
            if new_liq_queue.clone().is_some() {
                //Gets Liquidation Queue max premium.
                //The premium has to be at most 5% less than the difference between max_LTV and 100%
                //The ideal variable for the 5% is the avg caller_liq_fee during high traffic periods
                let max_premium = match Uint128::new(95u128).checked_sub( asset.max_LTV * Uint128::new(100u128) ){
                    Ok( diff ) => diff,
                    //A default to 10 assuming that will be the highest sp_liq_fee
                    Err( _err ) => Uint128::new(10u128),
                };
                //We rather the LQ liquidate than the SP if possible so its max_premium will be at most the sp_liq fee...
                //..if the first subtraction fails.
                //If it failed, allowing the LQ premium to be more than the SP fee means less efficient liquidations..
                //Since we are aiming for lowest possible fee

                msgs.push(CosmosMsg::Wasm(WasmMsg::Execute {
                    contract_addr: new_liq_queue.clone().unwrap_or_else(|| Addr::unchecked("")).to_string(),
                    msg: to_binary(&LQ_ExecuteMsg::AddQueue {
                        bid_for: asset.clone().asset.info,
                        max_premium,
                        //Bid total before bids go to the waiting queue. 
                        // The cumulative threshold of frequented slots should be larger than the largest single liquidation amount to prevent waiting bids from causing InsufficientBids errors.
                        bid_threshold: Uint256::from(1_000_000_000_000u128), //1 million
                    })?,
                    funds: vec![],
                }));
            }
        }

        let mut lp = false;
        if asset.pool_info.is_some() {
            lp = true;
        }
        //Push the cAsset's asset info
        collateral_supply_caps.push(SupplyCap {
            asset_info: asset.clone().asset.info,
            current_supply: Uint128::zero(),
            supply_cap_ratio: Decimal::zero(),
            debt_total: Uint128::zero(),
            lp,
            stability_pool_ratio_for_debt_cap: None,
        });
    }

    //Set Basket fields
    let base_interest_rate = base_interest_rate.unwrap_or(Decimal::zero());

    let new_basket: Basket = Basket {
        basket_id,
        current_position_id: Uint128::from(1u128),
        collateral_types: new_assets,
        collateral_supply_caps,
        lastest_collateral_rates: vec![], //This will be set in the accrue function
        multi_asset_supply_caps: vec![],
        credit_asset: credit_asset.clone(),
        credit_price: PriceResponse {
            price: credit_price,
            prices: vec![],
            decimals: 6,
        },
        base_interest_rate,
        pending_revenue: Uint128::zero(),
        credit_last_accrued: env.block.time.seconds(),
        rates_last_accrued: env.block.time.seconds(),
        liq_queue: new_liq_queue,
        negative_rates: true,
        cpc_margin_of_error: Decimal::one(),
        oracle_set: false,
        frozen: false,
        rev_to_stakers: true,
    };

    //Denom check
    if let AssetInfo::Token { address :_} = credit_asset.info {
        return Err(ContractError::CustomError {
            val: String::from("Basket credit must be a native token denom"),
        });
    }

    //Add asset to liquidity check contract
    //Liquidity AddAsset Msg
    if let Some(liquidity_contract) = config.liquidity_contract {
        msgs.push(CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr: liquidity_contract.to_string(),
            msg: to_binary(&LiquidityExecuteMsg::AddAsset {
                asset: LiquidityInfo {
                    asset: new_basket.clone().credit_asset.info,
                    pool_infos: credit_pool_infos,
                },
            })?,
            funds: vec![],
        }));
    }

    //Save Basket
    BASKET.save( deps.storage, &new_basket )?;

    //Response Building
    let response = Response::new();

    Ok(response
        .add_attributes(vec![
            attr("method", "create_basket"),
            attr("basket_id", basket_id),
            attr("credit_asset", credit_asset.to_string()),
            attr("credit_price", credit_price.to_string()),
            attr(
                "liq_queue",
                liq_queue.unwrap_or_else(|| String::from("None")),
            ),
        ])
        .add_messages(msgs))
} 

/// Edit the contract's Basket.
/// Can't edit basket id, current_position_id or credit_asset.
/// Credit price can only be changed thru the accrue function.
/// Validates parameters and updates the basket.
pub fn edit_basket(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    editable_parameters: EditBasket,
) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;

    if info.sender != config.owner {
        return Err(ContractError::Unauthorized { owner: config.owner.to_string() });
    }

    let mut new_queue: Option<Addr> = None;
    if let Some(liq_queue) = editable_parameters.clone().liq_queue {
        new_queue = Some(deps.api.addr_validate(&liq_queue)?);
    }

    //Blank cAsset
    //This never gets added unless its edited. Here due to uninitialized errors.
    let mut new_cAsset = cAsset {
        asset: Asset {
            info: AssetInfo::NativeToken {
                denom: String::from("None"),
            },
            amount: Uint128::zero(),
        },
        max_borrow_LTV: Decimal::zero(),
        max_LTV: Decimal::zero(),
        pool_info: None,
        rate_index: Decimal::one(),
    };

    let mut msgs: Vec<CosmosMsg> = vec![];    
    let mut attrs = vec![attr("method", "edit_basket")];

    let mut basket = BASKET.load(deps.storage)?;
    //cAsset check
    if let Some(added_cAsset) = editable_parameters.clone().added_cAsset {
        let mut check = true;
        new_cAsset = added_cAsset.clone();

        //new_cAsset can't be the basket credit_asset or TEMA 
        if let Some(staking_contract) = config.clone().staking_contract {
            let tema_denom = deps.querier.query::<Staking_Config>(&QueryRequest::Wasm(WasmQuery::Smart { 
                contract_addr: staking_contract.to_string(), 
                msg: to_binary(&Staking_QueryMsg::Config { })? 
            }))?
            .tema_denom;

            if new_cAsset.asset.info.to_string() == tema_denom {
                return Err(ContractError::InvalidCollateral {  } )
            }
        }
        if new_cAsset.asset.info == basket.clone().credit_asset.info {
            return Err(ContractError::InvalidCollateral {  } )
        }
        ////
        
        //Each cAsset has to initialize amount as 0..
        new_cAsset.asset.amount = Uint128::zero();
        
        //..and index at 1
        new_cAsset.rate_index = Decimal::one();

        //No duplicates
        if let Some(_duplicate) = basket
            .clone()
            .collateral_types
            .into_iter()
            .find(|cAsset| cAsset.asset.info.equal(&new_cAsset.asset.info))
        {
            return Err(ContractError::CustomError {
                val: String::from("Attempting to add duplicate asset"),
            });
        }

        if let Some(mut pool_info) = added_cAsset.pool_info {

            //Query share asset amount
            let pool_state = match deps.querier.query::<PoolStateResponse>(&QueryRequest::Wasm(
                WasmQuery::Smart {
                    contract_addr: config.clone().osmosis_proxy.unwrap_or_else(|| Addr::unchecked("")).to_string(),
                    msg: match to_binary(&OsmoQueryMsg::PoolState {
                        id: pool_info.pool_id,
                    }) {
                        Ok(binary) => binary,
                        Err(err) => {
                            return Err(ContractError::CustomError {
                                val: err.to_string(),
                            })
                        }
                    },
                },
            )) {
                Ok(resp) => resp,
                Err(err) => {
                    return Err(ContractError::CustomError {
                        val: err.to_string(),
                    })
                }
            };
            let pool_assets = pool_state.assets;

            //Set correct shares denom
            new_cAsset.asset.info = AssetInfo::NativeToken {
                denom: pool_state.shares.denom,
            };

            //Set pool_assets in PoolInfo object
            //Assert pool_assets are already in the basket, which confirms an oracle and adequate parameters for them
            for (i, asset) in pool_assets.iter().enumerate() {

                //Set pool assets 
                pool_info.asset_infos[i].info = AssetInfo::NativeToken { denom: asset.clone().denom };               
               
                //Asserting that its pool assets are already added as collateral types
                if !basket.clone().collateral_types.into_iter().any(|cAsset| {
                    cAsset.asset.info.equal(&AssetInfo::NativeToken {
                        denom: asset.clone().denom,
                    })
                }){
                    return Err(ContractError::CustomError {
                        val: String::from("Need to add all pool assets before adding the LP"),
                    });
                }
            }

            //Update pool_info
            new_cAsset.pool_info = Some(pool_info.clone());

            //Add share_token to the oracle
            msgs.push(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: config.clone().oracle_contract.unwrap_or_else(|| Addr::unchecked("")).to_string(),
                msg: to_binary(&OracleExecuteMsg::AddAsset { 
                    asset_info: new_cAsset.clone().asset.info,
                    oracle_info: AssetOracleInfo { 
                        basket_id: Uint128::one(), 
                        pools_for_osmo_twap: vec![],
                        is_usd_par: false,
                        lp_pool_info: Some(
                            PoolInfo { 
                                pool_id: pool_info.pool_id,
                                asset_infos: vec![
                                    LPAssetInfo { 
                                        info: AssetInfo::NativeToken { denom: pool_assets[0].clone().denom  }, 
                                        decimals: 6, 
                                        ratio: Decimal::percent(50),
                                    },
                                    LPAssetInfo { 
                                        info: AssetInfo::NativeToken { denom: pool_assets[1].clone().denom  }, 
                                        decimals: 6, 
                                        ratio: Decimal::percent(50),
                                    },
                                ],
                            }
                        ),                       
                        decimals: 18,
                        pyth_price_feed_id: None,
                    },
                })?,
                funds: vec![],
            }));

        } else {
            //Asserting the Collateral Asset has an oracle
            if config.oracle_contract.is_some() {
                //Query Asset Oracle
                deps.querier
                    .query::<Vec<AssetResponse>>(&QueryRequest::Wasm(WasmQuery::Smart {
                        contract_addr: config.clone().oracle_contract.unwrap_or_else(|| Addr::unchecked("")).to_string(),
                        msg: to_binary(&OracleQueryMsg::Assets {
                            asset_infos: vec![new_cAsset.clone().asset.info],
                        })?,
                    }))?;

                //If it errors it means the oracle doesn't exist
            } else {
                return Err(ContractError::CustomError {
                    val: String::from("Need to setup oracle contract before adding assets"),
                });
            }
        }        

        //Create Liquidation Queue for the asset
        if basket.clone().liq_queue.is_some() {
            //Gets Liquidation Queue max premium.
            //The premium has to be at most 5% less than the difference between max_LTV and 100%
            //The ideal variable for the 5% is the avg caller_liq_fee during high traffic periods
            let max_premium = match Uint128::new(95u128).checked_sub( new_cAsset.max_LTV * Uint128::new(100u128) ){
                Ok( diff ) => diff,
                //A default to 10 assuming that will be the highest sp_liq_fee
                Err( _err ) => Uint128::new(10u128),
            };

            msgs.push(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: basket.clone().liq_queue.unwrap_or_else(|| Addr::unchecked("")).into_string(),
                msg: to_binary(&LQ_ExecuteMsg::AddQueue {
                    bid_for: new_cAsset.clone().asset.info,
                    max_premium,
                    //Bid total before bids go to the waiting queue. 
                    //Threshold should be larger than the largest single liquidation amount to prevent waiting bids from causing InsufficientBids errors.
                    bid_threshold: Uint256::from(1_000_000_000_000u128), //1 million
                })?,
                funds: vec![],
            }));
        } else if let Some(new_queue) = new_queue.clone() {
            //Gets Liquidation Queue max premium.
            //The premium has to be at most 5% less than the difference between max_LTV and 100%
            //The ideal variable for the 5% is the avg caller_liq_fee during high traffic periods
            let max_premium = match Uint128::new(95u128).checked_sub( new_cAsset.max_LTV * Uint128::new(100u128) ){
                Ok( diff ) => diff,
                //A default to 10 assuming that will be the highest sp_liq_fee
                Err( _err ) => Uint128::new(10u128) 
                ,
            };

            msgs.push(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: new_queue.into_string(),
                msg: to_binary(&LQ_ExecuteMsg::AddQueue {
                    bid_for: new_cAsset.clone().asset.info,
                    max_premium,
                    //Bid total before bids go to the waiting queue. 
                    //Threshold should be larger than the largest single liquidation amount to prevent waiting bids from causing InsufficientBids errors.
                    bid_threshold: Uint256::from(1_000_000_000_000u128), //1 million
                })?,
                funds: vec![],
            }));
        }

        //..needs minimum viable LTV parameters
        if new_cAsset.max_borrow_LTV >= new_cAsset.max_LTV
            || new_cAsset.max_borrow_LTV
                >= Decimal::from_ratio(Uint128::new(100u128), Uint128::new(1u128))
        {
            check = false;
        }

        if !check {
            return Err(ContractError::CustomError {
                val: "Max borrow LTV can't be greater or equal to max_LTV nor equal to 100"
                    .to_string(),
            });
        }

        let mut lp = false;
        if new_cAsset.pool_info.is_some() {
            lp = true;
        }
        //Push the cAsset's asset info
        basket.collateral_supply_caps.push(SupplyCap {
            asset_info: new_cAsset.clone().asset.info,
            current_supply: Uint128::zero(),
            supply_cap_ratio: Decimal::zero(),
            debt_total: Uint128::zero(),
            lp,
            stability_pool_ratio_for_debt_cap: None,
        });

        //Create Volatility Index for the asset
        VOLATILITY.save(deps.storage, new_cAsset.clone().asset.info.to_string(), &CollateralVolatility {
            index: Decimal::one(),
            volatility_list: vec![],
        })?;
    }
    
    //Save basket's new collateral_supply_caps
    BASKET.save(deps.storage, &basket)?;

    //Send credit_asset TWAP info to Oracle Contract
    let mut oracle_set = basket.oracle_set;

    if let Some(credit_twap) = editable_parameters.clone().credit_asset_twap_price_source {
        if let Some(oracle_contract) = config.clone().oracle_contract {
            //Set the credit Oracle. Using EditAsset updates or adds.
            msgs.push(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: oracle_contract.to_string(),
                msg: to_binary(&OracleExecuteMsg::EditAsset {
                    asset_info: basket.clone().credit_asset.info,
                    oracle_info: Some(AssetOracleInfo {
                        basket_id: basket.clone().basket_id,
                        pools_for_osmo_twap: vec![credit_twap],
                        is_usd_par: false,
                        lp_pool_info: None,
                        decimals: 6,
                        pyth_price_feed_id: None,
                    }),
                    remove: false,
                })?,
                funds: vec![],
            }));

            oracle_set = true;
        }
    };

    //Add pool_infos to the Liquidity contract
    if let Some(pool_infos) = editable_parameters.clone().credit_pool_infos {
        attrs.push(attr("new_pool_infos", format!("{:?}", pool_infos)));

        if let Some(liquidity_contract) = config.liquidity_contract {
            msgs.push(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: liquidity_contract.to_string(),
                msg: to_binary(&LiquidityExecuteMsg::EditAsset {
                    asset: LiquidityInfo {
                        asset: basket.clone().credit_asset.info,
                        pool_infos,
                    },
                })?,
                funds: vec![],
            }));
        }
    }

    //If updating frozen, set timer
    if let Some(frozen) = editable_parameters.clone().frozen {
        let mut timer = match FREEZE_TIMER.load(deps.storage){
            Ok(timer) => timer,
            Err(_err) => Timer {
                start_time: 0,
                end_time: 0,
            },
        };

        if frozen && !basket.frozen{
            //If we are freezing, set start timer
            timer.start_time = env.block.time.seconds();
            
            //Save timer
            FREEZE_TIMER.save(deps.storage, &timer)?;
        } else  if !frozen && basket.frozen {
            //If we are unfreezing, set end timer
            timer.end_time = env.block.time.seconds();

            //Save timer
            FREEZE_TIMER.save(deps.storage, &timer)?;
        }
    }
    //Reset the Volatility Index for any edited supply caps
    if let Some(caps) = editable_parameters.clone().collateral_supply_caps {
        for cap in caps {
            VOLATILITY.update(deps.storage, cap.asset_info.to_string(), |mut vol| -> StdResult<CollateralVolatility> {
                match vol {
                    Some(mut vol) => {
                        vol.index = Decimal::one();
                        Ok(vol)
                    },
                    None => {
                        let mut vol = CollateralVolatility {
                            index: Decimal::one(),
                            volatility_list: vec![],
                        };
                        Ok(vol)
                    }
                }
            })?;
        }
    }
    if let Some(caps) = editable_parameters.clone().multi_asset_supply_caps {
        for cap in caps {
            for asset in cap.assets {
                VOLATILITY.update(deps.storage, asset.to_string(), |mut vol| -> StdResult<CollateralVolatility> {
                    match vol {
                        Some(mut vol) => {
                            vol.index = Decimal::one();
                            Ok(vol)
                        },
                        None => {
                            let mut vol = CollateralVolatility {
                                index: Decimal::one(),
                                volatility_list: vec![],
                            };
                            Ok(vol)
                        }
                    }
                })?;
            }
        }
    }

    //Update Basket
    BASKET.update(deps.storage, |mut basket| -> Result<Basket, ContractError> {
        //Set all optional parameters
        editable_parameters.edit_basket(&mut basket, new_cAsset, new_queue, oracle_set)?;        

        Ok(basket)
    })?;
    attrs.push(attr("updated_basket", format!("{:?}", basket.clone())));

    //Return Response
    Ok(Response::new().add_attributes(attrs).add_messages(msgs))
}


/// Calculate desired amount of credit to borrow to reach target LTV
pub fn get_amount_from_LTV(
    storage: &dyn Storage,
    querier: QuerierWrapper,
    env: Env,
    config: Config,
    position: Position,
    basket: Basket,
    target_LTV: Decimal,
) -> Result<Uint128, ContractError>{
    //Get avg_borrow_LTV & total_value
    let (avg_borrow_LTV, _avg_max_LTV, total_value, _cAsset_prices, _cAsset_ratios) = get_avg_LTV(
        storage, 
        env, 
        querier, 
        config, 
        Some(basket.clone()),
        position.clone().collateral_assets,
        false,
    )?;

    //Target LTV can't be greater than possible borrowable LTV for the Position
    if target_LTV > avg_borrow_LTV {
        return Err(ContractError::InvalidLTV { target_LTV })
    }

    //Calc current LTV
    let current_LTV = {
        let credit_value = basket.clone().credit_price.get_value(position.credit_amount)?;

        decimal_division(credit_value, total_value)?
    };

    //If target_LTV is <= current_LTV there is no room to increase
    if target_LTV <= current_LTV {
        return Err(ContractError::InvalidLTV { target_LTV })
    }

    //Calculate amount of credit to get to target_LTV
    let credit_amount: Uint128 = {        
        //Calc spread between current LTV and target_LTV
        let LTV_spread = target_LTV - current_LTV;

        //Calc the value LTV_spread represents
        let increased_credit_value = decimal_multiplication(total_value, LTV_spread)?;
        
        //Calc the amount of credit needed to reach the increased_credit_value
        basket.credit_price.get_amount(increased_credit_value)?
    };

    Ok( credit_amount )
}


/// Checks if any Basket caps are set to 0.
/// If so the withdrawal assets have to either fully withdraw the asset from the position or only withdraw said asset.
/// Otherwise users could just fully withdrawal other assets and create a new position.
/// In a LUNA situation this would leave debt backed by an asset whose solvency Sistema has no faith in.
fn check_for_expunged(
    position_assets: Vec<cAsset>,
    withdrawal_assets: Vec<cAsset>,
    basket: Basket
)-> StdResult<()>{
    //Extract the Asset from the cAssets
    let position_assets: Vec<Asset> = position_assets
        .into_iter()
        .map(|cAsset| cAsset.asset)
        .collect::<Vec<Asset>>();

    let withdrawal_assets: Vec<Asset> = withdrawal_assets
        .into_iter()
        .map(|cAsset| cAsset.asset)
        .collect::<Vec<Asset>>();

    let mut passed = true;
    let mut invalid_withdraws = vec![];

    //For any supply cap at 0
    for cap in basket.collateral_supply_caps {

        if cap.supply_cap_ratio.is_zero(){

            //If in the position
            if let Some( asset ) = position_assets.clone().into_iter().find(|asset| asset.info.equal(&cap.asset_info)){

                //Withdraw asset has to either..
                //1) Only withdraw the asset
                if withdrawal_assets[0].info.equal(&asset.info) && withdrawal_assets.len() == 1_usize{
                    passed = true;
                
                //2) Fully withdraw the asset
                } else if let Some( withdrawal_asset ) = withdrawal_assets.clone().into_iter().find(|w_asset| w_asset.info.equal(&asset.info)){

                    if withdrawal_asset.amount == asset.amount {
                        passed = true;
                    }else {
                        passed = false;
                        invalid_withdraws.push( asset.info.to_string() );
                    } 
                } else {
                    passed = false;
                    invalid_withdraws.push( asset.info.to_string() );
                }
            }
        }
    }
    if !passed {
        return Err( StdError::GenericErr { msg: format!("These assets need to be expunged from the positon: {:?}", invalid_withdraws) } )
    }

    Ok(())
}

/// Create Position instance
pub fn create_position(
    cAssets: Vec<cAsset>, //Assets being added into the position
    basket: &mut Basket,
) -> Result<Position, ContractError> {   
    let new_position = Position {
        position_id: basket.current_position_id,
        collateral_assets: cAssets,
        credit_amount: Uint128::zero(),
    };

    //increment position id
    basket.current_position_id += Uint128::from(1u128);

    Ok(new_position)
}

/// Creates a CosmosMsg to mint tokens
pub fn credit_mint_msg(
    config: Config,
    credit_asset: Asset,
    recipient: Addr,
) -> StdResult<CosmosMsg> {
    match credit_asset.clone().info {
        AssetInfo::Token { address: _ } => {
            Err(StdError::GenericErr {
                msg: String::from("Credit has to be a native token"),
            })
        }
        AssetInfo::NativeToken { denom } => {
            if config.osmosis_proxy.is_some() {
                let message = CosmosMsg::Wasm(WasmMsg::Execute {
                    contract_addr: config.osmosis_proxy.unwrap_or_else(|| Addr::unchecked("")).to_string(),
                    msg: to_binary(&OsmoExecuteMsg::MintTokens {
                        denom,
                        amount: credit_asset.amount,
                        mint_to_address: recipient.to_string(),
                    })?,
                    funds: vec![],
                });
                Ok(message)
            } else {
                Err(StdError::GenericErr {
                    msg: String::from("No proxy contract setup"),
                })
            }
        }
    }
}

/// Creates a CosmosMsg to distribute debt tokens
pub fn credit_burn_rev_msg(
    config: Config, 
    env: Env, 
    credit_asset: Asset,
    basket: &mut Basket,
) -> StdResult<Vec<CosmosMsg>> {

    //Calculate the amount to burn
    let (burn_amount, revenue_amount) = {
        //If not sent to stakers, burn all
        if !basket.rev_to_stakers {
            (credit_asset.amount, Uint128::zero())

            //if pending rev is != 0
        } else if !basket.pending_revenue.is_zero() {
            //If pending_revenue && repay amount are more than 50 CDT, send all to stakers
            //Limits Repay gas costs for smaller users & frequent management costs for larger
            if basket.pending_revenue >= Uint128::new(50_000_000) && credit_asset.amount >= Uint128::new(50_000_000){
                if basket.pending_revenue >= credit_asset.amount {
                    //if pending rev is greater send the full repayment
                    (Uint128::zero(), credit_asset.amount)
                } else {
                    //if pending rev is less send the full pending rev
                    //Burn the remainder
                    (credit_asset.amount - basket.pending_revenue, basket.pending_revenue)
                }
            } else {
                (credit_asset.amount, Uint128::zero())
            }

        } else {
            (credit_asset.amount, Uint128::zero())
        }        
    };
    //Update pending_revenue
    basket.pending_revenue -= revenue_amount;

    //Initialize messages
    let mut messages = vec![];
    
    if let AssetInfo::NativeToken { denom } = credit_asset.clone().info {
        if let Some(addr) = config.osmosis_proxy {
            if !burn_amount.is_zero() {    
                //Create burn msg
                let burn_message = CosmosMsg::Wasm(WasmMsg::Execute {
                    contract_addr: addr.to_string(),
                    msg: to_binary(&OsmoExecuteMsg::BurnTokens {
                        denom,
                        amount: burn_amount,
                        burn_from_address: env.contract.address.to_string(),
                    })?,
                    funds: vec![],
                });
                messages.push(burn_message);
            }

            //Create DepositFee Msg
            if !revenue_amount.is_zero() && config.staking_contract.is_some(){
                let rev_message = CosmosMsg::Wasm(WasmMsg::Execute {
                    contract_addr: config.staking_contract.unwrap_or_else(|| Addr::unchecked("")).to_string(),
                    msg: to_binary(&Staking_ExecuteMsg::DepositFee { })?,
                    funds: vec![ asset_to_coin(Asset {
                        amount: revenue_amount,
                        ..credit_asset
                    })? ],
                });
                messages.push(rev_message);
            }

            Ok(messages)
        } else {
            Err(StdError::GenericErr { msg: String::from("No proxy contract setup")})
        }
    } else { Err(StdError::GenericErr { msg: String::from("Cw20 assets aren't allowed") }) }
}

/// Checks if any cAsset amount is zero or if asset list is empty
pub fn check_for_empty_position( collateral_assets: Vec<cAsset> )-> bool {
    
    if collateral_assets.len() == 0 {
        return true
    }

    //Checks if any cAsset amount is not zero
    for asset in collateral_assets {    
        if !asset.asset.amount.is_zero(){
            return false
        }
    }
    true 
}