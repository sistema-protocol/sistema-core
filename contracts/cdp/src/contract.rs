use std::env;

#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    attr, to_binary, Addr, Binary, CosmosMsg, Decimal, Deps, DepsMut, Env,
    MessageInfo, Reply, Response, StdError, StdResult, Uint128, WasmMsg,
};

use membrane::auction::ExecuteMsg as AuctionExecuteMsg;
use membrane::helpers::assert_sent_native_token_balance;
use membrane::liq_queue::ExecuteMsg as LQ_ExecuteMsg;
use membrane::cdp::{Config, CallbackMsg, ExecuteMsg, InstantiateMsg, QueryMsg, UpdateConfig, MigrateMsg};
use membrane::types::{
    cAsset, Asset, AssetInfo, Basket, UserInfo,
};
use membrane::osmosis_proxy::ExecuteMsg as OP_ExecuteMsg;

use crate::error::ContractError;
use crate::rates::external_accrue_call;
use crate::risk_engine::assert_basket_assets;
use crate::positions::{
    deposit,
    edit_basket, increase_debt,
    liq_repay, repay, redeem_for_collateral, edit_redemption_info,
    withdraw, BAD_DEBT_REPLY_ID, WITHDRAW_REPLY_ID,
    LIQ_QUEUE_REPLY_ID, USER_SP_REPAY_REPLY_ID, create_basket,
};
use crate::query::{
    query_basket_credit_interest, query_basket_debt_caps, query_basket_positions, query_basket_redeemability, query_collateral_rates, simulate_LTV_mint
};
use crate::liquidations::liquidate;
use crate::reply::{handle_liq_queue_reply, handle_withdraw_reply};
use crate::state::{ get_target_position, update_position, CollateralVolatility, ContractVersion, BASKET, CONFIG, CONTRACT, OWNERSHIP_TRANSFER, VOLATILITY };

// version info for migration info
const CONTRACT_NAME: &str = "crates.io:cdp";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    
    let mut config = Config {
        liq_fee: msg.liq_fee,
        owner: info.clone().sender,
        stability_pool: None,
        dex_router: None,
        staking_contract: None,
        oracle_contract: None,
        osmosis_proxy: None,
        debt_auction: None,
        liquidity_contract: None,
        discounts_contract: None,
        oracle_time_limit: msg.oracle_time_limit,
        cpc_multiplier: Decimal::one(), 
        rate_slope_multiplier: msg.rate_slope_multiplier,
        debt_minimum: msg.debt_minimum,
        base_debt_cap_multiplier: msg.base_debt_cap_multiplier,
        collateral_twap_timeframe: msg.collateral_twap_timeframe,
        credit_twap_timeframe: msg.credit_twap_timeframe,
    };

    //Set optional config parameters
    if let Some(address) = msg.owner {
        config.owner = deps.api.addr_validate(&address)?;
    };
    if let Some(address) = msg.stability_pool {
        config.stability_pool = Some(deps.api.addr_validate(&address)?)
    };
    if let Some(address) = msg.dex_router {
        config.dex_router = Some(deps.api.addr_validate(&address)?)
    };
    if let Some(address) = msg.staking_contract {
        config.staking_contract = Some(deps.api.addr_validate(&address)?)
    };
    if let Some(address) = msg.oracle_contract {
        config.oracle_contract = Some(deps.api.addr_validate(&address)?)
    };
    if let Some(address) = msg.osmosis_proxy {
        config.osmosis_proxy = Some(deps.api.addr_validate(&address)?)
    };
    if let Some(address) = msg.debt_auction {
        config.debt_auction = Some(deps.api.addr_validate(&address)?)
    };
    if let Some(address) = msg.liquidity_contract {
        config.liquidity_contract = Some(deps.api.addr_validate(&address)?);
    };
    
    CONFIG.save(deps.storage, &config)?;

    //Set contract version
    CONTRACT.save(deps.storage, &ContractVersion {
        contract: String::from(CONTRACT_NAME),
        version: String::from(CONTRACT_VERSION),
    })?;

    //Create basket
    create_basket(
        deps, 
        info, 
        env.clone(), 
        msg.create_basket.basket_id, 
        msg.create_basket.collateral_types, 
        msg.create_basket.credit_asset, 
        msg.create_basket.credit_price, 
        msg.create_basket.base_interest_rate, 
        msg.create_basket.credit_pool_infos, 
        msg.create_basket.liq_queue
    )?; 

    Ok(Response::new()
        .add_attribute("method", "instantiate")
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
        ExecuteMsg::UpdateConfig (update) => update_config(deps, info, update),
        ExecuteMsg::Deposit { position_owner, position_id} => {
            //Set valid_assets from funds sent
            let valid_assets = info
                .clone()
                .funds
                .into_iter()
                .map(|coin| Asset {
                    info: AssetInfo::NativeToken { denom: coin.denom },
                    amount: coin.amount,
                })
                .collect::<Vec<Asset>>();

            let cAssets: Vec<cAsset> = assert_basket_assets(
                deps.storage,
                deps.querier,
                env.clone(),
                valid_assets,
            )?;

            //If there is nothing being deposited, error
            if cAssets == vec![] { return Err(ContractError::CustomError { val: String::from("No deposit assets passed") }) }

            deposit(deps, env, info, position_owner, position_id, cAssets)
        }
        ExecuteMsg::Withdraw {
            position_id,
            assets,
            send_to,
        } => {
            duplicate_asset_check(assets.clone())?;
            let cAssets: Vec<cAsset> = assert_basket_assets(
                deps.storage,
                deps.querier,
                env.clone(),
                assets,
            )?;
            //If there is nothing being withdrawn, error
            if cAssets == vec![] { return Err(ContractError::CustomError { val: String::from("No withdrawal assets passed") }) }
            withdraw(deps, env, info, position_id, cAssets, send_to)
        }

        ExecuteMsg::IncreaseDebt {
            position_id,
            amount,
            mint_to_addr,
            LTV,
        } => increase_debt(deps, env, info, position_id, amount, LTV, mint_to_addr),
        ExecuteMsg::Repay {
            position_id,
            position_owner,
            send_excess_to,
        } => {
            let basket: Basket = BASKET.load(deps.storage)?;                        
            let credit_asset = assert_sent_native_token_balance(basket.credit_asset.info, &info)?;

            repay(
                deps.storage,
                deps.querier,
                deps.api,
                env,
                info,
                position_id,
                position_owner,
                credit_asset,
                send_excess_to,
            )
        },
        ExecuteMsg::Accrue { position_owner, position_ids } => { external_accrue_call(deps.storage, deps.api, deps.querier, info, env, position_owner, position_ids) },
        ExecuteMsg::RedeemCollateral { max_collateral_premium } => {
            redeem_for_collateral(
                deps, 
                env, 
                info, 
                max_collateral_premium.unwrap_or(99u128)
            )
        },
        ExecuteMsg::EditRedeemability { position_ids, redeemable, premium, max_loan_repayment, restricted_collateral_assets } => {
            edit_redemption_info(
                deps, 
                info, 
                position_ids, 
                redeemable, 
                premium, 
                max_loan_repayment,
                restricted_collateral_assets
            )
        },
        ExecuteMsg::LiqRepay {} => {
            if !info.funds.is_empty() {
                let credit_asset = Asset {
                    info: AssetInfo::NativeToken {
                        denom: info.funds[0].clone().denom,
                    },
                    amount: info.funds[0].amount,
                };
                liq_repay(deps, env, info, credit_asset)
            } else { //This is checked more specifically in repay(). This is solely to guarantee only one asset is checked.
                 Err(ContractError::InvalidCredit {})
            }
        }
        ExecuteMsg::EditcAsset {
            asset,
            max_borrow_LTV,
            max_LTV,
        } => edit_cAsset(deps, info, asset, max_borrow_LTV, max_LTV),
        ExecuteMsg::EditBasket(edit) => edit_basket(deps, env, info,edit),
        ExecuteMsg::Liquidate {
            position_id,
            position_owner,
        } => liquidate(
            deps.storage,
            deps.api,
            deps.querier,
            env,
            info,
            position_id,
            position_owner,
        ),
        ExecuteMsg::Callback(msg) => {
            if info.sender == env.contract.address {
                callback_handler(deps, env, msg)
            } else {
                Err(ContractError::Unauthorized { owner: env.contract.address.to_string() })
            }
        }
    }
}

/// Edit params for a cAsset in the basket
fn edit_cAsset(
    deps: DepsMut,
    info: MessageInfo,
    asset: AssetInfo,
    max_borrow_LTV: Option<Decimal>,
    max_LTV: Option<Decimal>,
) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;

    //Assert Authority
    if info.sender != config.owner {
        return Err(ContractError::Unauthorized { owner: config.owner.to_string() });
    }

    let mut basket: Basket = BASKET.load(deps.storage)?;
    let mut attrs = vec![
        attr("method", "edit_cAsset"),
    ];

    let new_asset: cAsset;
    let mut msgs: Vec<CosmosMsg> = vec![];

    match basket
        .clone()
        .collateral_types
        .into_iter()
        .find(|cAsset| cAsset.asset.info.equal(&asset))
    {
        Some(mut asset) => {
            attrs.push(attr("asset", asset.asset.info.to_string()));

            if let Some(LTV) = max_LTV {
                //Enforce 1-100% range
                if LTV > Decimal::percent(100) || LTV < Decimal::percent(1) {
                    return Err(ContractError::InvalidMaxLTV { max_LTV: LTV });
                }
                asset.max_LTV = LTV;

                //Edit the asset's liq_queue max_premium
                //Create Liquidation Queue for its assets
                if basket.clone().liq_queue.is_some() {
                    //Gets Liquidation Queue max premium.
                    //The premium has to be at most 5% less than the difference between max_LTV and 100%
                    //The ideal variable for the 5% is the avg caller_liq_fee during high traffic periods
                    let max_premium = match Uint128::new(95u128).checked_sub( LTV * Uint128::new(100u128) ){
                        Ok( diff ) => diff,
                        //A default to 10 assuming that will be the highest sp_liq_fee
                        Err( _err ) => Uint128::new(10u128),
                    };

                    msgs.push(CosmosMsg::Wasm(WasmMsg::Execute {
                        contract_addr: basket.clone().liq_queue.unwrap_or_else(|| Addr::unchecked("")).into_string(),
                        msg: to_binary(&LQ_ExecuteMsg::UpdateQueue {
                            bid_for: asset.clone().asset.info,
                            max_premium: Some(max_premium),
                            bid_threshold: None,
                        })?,
                        funds: vec![],
                    }));
                }

                attrs.push(attr("max_LTV", LTV.to_string()));
            }

            if let Some(LTV) = max_borrow_LTV {
                if LTV < Decimal::percent(100) && LTV < asset.max_LTV {
                    asset.max_borrow_LTV = LTV;
                    attrs.push(attr("max_borrow_LTV", LTV.to_string()));
                } else {
                    return Err(ContractError::CustomError {
                        val:String::from("Invalid borrow LTV"),
                    })
                }
            }
            new_asset = asset;
        }
        None => {
            return Err(ContractError::CustomError {
                val:String::from("Collateral type doesn't exist in basket"),
            })
        }
    };
    //Set and Save new basket
    basket.collateral_types = basket
        .clone()
        .collateral_types
        .into_iter()
        .filter(|asset| !asset.asset.info.equal(&new_asset.asset.info))
        .collect::<Vec<cAsset>>();

    basket.collateral_types.push(new_asset);

    BASKET.save(deps.storage, &basket)?;

    Ok(Response::new().add_attributes(attrs).add_messages(msgs))
}

/// Update contract config
fn update_config(
    deps: DepsMut,
    info: MessageInfo,
    update: UpdateConfig,
) -> Result<Response, ContractError> {
    let mut config = CONFIG.load(deps.storage)?;
    let mut attrs = vec![
        attr("method", "update_config"),
    ];

    //Assert Authority
    if info.sender != config.owner {
        //Check if ownership transfer is in progress & transfer if so
        let new_owner = OWNERSHIP_TRANSFER.load(deps.storage)?;
        if info.sender == new_owner {
            config.owner = info.sender;
        } else {
            return Err(ContractError::Unauthorized { owner: new_owner.to_string() });
        }
    }
    
    if let Some(owner) = update.clone().owner {
        let valid_addr = deps.api.addr_validate(&owner)?;

        //Set owner transfer state
        OWNERSHIP_TRANSFER.save(deps.storage, &valid_addr)?; 
        attrs.push(attr("owner_transfer", valid_addr));
    }
    
    //Update Config
    update.update_config(deps.api, &mut config)?;

    //Save new Config
    CONFIG.save(deps.storage, &config)?;
    
    attrs.push(
        attr("updated_config", format!("{:?}", config)));
    Ok(Response::new().add_attributes(attrs))
}

/// Handle CallbackMsgs
pub fn callback_handler(
    deps: DepsMut,
    env: Env,
    msg: CallbackMsg,
) -> Result<Response, ContractError> {
    match msg {
        CallbackMsg::BadDebtCheck {
            position_owner,
            position_id,
        } => check_and_fulfill_bad_debt(deps, env, position_id, position_owner),
    }
}

/// Check and recapitilize Bad Debt w/ revenue or TEMA auctions
fn check_and_fulfill_bad_debt(
    deps: DepsMut,
    _env: Env,
    position_id: Uint128,
    position_owner: Addr,
) -> Result<Response, ContractError> {
    let config: Config = CONFIG.load(deps.storage)?;

    let mut basket: Basket = BASKET.load(deps.storage)?;

    //Get target Position
    let (_i, mut target_position) = get_target_position(deps.storage, position_owner.clone(), position_id)?;

    //We do a lazy check for bad debt by checking if there is debt without any assets left in the position
    //This is allowed bc any calls here will be after a liquidation where the SP would've sold all it could to cover debts
    let total_assets: Uint128 = target_position
        .collateral_assets
        .iter()
        .map(|asset| asset.asset.amount)
        .collect::<Vec<Uint128>>()
        .iter()
        .sum();

    if total_assets > Uint128::zero() || target_position.credit_amount.is_zero() {
        Err(ContractError::PositionSolvent {})
    } else {
        let mut messages: Vec<CosmosMsg> = vec![];
        let mut bad_debt_amount = target_position.credit_amount;
        let mut attrs = vec![
            attr("method", "check_and_fulfill_bad_debt"),
            attr("bad_debt_amount", bad_debt_amount),
        ];

        //If the basket has revenue, mint and repay the bad debt
        if !basket.pending_revenue.is_zero() {
            if bad_debt_amount >= basket.pending_revenue {

                //Update bad_debt
                bad_debt_amount -= basket.pending_revenue;

                //Update basket revenue
                basket.pending_revenue = Uint128::zero();
            } else {                
                //Update basket revenue
                basket.pending_revenue -= bad_debt_amount;

                //Set bad_debt to 0
                bad_debt_amount = Uint128::zero();

            }
        }

        //Set target_position.credit_amount to the leftover bad debt
        target_position.credit_amount = bad_debt_amount;
        
        //Save target_position w/ updated debt
        update_position(deps.storage, position_owner.clone(), target_position)?;

        //Send bad debt amount to the auction contract if greater than 0
        if config.debt_auction.is_some() && !bad_debt_amount.is_zero() {
            let auction_msg = AuctionExecuteMsg::StartAuction {
                repayment_position_info: Some(UserInfo {
                    position_id,
                    position_owner: position_owner.to_string(),
                }),
                auction_asset: Asset {
                    amount: bad_debt_amount,
                    info: basket.clone().credit_asset.info,
                },
                send_to: None,
            };

            messages.push(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: config.debt_auction.unwrap_or_else(|| Addr::unchecked("")).to_string(),
                msg: to_binary(&auction_msg)?,
                funds: vec![],
            }));
        } else {
            return Err(ContractError::CustomError {
                val: String::from("Debt Auction contract not added to config"),
            });
        }

        //Save Basket w/ updated revenue
        BASKET.save(deps.storage, &basket)?;
        
        attrs.push(
            attr("amount_sent_to_auction", bad_debt_amount)
        );

        Ok(Response::new()
            .add_messages(messages)
            .add_attributes(attrs))
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn reply(deps: DepsMut, env: Env, msg: Reply) -> StdResult<Response> {
    match msg.id {
        LIQ_QUEUE_REPLY_ID => handle_liq_queue_reply(deps, msg, env),
        WITHDRAW_REPLY_ID => handle_withdraw_reply(deps, env, msg),
        BAD_DEBT_REPLY_ID => Ok(Response::new()),
        id => Err(StdError::generic_err(format!("invalid reply id: {}", id))),
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => to_binary(&CONFIG.load(deps.storage)?),
        QueryMsg::GetBasketPositions {
            start_after,
            limit,
            user_info, 
            user,
        } => to_binary(&query_basket_positions(
            deps,
            env,
            start_after,
            limit,
            user_info, 
            user,
        )?),
        QueryMsg::GetBasket { } => to_binary(&BASKET.load(deps.storage)?),
        QueryMsg::GetBasketRedeemability { position_owner, start_after, limit } => {
            to_binary(&query_basket_redeemability(deps, position_owner, start_after, limit)?)
        }
        QueryMsg::GetBasketDebtCaps { } => {
            to_binary(&query_basket_debt_caps(deps, env)?)
        }
        QueryMsg::GetCreditRate { } => {
            to_binary(&query_basket_credit_interest(deps, env)?)
        }
        QueryMsg::GetCollateralInterest { } => {
            to_binary(&query_collateral_rates(deps)?)
        },
        QueryMsg::SimulateMint { position_info, LTV } => {
            to_binary(&simulate_LTV_mint(deps, env, position_info, LTV)?)
        }
    }
}

/// Check for duplicate assets in a Vec<Asset>
fn duplicate_asset_check(assets: Vec<Asset>) -> Result<(), ContractError> {
    //No duplicates
    for (i, asset) in assets.clone().into_iter().enumerate() {
        let mut assets_copy = assets.clone();
        assets_copy.remove(i);

        if let Some(_asset) = assets_copy
            .into_iter()
            .find(|asset_clone| asset_clone.info.equal(&asset.info))
        {
            return Err(ContractError::CustomError { val: String::from("No duplicate assets in Asset object lists") } );
        }
    }

    Ok(())
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn migrate(deps: DepsMut, env: Env, _msg: MigrateMsg) -> Result<Response, ContractError> {
    
    Ok(Response::default())
}