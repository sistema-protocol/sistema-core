//Token factory fork
//https://github.com/osmosis-labs/bindings/blob/main/contracts/tokenfactory

use std::str::FromStr;

#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    attr, to_binary, Addr, Binary, Decimal, Deps, DepsMut, Env, MessageInfo, QuerierWrapper,
    QueryRequest, Reply, Response, StdError, StdResult, Uint128, SubMsg,
};
use cw2::set_contract_version;

use crate::error::TokenFactoryError;
use crate::state::{Config, TokenInfo, CONFIG, TOKENS};
use membrane::osmosis_proxy::{
    ExecuteMsg, GetDenomResponse, InstantiateMsg, QueryMsg, TokenInfoResponse,
};
use osmo_bindings::{
    ArithmeticTwapToNowResponse, FullDenomResponse, OsmosisMsg, OsmosisQuerier, OsmosisQuery,
    PoolStateResponse,
};

// version info for migration info
const CONTRACT_NAME: &str = "crates.io:osmosis-proxy";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

const CREATE_DENOM_REPLY_ID: u64 = 1u64;

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut<OsmosisQuery>,
    _env: Env,
    info: MessageInfo,
    _msg: InstantiateMsg,
) -> Result<Response, TokenFactoryError> {
    let config = Config {
        owners: vec![info.sender.clone()],
        debt_auction: None,
    };
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    CONFIG.save(deps.storage, &config)?;

    Ok(Response::new()
        .add_attribute("method", "instantiate")
        .add_attribute("owner", info.sender))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut<OsmosisQuery>,
    _env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response<OsmosisMsg>, TokenFactoryError> {
    match msg {
        ExecuteMsg::CreateDenom {
            subdenom,
            basket_id,
            max_supply,
            liquidity_multiplier,
        } => create_denom(
            deps,
            info,
            subdenom,
            basket_id,
            max_supply,
            liquidity_multiplier,
        ),
        ExecuteMsg::ChangeAdmin {
            denom,
            new_admin_address,
        } => change_admin(deps, info, denom, new_admin_address),
        ExecuteMsg::MintTokens {
            denom,
            amount,
            mint_to_address,
        } => mint_tokens(deps, info, denom, amount, mint_to_address),
        ExecuteMsg::BurnTokens {
            denom,
            amount,
            burn_from_address,
        } => burn_tokens(deps, info, denom, amount, burn_from_address),
        ExecuteMsg::EditTokenMaxSupply { denom, max_supply } => {
            edit_token_max(deps, info, denom, max_supply)
        }
        ExecuteMsg::UpdateConfig {
            owner,
            add_owner,
            debt_auction,
        } => update_config(deps, info, owner, debt_auction, add_owner),
    }
}

fn update_config(
    deps: DepsMut<OsmosisQuery>,
    info: MessageInfo,
    owner: Option<String>,
    debt_auction: Option<String>,
    add_owner: bool,
) -> Result<Response<OsmosisMsg>, TokenFactoryError> {
    let mut config = CONFIG.load(deps.storage)?;

    let mut attrs = vec![
        attr("method", "edit_owners"),
        attr("add_owner", add_owner.to_string()),
    ];

    if !validate_authority(config.clone(), info) {
        return Err(TokenFactoryError::Unauthorized {});
    }

    //Edit Owner
    if let Some(owner) = owner {
        if add_owner {
            config.owners.push(deps.api.addr_validate(&owner)?);
        } else {
            deps.api.addr_validate(&owner)?;
            //Filter out owner
            config.owners = config
                .clone()
                .owners
                .into_iter()
                .filter(|stored_owner| *stored_owner != owner)
                .collect::<Vec<Addr>>();
        }
        attrs.push(attr("owner", owner));
    }

    //Edit Debt Auction
    if let Some(debt_auction) = debt_auction {
        config.debt_auction = Some(deps.api.addr_validate(&debt_auction)?);
    }

    //Save Config
    CONFIG.save(deps.storage, &config)?;

    Ok(Response::new().add_attributes(attrs))
}

fn validate_authority(config: Config, info: MessageInfo) -> bool {
    //Owners or Debt Auction have contract authority
    match config
        .owners
        .into_iter()
        .find(|owner| *owner == info.sender)
    {
        Some(_owner) => true,
        None => {
            if let Some(debt_auction) = config.debt_auction {
                info.sender == debt_auction
            } else {
                false
            }
        }
    }
}

pub fn create_denom(
    deps: DepsMut<OsmosisQuery>,
    info: MessageInfo,
    subdenom: String,
    basket_id: String,
    max_supply: Option<Uint128>,
    liquidity_multiplier: Option<Decimal>,
) -> Result<Response<OsmosisMsg>, TokenFactoryError> {

    let config = CONFIG.load(deps.storage)?;
    //Assert Authority
    if !validate_authority(config, info) {
        return Err(TokenFactoryError::Unauthorized {});
    }

    if subdenom.eq("") {
        return Err(TokenFactoryError::InvalidSubdenom { subdenom });
    }
    

    let create_denom_msg = SubMsg::reply_on_success(OsmosisMsg::CreateDenom {
        subdenom: subdenom.clone(),
    }, CREATE_DENOM_REPLY_ID );


    let res = Response::new()
        .add_attribute("method", "create_denom")
        .add_attribute("sub_denom", subdenom)
        .add_attribute("max_supply", max_supply.unwrap_or_else(Uint128::zero))
        .add_attribute("basket_id", basket_id)
        .add_attribute(
            "liquidity_multiplier",
            liquidity_multiplier
                .unwrap_or_else(Decimal::zero)
                .to_string(),
        )
        .add_submessage(create_denom_msg);

    Ok(res)
}

pub fn change_admin(
    deps: DepsMut<OsmosisQuery>,
    info: MessageInfo,
    denom: String,
    new_admin_address: String,
) -> Result<Response<OsmosisMsg>, TokenFactoryError> {
    let config = CONFIG.load(deps.storage)?;
    //Assert Authority
    if !validate_authority(config, info) {
        return Err(TokenFactoryError::Unauthorized {});
    }

    deps.api.addr_validate(&new_admin_address)?;

    validate_denom(deps.querier, denom.clone())?;

    let change_admin_msg = OsmosisMsg::ChangeAdmin {
        denom: denom.clone(),
        new_admin_address: new_admin_address.clone(),
    };

    let res = Response::new()
        .add_attribute("method", "change_admin")
        .add_attribute("denom", denom)
        .add_attribute("new_admin_address", new_admin_address)
        .add_message(change_admin_msg);

    Ok(res)
}

fn edit_token_max(
    deps: DepsMut<OsmosisQuery>,
    info: MessageInfo,
    denom: String,
    max_supply: Uint128,
) -> Result<Response<OsmosisMsg>, TokenFactoryError> {
    let config = CONFIG.load(deps.storage)?;
    //Assert Authority
    if !validate_authority(config, info) {
        return Err(TokenFactoryError::Unauthorized {});
    }

    //Update Token Max
    TOKENS.update(
        deps.storage,
        denom.clone(),
        |token_info| -> Result<TokenInfo, TokenFactoryError> {
            match token_info {
                Some(mut token_info) => {
                    token_info.max_supply = Some(max_supply);

                    Ok(token_info)
                }
                None => {
                    Err(TokenFactoryError::CustomError {
                        val: String::from("Denom was not created in this contract"),
                    })
                }
            }
        },
    )?;

    //If max supply is changed to under current_supply, it halts new mints.

    Ok(Response::new().add_attributes(vec![
        attr("method", "edit_token_max"),
        attr("denom", denom),
        attr("new_max", max_supply),
    ]))
}

pub fn mint_tokens(
    deps: DepsMut<OsmosisQuery>,
    info: MessageInfo,
    denom: String,
    amount: Uint128,
    mint_to_address: String,
) -> Result<Response<OsmosisMsg>, TokenFactoryError> {
    let config = CONFIG.load(deps.storage)?;
    //Assert Authority
    if !validate_authority(config.clone(), info.clone()) {
        return Err(TokenFactoryError::Unauthorized {});
    }

    deps.api.addr_validate(&mint_to_address)?;

    if amount.eq(&Uint128::new(0_u128)) {
        return Result::Err(TokenFactoryError::ZeroAmount {});
    }

    validate_denom(deps.querier, denom.clone())?;

    //Debt Auction can mint over max supply
    let mut mint_allowed = false;
    if let Some(debt_auction) = config.debt_auction {
        if info.sender == debt_auction {
            mint_allowed = true;
        }
    };

    //Update Token Supply
    TOKENS.update(
        deps.storage,
        denom.clone(),
        |token_info| -> Result<TokenInfo, TokenFactoryError> {
            match token_info {
                Some(mut token_info) => {
                    if token_info.clone().max_supply.is_some() {
                        if token_info.current_supply <= token_info.max_supply.unwrap()
                            || mint_allowed
                        {
                            token_info.current_supply += amount;
                            mint_allowed = true;
                        }
                    } else {
                        token_info.current_supply += amount;
                        mint_allowed = true;
                    }

                    Ok(token_info)
                }
                None => {
                    Err(TokenFactoryError::CustomError {
                        val: String::from("Denom was not created in this contract"),
                    })
                }
            }
        },
    )?;

    let mint_tokens_msg = OsmosisMsg::mint_contract_tokens(denom, amount, mint_to_address.clone());    

    let mut res = Response::new()
        .add_attribute("method", "mint_tokens")
        .add_attribute("mint_status", mint_allowed.to_string())
        .add_attribute("amount", Uint128::zero());

    //If a mint was made/allowed
    if mint_allowed {
        res = Response::new()
            .add_attribute("method", "mint_tokens")
            .add_attribute("mint_status", mint_allowed.to_string())
            .add_attribute("amount", amount)
            .add_attribute("mint_to_address", mint_to_address)
            .add_message(mint_tokens_msg);
    }

    Ok(res)
}

pub fn burn_tokens(
    deps: DepsMut<OsmosisQuery>,
    info: MessageInfo,
    denom: String,
    amount: Uint128,
    burn_from_address: String,
) -> Result<Response<OsmosisMsg>, TokenFactoryError> {
    let config = CONFIG.load(deps.storage)?;
    //Assert Authority
    if !validate_authority(config, info) {
        return Err(TokenFactoryError::Unauthorized {});
    }

    if !burn_from_address.is_empty() {
        return Result::Err(TokenFactoryError::BurnFromAddressNotSupported {
            address: burn_from_address,
        });
    }

    if amount.eq(&Uint128::new(0_u128)) {
        return Result::Err(TokenFactoryError::ZeroAmount {});
    }

    validate_denom(deps.querier, denom.clone())?;

    //Update Token Supply
    TOKENS.update(
        deps.storage,
        denom.clone(),
        |token_info| -> Result<TokenInfo, TokenFactoryError> {
            match token_info {
                Some(mut token_info) => {
                    token_info.current_supply -= amount;
                    Ok(token_info)
                }
                None => {
                    Err(TokenFactoryError::CustomError {
                        val: String::from("Denom was not created in this contract"),
                    })
                }
            }
        },
    )?;

    let burn_token_msg = OsmosisMsg::burn_contract_tokens(denom, amount, burn_from_address.clone());

    let res = Response::new()
        .add_attribute("method", "burn_tokens")
        .add_attribute("amount", amount)
        .add_attribute("burn_from_address", burn_from_address)
        .add_message(burn_token_msg);

    Ok(res)
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps<OsmosisQuery>, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::GetDenom {
            creator_address,
            subdenom,
        } => to_binary(&get_denom(deps, creator_address, subdenom)),
        QueryMsg::PoolState { id } => to_binary(&get_pool_state(deps, id)?),
        QueryMsg::ArithmeticTwapToNow {
            id,
            quote_asset_denom,
            base_asset_denom,
            start_time,
        } => to_binary(&get_arithmetic_twap_to_now(
            deps,
            id,
            quote_asset_denom,
            base_asset_denom,
            start_time,
        )?),
        QueryMsg::GetTokenInfo { denom } => to_binary(&get_token_info(deps, denom)?),
        QueryMsg::Config {} => to_binary(&CONFIG.load(deps.storage)?),
    }
}

fn get_token_info(deps: Deps<OsmosisQuery>, denom: String) -> StdResult<TokenInfoResponse> {
    let token_info = TOKENS.load(deps.storage, denom.clone())?;
    Ok(TokenInfoResponse {
        denom,
        current_supply: token_info.current_supply,
        max_supply: token_info.max_supply.unwrap_or_else(Uint128::zero),
    })
}

fn get_arithmetic_twap_to_now(
    deps: Deps<OsmosisQuery>,
    id: u64,
    quote_asset_denom: String,
    base_asset_denom: String,
    start_time: i64,
) -> StdResult<ArithmeticTwapToNowResponse> {
    let msg =
        OsmosisQuery::arithmetic_twap_to_now(id, quote_asset_denom, base_asset_denom, start_time);
    let request: QueryRequest<OsmosisQuery> = OsmosisQuery::into(msg);

    let response: ArithmeticTwapToNowResponse = deps.querier.query(&request)?;

    Ok(response)
}

fn get_pool_state(deps: Deps<OsmosisQuery>, id: u64) -> StdResult<PoolStateResponse> {
    let msg = OsmosisQuery::PoolState { id };
    let request: QueryRequest<OsmosisQuery> = OsmosisQuery::into(msg);

    let response: PoolStateResponse = deps.querier.query(&request)?;

    Ok(response)
}

fn get_denom(deps: Deps<OsmosisQuery>, creator_addr: String, subdenom: String) -> GetDenomResponse {
    let querier = OsmosisQuerier::new(&deps.querier);
    let response = querier.full_denom(creator_addr, subdenom).unwrap();

    GetDenomResponse {
        denom: response.denom,
    }
}

pub fn validate_denom(
    querier: QuerierWrapper<OsmosisQuery>,
    denom: String,
) -> Result<(), TokenFactoryError> {
    let denom_to_split = denom.clone();
    let tokenfactory_denom_parts: Vec<&str> = denom_to_split.split('/').collect();

    if tokenfactory_denom_parts.len() != 3 {
        return Result::Err(TokenFactoryError::InvalidDenom {
            denom,
            message: std::format!(
                "denom must have 3 parts separated by /, had {}",
                tokenfactory_denom_parts.len()
            ),
        });
    }

    let prefix = tokenfactory_denom_parts[0];
    let creator_address = tokenfactory_denom_parts[1];
    let subdenom = tokenfactory_denom_parts[2];

    if !prefix.eq_ignore_ascii_case("factory") {
        return Result::Err(TokenFactoryError::InvalidDenom {
            denom,
            message: std::format!("prefix must be 'factory', was {}", prefix),
        });
    }

    // Validate denom by attempting to query for full denom
    let response = OsmosisQuerier::new(&querier)
        .full_denom(String::from(creator_address), String::from(subdenom));
    if response.is_err() {
        return Result::Err(TokenFactoryError::InvalidDenom {
            denom,
            message: response.err().unwrap().to_string(),
        });
    }

    Result::Ok(())
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn reply(deps: DepsMut<OsmosisQuery>, env: Env, msg: Reply) -> StdResult<Response> {
    match msg.id {
        CREATE_DENOM_REPLY_ID => handle_create_denom_reply(deps, env, msg),
        id => Err(StdError::generic_err(format!("invalid reply id: {}", id))),
    }
}

fn handle_create_denom_reply(
    deps: DepsMut<OsmosisQuery>,
    env: Env,
    msg: Reply,
) -> StdResult<Response> {
    match msg.result.into_result() {
        Ok(result) => {
            let instantiate_event = result
                .events
                .into_iter()
                .find(|e| e.attributes.iter().any(|attr| attr.key == "subdenom"))
                .ok_or_else(|| {
                    StdError::generic_err("unable to find create_denom event".to_string())
                })?;

            let subdenom = &instantiate_event
                .attributes
                .iter()
                .find(|attr| attr.key == "subdenom")
                .unwrap()
                .value;

            let max_supply = &instantiate_event
                .attributes
                .iter()
                .find(|attr| attr.key == "max_supply")
                .unwrap()
                .value;

            //Query fulldenom to save to TOKENS
            let response: FullDenomResponse = OsmosisQuerier::new(&deps.querier)
                .full_denom(String::from(env.contract.address), String::from(subdenom))?;

            let max_supply = {
                if Uint128::from_str(max_supply)?.is_zero() {
                    None
                } else {
                    Some(Uint128::from_str(max_supply)?)
                }
            };
            TOKENS.save(
                deps.storage,
                response.denom,
                &TokenInfo {
                    current_supply: Uint128::zero(),
                    max_supply,
                },
            )?;
        } //We only reply on success
        Err(err) => return Err(StdError::GenericErr { msg: err }),
    }
    Ok(Response::new())
}
