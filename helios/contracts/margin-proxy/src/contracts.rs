use std::str::FromStr;

use cosmwasm_std::{
    attr, entry_point, to_binary, Binary, Deps, DepsMut, Env, MessageInfo, 
    Response, StdResult, Uint128, WasmQuery, SubMsg, Storage, Addr, CosmosMsg, WasmMsg, Reply, StdError, QueryRequest, Decimal, coin, QuerierWrapper,
};
use cw2::set_contract_version;

use membrane::margin_proxy::{Config, ExecuteMsg, InstantiateMsg, QueryMsg};
use membrane::math::decimal_multiplication;
use membrane::positions::{ExecuteMsg as CDP_ExecuteMsg, QueryMsg as CDP_QueryMsg, PositionResponse, PositionsResponse, BasketResponse};
use membrane::apollo_router::ExecuteMsg as RouterExecuteMsg;
use membrane::types::{Position, AssetInfo};

use crate::error::ContractError;
use crate::state::{CONFIG, COMPOSITION_CHECK, USERS, NEW_POSITION_INFO, NUM_OF_LOOPS, LOOP_PARAMETERS};

// Contract name and version used for migration.
const CONTRACT_NAME: &str = "margin_proxy";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

//Constants
const MAX_LIMIT: u64 = 31u64;

//Reply IDs
const EXISTING_DEPOSIT_REPLY_ID: u64 = 1u64;
const NEW_DEPOSIT_REPLY_ID : u64 = 2u64;
const LOOP_REPLY_ID: u64 = 3u64;

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    let config: Config;
    if let Some(owner) = msg.owner {
        config = Config {
            owner: deps.api.addr_validate(&owner)?,
            apollo_router_contract: deps.api.addr_validate(&msg.apollo_router_contract)?,
            positions_contract: deps.api.addr_validate(&msg.positions_contract)?,
            max_slippage: msg.max_slippage,
        };
    } else {
        config = Config {
            owner: info.sender,
            apollo_router_contract: deps.api.addr_validate(&msg.apollo_router_contract)?,
            positions_contract: deps.api.addr_validate(&msg.positions_contract)?,
            max_slippage: msg.max_slippage,
        };
    }

    //Save Config
    CONFIG.save(deps.storage, &config)?;

    Ok(Response::default())
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::Deposit {
            basket_id,
            position_id,
         } => {
            deposit_to_cdp( deps, env, info, basket_id, position_id )
        },
        ExecuteMsg::Loop { 
            basket_id,
            position_id, 
            num_loops,
            target_LTV 
        } => {
            loop_leverage(deps.storage, deps.querier, env, info.sender, basket_id, position_id, num_loops, target_LTV)
        }
        ExecuteMsg::UpdateConfig {
            owner,
            apollo_router_contract,
            positions_contract,
            max_slippage,
        } => update_config(deps, info, owner, apollo_router_contract, positions_contract, max_slippage),
    }
}

//Loop position composition at a desired LTV
//If num_loops is passed, stop the loop there or LTV if met first
fn loop_leverage(
    storage: &mut dyn Storage,
    querier: QuerierWrapper,
    env: Env,
    sender: Addr,
    basket_id: Uint128,
    position_id: Uint128,
    num_loops: Option<u64>,
    target_LTV: Decimal,
) -> Result<Response, ContractError>{

    //Load Config
    let config = CONFIG.load(storage)?;

    //Query Collateral Composition so we know what ratios to loop w/
    let position_response = querier.query::<PositionResponse>(&QueryRequest::Wasm(WasmQuery::Smart {
        contract_addr: config.clone().positions_contract.to_string(), 
        msg: to_binary(&CDP_QueryMsg::GetPosition {
            basket_id: basket_id.clone(),        
            position_id, 
            position_owner: env.contract.address.to_string(), 
        })?
    }))?;

    //Save Position Info
    COMPOSITION_CHECK.save(storage, &(position_response, basket_id))?;

    //Save NUM_OF_LOOPS
    NUM_OF_LOOPS.save(storage, &num_loops)?;

    //Save LOOP_PARAMETERS
    LOOP_PARAMETERS.save(storage, &(sender, target_LTV))?;

    //Increase Debt Msg for position
    let msg = CosmosMsg::Wasm(WasmMsg::Execute { 
        contract_addr: config.clone().positions_contract.to_string(), 
        msg: to_binary(&CDP_ExecuteMsg::IncreaseDebt { 
            basket_id: basket_id.clone(), 
            position_id: position_id.clone(), 
            amount: None, 
            LTV: Some(target_LTV), 
            mint_to_addr: None 
        })?, 
        funds: vec![],
    });
    //Reply_always
    let sub_msg = SubMsg::reply_always(msg, LOOP_REPLY_ID);

    //Response Builder
    let mut attrs = vec![
        attr("method", "looped_leverage"),
        attr("basket_id", basket_id),
        attr("position_id", position_id)
    ];

    if let Some(num) = num_loops {
        attrs.push(attr("num_of_loops_left", num.to_string()));
    } else {
        attrs.push(attr("target_LTV", target_LTV.to_string()));
    }
    
    Ok(Response::new()
        .add_submessage(sub_msg)
        .add_attributes(attrs)
    )
    
}

fn deposit_to_cdp(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,    
    basket_id: Uint128,
    position_id: Option<Uint128>,
) -> Result<Response, ContractError>{

    //Load Config
    let config = CONFIG.load( deps.storage )?;

    //If there is a position_id, make sure the current user is the owner of that position
    if let Some(position_id) = position_id {
        //If id isn't found in the list for the User, error
        if let None = USERS.load(deps.storage, info.clone().sender )?.into_iter().find(|ids| ids == &(basket_id, position_id)){
            return Err(ContractError::InvalidID { id: position_id })
        } 
        //If the user owns the position we still error bc the contract needs to own the position for the user
    };
   
    //Create Reponse objects
    let sub_msg: SubMsg;
    let mut attrs = vec![];

    //Create Deposit Msg
    if position_id.is_none(){

        //Create DepositMsg
        let deposit_msg = CDP_ExecuteMsg::Deposit {
            position_owner: None, //Margin Contract
            position_id: None, //New position
            basket_id: basket_id.clone(),        
        };
    
        let msg = CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr: config.clone().positions_contract,
            msg: to_binary(&deposit_msg)?,
            funds: info.clone().funds,
        });

        //Create a submsg
        //Reply on success and query the position ID for this deposit
        sub_msg = SubMsg::reply_on_success(msg, NEW_DEPOSIT_REPLY_ID);

        //Save User so the contract knows who to save the new position id under
        NEW_POSITION_INFO.save(deps.storage, &(info.clone().sender, basket_id))?;

    } else {
        //Adding Position_id to User's list        
        let unwrapped_id = position_id.unwrap() ;

        append_to_user_list_of_ids( deps.storage, info.clone().sender, basket_id.clone(), unwrapped_id.clone() )?;

        let deposit_msg = CDP_ExecuteMsg::Deposit {
            position_owner: None, //Margin Contract
            position_id: position_id.clone(), 
            basket_id: basket_id.clone(),        
        };
    
        let msg = CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr: config.clone().positions_contract,
            msg: to_binary(&deposit_msg)?,
            funds: info.clone().funds,
        });

        //Submsg reply on success
        sub_msg = SubMsg::reply_on_success(msg, EXISTING_DEPOSIT_REPLY_ID);

        //Confirm that deposits are of the same composition as the position by querying cAsset + ratios now and asserting equality in the reply
        let position_response = deps.querier.query::<PositionResponse>(&QueryRequest::Wasm(WasmQuery::Smart {
            contract_addr: config.clone().positions_contract.to_string(), 
            msg: to_binary(&CDP_QueryMsg::GetPosition {
                basket_id: basket_id.clone(),        
                position_id: unwrapped_id, 
                position_owner: env.contract.address.to_string(), 
            })?
        }))?;

        //Save Position Info
        COMPOSITION_CHECK.save(deps.storage, &(position_response, basket_id))?;
    }
    
    
    Ok( Response::new()
        .add_submessage(sub_msg) )
}


#[cfg_attr(not(feature = "library"), entry_point)]
pub fn reply(deps: DepsMut, env: Env, msg: Reply) -> StdResult<Response> {
    match msg.id {
        EXISTING_DEPOSIT_REPLY_ID => handle_existing_deposit_reply(deps, env, msg),
        NEW_DEPOSIT_REPLY_ID => handle_new_deposit_reply(deps, env, msg),
        LOOP_REPLY_ID => handle_loop_reply(deps, env, msg),
        id => Err(StdError::generic_err(format!("invalid reply id: {}", id))),
    }
}

//On success, sell for collateral composition, redeposit & call loop fn again
//Increment Loop number
//On error, reset loop number
fn handle_loop_reply(
    deps: DepsMut,
    env: Env,
    msg: Reply,
) -> StdResult<Response>{
    
    match msg.result.into_result() {
        Ok(result) => {

            let debt_mint_event = result
                .events
                .into_iter()
                .find(|e| e.attributes.iter().any(|attr| attr.key == "total_loan"))
                .ok_or_else(|| StdError::generic_err(format!("unable to find debt_mint event")))?;

            let increased_debt_amount = &debt_mint_event
                .attributes
                .iter()
                .find(|attr| attr.key == "increased_by")
                .unwrap()
                .value;

            //Get how much credit was minted
            let credit_amount = Decimal::from_str(increased_debt_amount)?;

            //Load Config
            let config: Config = CONFIG.load(deps.storage)?;

            //Load Previous Composition
            let previous_composition = COMPOSITION_CHECK.load(deps.storage)?;
            let basket_id = previous_composition.1;
            
            //Create (AssetInfo, ratio) tuple list
            let composition_to_loop = previous_composition.0.clone().cAsset_ratios
                .into_iter()
                .enumerate()
                .map(|(i, ratio)| (previous_composition.0.clone().collateral_assets[i].asset.info, ratio) )
                .collect::<Vec<(AssetInfo, Decimal)>>();

            //Query Basket credit asset
            let credit_asset = deps.querier.query::<BasketResponse>(&QueryRequest::Wasm(WasmQuery::Smart { 
                contract_addr: config.clone().positions_contract.to_string(), 
                msg: to_binary(&CDP_QueryMsg::GetBasket { basket_id: basket_id.clone() })?
            }))?
            .credit_asset;

            //Initialize messages
            let mut messages = vec![];

            //Sell new debt for collateral composition & redeposit 
            for (collateral, ratio) in composition_to_loop {
                
                let credit_to_sell = decimal_multiplication(credit_amount, ratio);

                let msg = create_router_msg( 
                    env.contract.address.to_string(),
                    config.clone().positions_contract.to_string(),
                    config.clone().apollo_router_contract.to_string(),
                    collateral,
                    credit_asset.info,
                    credit_to_sell,
                    basket_id,
                    Some(previous_composition.clone().0.position_id),
                    config.clone().max_slippage,
                )?;

                messages.push(SubMsg::new(msg));
            }
            
            //Load parameters from last loop
            let loop_parameters = LOOP_PARAMETERS.load(deps.storage)?;

            //Update number of loops left
            let num_of_loops_left = if let Some(num) = NUM_OF_LOOPS.load(deps.storage)?{
                //If num_of_loops_left is 0, finish the loop
                if num == 0 {
                    return Ok(Response::new().add_attribute("loop_finished", "true"))
                } else {
                    Some(num - 1u64)
                }
            } else {
                None
            };

            //Recall loop function
            let mut res = match loop_leverage(
                deps.storage, 
                deps.querier, 
                env, 
                loop_parameters.0, 
                basket_id, 
                previous_composition.clone().0.position_id, 
                num_of_loops_left, 
                loop_parameters.1
            ){
                Ok(res) => res,
                Err(err) => return Err(StdError::GenericErr { msg: err.to_string() })
            };            
            
            //Push new debt mint messages to the end
            //collateral sales -> new debt
            messages.extend(res.messages);

            
            //Response Builder
            let mut attrs = vec![
                attr("debt_increased_by", credit_amount.to_string())
            ];

            if let Some(num) = num_of_loops_left {
                attrs.push(attr("num_of_loops_left", num.to_string()));
            } else {
                attrs.push(attr("target_LTV", loop_parameters.1.to_string()));
            }
            
            Ok(Response::new()
                .add_submessages(messages)
                .add_attributes(attrs)
            )

        },
        Err(string) => {            
            //Error likely means the target_LTV was hit
            Ok(Response::new().add_attribute("increase_debt_error", string))
        }
    }
}

fn create_router_msg(
    contract_addr: String, //Margin contract
    positions_contract: String,
    apollo_router_addr: String,
    asset_to_buy: AssetInfo,
    asset_to_sell: AssetInfo, //Credit asset
    amount_to_sell: Decimal,
    basket_id: Uint128,
    position_id: Option<Uint128>,
    max_spread: Option<Decimal>,
) -> StdResult<CosmosMsg>{
    //We know the credit asset is a native asset
    if let AssetInfo::NativeToken { denom } = asset_to_sell {
        
        let router_msg = RouterExecuteMsg::SwapFromNative {
            to: asset_to_buy.clone(), //Buy
            max_spread, 
            recipient: Some(positions_contract), //Deposit to positions contract
            hook_msg: Some(to_binary(&CDP_ExecuteMsg::Deposit { 
                basket_id, 
                position_id, 
                position_owner: Some(contract_addr), //Owner is this contract
            })?),
            split: None,
        };

        let payment = coin(
            (amount_to_sell * Uint128::new(1u128)).u128(),
            denom,
        );

        let msg: CosmosMsg = CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr: apollo_router_addr,
            msg: to_binary(&router_msg)?,
            funds: vec![payment],
        });

        Ok(msg)            
    } else {
        return Err(StdError::GenericErr { msg: String::from("Credit assets are supposed to be native") })
    }

}

fn handle_existing_deposit_reply(
    deps: DepsMut,
    env: Env,
    msg: Reply,
) -> StdResult<Response>{
    match msg.result.into_result() {
        Ok(_result) => {

            //Load Config 
            let config = CONFIG.load(deps.storage)?;

            //Load Composition Check
            let previous_composition: (PositionResponse, Uint128) = COMPOSITION_CHECK.load(deps.storage)?;

            //Confirm cAsset_ratios and cAsset makeup hasn't changed    
            ////Query current Position
            let position_response = deps.querier.query::<PositionResponse>(&QueryRequest::Wasm(WasmQuery::Smart {
                contract_addr: config.clone().positions_contract.to_string(), 
                msg: to_binary(&CDP_QueryMsg::GetPosition {
                    basket_id: previous_composition.1, 
                    position_id: previous_composition.0.position_id, 
                    position_owner: env.contract.address.to_string(), 
                })?
            }))?;

            //Create (AssetInfo, ratio) tuple list
            let composition = previous_composition.0.clone().cAsset_ratios
                .into_iter()
                .enumerate()
                .map(|(i, ratio)| (previous_composition.0.clone().collateral_assets[i].asset.info, ratio) )
                .collect::<Vec<(AssetInfo, Decimal)>>();

            //Create lists for both previous and current position asset_infos
            let previous_assets = previous_composition.0.clone().collateral_assets
                .into_iter()
                .map(|cAsset| cAsset.asset.info )
                .collect::<Vec<AssetInfo>>();

            let current_assets = position_response.clone().collateral_assets
                .into_iter()
                .map(|cAsset| cAsset.asset.info )
                .collect::<Vec<AssetInfo>>();

            //Assert ratio equality & cAsset equality
            if previous_composition.0.cAsset_ratios != position_response.cAsset_ratios 
               || 
               previous_assets != current_assets{                
                return Err(StdError::GenericErr { msg: format!("Can only deposit more of the current position composition: {:?}", composition) })
            }

            Ok(Response::new().add_attribute("valid_composition", "true"))

        },
        Err(string) => {
            //This is only reply_on_success so this shouldn't be reached
            Ok(Response::new().add_attribute("error", string))
        }
    }
}

fn handle_new_deposit_reply(
    deps: DepsMut,
    env: Env,
    msg: Reply,
) -> StdResult<Response>{

    match msg.result.into_result() {
        Ok(_result) => {

            //Load Config
            let config = CONFIG.load(deps.storage)?;

            //Load NEW_POSITION_INFO
            let new_position_info = NEW_POSITION_INFO.load(deps.storage)?;

            //Get new Position_ID
            ////Query Positions contract for all positions from this contract and save last id to the user
            let positions_response = deps.querier.query::<PositionsResponse>(&QueryRequest::Wasm(WasmQuery::Smart { 
                contract_addr: config.clone().positions_contract.to_string(), 
                msg: to_binary(&CDP_QueryMsg::GetUserPositions {
                    basket_id: Some(new_position_info.1),
                    user: env.contract.address.to_string(),
                    limit: None,
                })?
            }))?;

            //Get latest position
            let latest_position: Position = positions_response.positions.pop().unwrap();

            //Save ID to User
            if let Err(err) = append_to_user_list_of_ids( deps.storage, new_position_info.0, new_position_info.1, latest_position.position_id){
                return Err(StdError::GenericErr { msg: err.to_string() })
            };

            Ok(Response::new()
                .add_attributes(vec![
                    attr("user", new_position_info.0),
                    attr("new_id",  latest_position.position_id),
            ]))
        },
        Err(string) => {
            //This is only reply_on_success so this shouldn't be reached
            Ok(Response::new().add_attribute("error", string))
        }
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => to_binary(&CONFIG.load(deps.storage)?),
    }
}


fn append_to_user_list_of_ids(
    storage: &mut dyn Storage,
    user: Addr,
    basket_id: Uint128,
    position_id: Uint128,
) -> Result<(), ContractError>{

    USERS.update( storage, user, |list_of_ids| -> Result<Vec<(Uint128, Uint128)>, ContractError> {
        match list_of_ids {
            Some(mut list) => {
                list.push( (basket_id, position_id) );

                Ok(list)
            },
            None => {
                Ok(vec![ (basket_id, position_id) ])
            }
        }
    })?;

    Ok(())
}

fn update_config(
    deps: DepsMut,
    info: MessageInfo,
    owner: Option<String>,
    apollo_router_contract: Option<String>,
    positions_contract: Option<String>,
    max_slippage: Option<Uint128>,
) -> Result<Response, ContractError> {

    let mut config = CONFIG.load(deps.storage)?;

    //Assert authority
    if info.sender != config.owner {
        return Err(ContractError::Unauthorized {});
    }

    //Save optionals
    if let Some(addr) = owner {
        config.owner = deps.api.addr_validate(&addr)?;
    }
    if let Some(addr) = apollo_router_contract {
        config.apollo_router_contract = deps.api.addr_validate(&addr)?;
    }
    if let Some(addr) = positions_contract {
        config.positions_contract = deps.api.addr_validate(&addr)?;
    }
    if let Some(slippage) = max_slippage {
        config.max_slippage = slippage;
    }

    //Save Config
    CONFIG.save(deps.storage, &config)?;

    Ok(Response::new())
}

