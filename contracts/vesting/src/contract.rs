///Expanded Fork of: https://github.com/astroport-fi/astroport-governance/tree/main/contracts/builder_unlock

#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    attr, coin, to_binary, Addr, BankMsg, Binary, Coin, CosmosMsg, Decimal, Deps, DepsMut, Env,
    MessageInfo, QueryRequest, Response, StdError, StdResult, Uint128, WasmMsg, WasmQuery, QuerierWrapper, Storage,
};
use cw2::set_contract_version;
use cw20::Cw20ExecuteMsg;

use membrane::vesting::{ Config, ExecuteMsg, InstantiateMsg, QueryMsg };
use membrane::governance::{ExecuteMsg as GovExecuteMsg, ProposalMessage, ProposalVoteOption};
use membrane::math::decimal_division;
use membrane::osmosis_proxy::ExecuteMsg as OsmoExecuteMsg;
use membrane::staking::{
    ExecuteMsg as StakingExecuteMsg, QueryMsg as StakingQueryMsg, RewardsResponse, StakerResponse,
};
use membrane::types::{Allocation, Asset, AssetInfo, VestingPeriod, Recipient};

use crate::error::ContractError;
use crate::query::{query_allocation, query_unlocked, query_recipients, query_recipient};
use crate::state::{CONFIG, RECIPIENTS};

// version info for migration info
const CONTRACT_NAME: &str = "crates.io:vesting";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

//Constants
const SECONDS_IN_A_DAY: u64 = 86400u64;

/////////////////////
///**Make sure everything is allocated before fees are sent**
/////////////////////

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    let mut config = Config {
        owner: info.sender,
        total_allocation: msg.initial_allocation,
        mbrn_denom: msg.mbrn_denom,
        osmosis_proxy: deps.api.addr_validate(&msg.osmosis_proxy)?,
        staking_contract: deps.api.addr_validate(&msg.staking_contract)?,
    };

    //Set Optionals
    match msg.owner {
        Some(address) => match deps.api.addr_validate(&address) {
            Ok(addr) => config.owner = addr,
            Err(_) => {}
        },
        None => {}
    };

    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    CONFIG.save(deps.storage, &config)?;

    //Save Recipients w/ the Labs team as the first Recipient
    RECIPIENTS.save(deps.storage, &vec![
        Recipient { 
            recipient: deps.api.addr_validate(&msg.labs_addr)?, 
            allocation: Some(Allocation { 
                amount: msg.initial_allocation, 
                amount_withdrawn: Uint128::zero(), 
                start_time_of_allocation: env.block.time.seconds(), 
                vesting_period: VestingPeriod { cliff: 730, linear: 365 },
            }), 
            claimables: vec![], 
        }
    ])?;

    let mut res = mint_initial_allocation(env.clone(), config.clone())?;

    let mut attrs = vec![
        attr("method", "instantiate"),
        attr("owner", config.owner.to_string()),
        attr("owner", env.contract.address.to_string()),
    ];
    attrs.extend(res.attributes);
    res.attributes = attrs;

    Ok(res)
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::Receive(_msg) => Ok(Response::new()),
        ExecuteMsg::AddRecipient { recipient } => add_recipient(deps, info, recipient),
        ExecuteMsg::RemoveRecipient { recipient } => remove_recipient(deps, info, recipient),
        ExecuteMsg::AddAllocation {
            recipient,
            allocation,
            vesting_period,
        } => add_allocation(deps, env, info, recipient, allocation, vesting_period),
        ExecuteMsg::WithdrawUnlocked {} => withdraw_unlocked(deps, env, info),
        ExecuteMsg::ClaimFeesforContract {} => claim_fees_for_contract(deps, env),
        ExecuteMsg::ClaimFeesforRecipient {} => claim_fees_for_recipient(deps, info),
        ExecuteMsg::SubmitProposal {
            title,
            description,
            link,
            messages,
            expedited
        } => submit_proposal(deps, info, title, description, link, messages, expedited),
        ExecuteMsg::CastVote { proposal_id, vote } => cast_vote(deps, info, proposal_id, vote),
        ExecuteMsg::UpdateConfig {
            owner,
            mbrn_denom,
            osmosis_proxy,
            staking_contract,
            additional_allocation,
        } => update_config(
            deps,
            info,
            owner,
            mbrn_denom,
            osmosis_proxy,
            staking_contract,
            additional_allocation
        ),
    }
}

//Calls the Governance contract SubmitProposalMsg
fn submit_proposal(
    deps: DepsMut,
    info: MessageInfo,
    title: String,
    description: String,
    link: Option<String>,
    messages: Option<Vec<ProposalMessage>>,
    expedited: bool,
) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;
    let recipients = RECIPIENTS.load(deps.storage)?;

    match recipients
        
        .into_iter()
        .find(|recipient| recipient.recipient == info.sender)
    {
        Some(recipient) => {
            let message = CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: config.owner.to_string(),
                msg: to_binary(&GovExecuteMsg::SubmitProposal {
                    title,
                    description,
                    link,
                    messages,
                    recipient: Some(recipient.recipient.to_string()),
                    expedited,
                })?,
                funds: vec![],
            });

            Ok(Response::new()
                .add_attributes(vec![
                    attr("method", "submit_proposal"),
                    attr("proposer", recipient.recipient.to_string()),
                ])
                .add_message(message))
        }
        None => Err(ContractError::InvalidRecipient {}),
    }
}

//Calls the Governance contract CastVoteMsg
fn cast_vote(
    deps: DepsMut,
    info: MessageInfo,
    proposal_id: u64,
    vote: ProposalVoteOption,
) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;
    let recipients = RECIPIENTS.load(deps.storage)?;

    match recipients
        
        .into_iter()
        .find(|recipient| recipient.recipient == info.sender)
    {
        Some(recipient) => {
            let message = CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: config.owner.to_string(),
                msg: to_binary(&GovExecuteMsg::CastVote {
                    proposal_id,
                    vote,
                    recipient: Some(recipient.recipient.to_string()),
                })?,
                funds: vec![],
            });

            Ok(Response::new()
                .add_attributes(vec![
                    attr("method", "cast_vote"),
                    attr("voter", recipient.recipient.to_string()),
                ])
                .add_message(message))
        }
        None => Err(ContractError::InvalidRecipient {}),
    }
}

//Claim a Recipient's proportion of staking rewards that were previously claimed using ClaimFeesForContract
fn claim_fees_for_recipient(deps: DepsMut, info: MessageInfo) -> Result<Response, ContractError> {

    //Load recipients
    let mut recipients = RECIPIENTS.load(deps.storage)?;

    let mut messages: Vec<CosmosMsg> = vec![];
    let claimables: Vec<Asset> = vec![];

    //Find Recipient claimables
    match recipients
        .clone()
        .into_iter()
        .enumerate()
        .find(|(_i, recipient)| recipient.recipient == info.sender)
    {
        Some((i, recipient)) => {
            if recipient.claimables == vec![] {
                return Err(ContractError::CustomError {
                    val: String::from("Nothing to claim"),
                });
            }

            //Create withdraw msg for each claimable asset
            for claimable in recipient.clone().claimables {
                messages.push(withdrawal_msg(claimable, recipient.clone().recipient)?);
            }

            //Set claims to Empty Vec
            recipients[i].claimables = vec![];
        }
        None => return Err(ContractError::InvalidRecipient {}),
    }
    //Save Edited claims
    RECIPIENTS.save(deps.storage, &recipients)?;

    //Claimables into String List
    let claimables_string: Vec<String> = claimables
        .into_iter()
        .map(|claim| claim.to_string())
        .collect::<Vec<String>>();

    Ok(Response::new().add_messages(messages).add_attributes(vec![
        attr("method", "claim_fees_for_recipient"),
        attr("claimables", format!("{:?}", claimables_string)),
    ]))
}

//Claim staking rewards for all contract owned staked MBRN
fn claim_fees_for_contract(deps: DepsMut, env: Env) -> Result<Response, ContractError> {

    //Load Config
    let config = CONFIG.load(deps.storage)?;

    //Query Rewards
    let res: RewardsResponse = deps.querier.query(&QueryRequest::Wasm(WasmQuery::Smart {
        contract_addr: config.staking_contract.to_string(),
        msg: to_binary(&StakingQueryMsg::StakerRewards {
            staker: env.contract.address.to_string(),
        })?,
    }))?;

    //Split rewards w/ recipients based on allocation amounts
    if res.claimables != vec![] {
        let recipients = RECIPIENTS.load(deps.storage)?;

        let mut allocated_recipients: Vec<Recipient> = recipients
            .clone()
            .into_iter()
            .filter(|recipient| recipient.allocation.is_some())
            .collect::<Vec<Recipient>>();

        //Calculate allocation ratios
        let allocation_ratios = get_allocation_ratios(deps.querier, env.clone(), config.clone(), &mut allocated_recipients)?;
        
        //Add Recipient's ratio of each claim asset to position
        for claim_asset in res.clone().claimables {
            for (i, recipient) in allocated_recipients.clone().into_iter().enumerate() {
                match recipient
                    .clone()
                    .claimables
                    .into_iter()
                    .enumerate()
                    .find(|(_index, claim)| claim.info == claim_asset.info)
                {
                    //If found in claimables, add amount to position
                    Some((index, _claim)) => {
                        allocated_recipients[i].claimables[index].amount +=
                            claim_asset.amount * allocation_ratios[i]
                    }
                    //If None, add asset as if new
                    None => allocated_recipients[i].claimables.push(Asset {
                        amount: claim_asset.amount * allocation_ratios[i],
                        ..claim_asset.clone()
                    }),
                }
            }
        }

        //Filter out, Extend, Save
        let mut new_recipients: Vec<Recipient> = recipients
            
            .into_iter()
            .filter(|recipient| recipient.allocation.is_none())
            .collect::<Vec<Recipient>>();
        new_recipients.extend(allocated_recipients);
        RECIPIENTS.save(deps.storage, &new_recipients)?;
    }

    //Construct ClaimRewards Msg to Staking Contract
    let msg = CosmosMsg::Wasm(WasmMsg::Execute {
        contract_addr: config.staking_contract.to_string(),
        msg: to_binary(&StakingExecuteMsg::ClaimRewards {
            claim_as_cw20: None,
            claim_as_native: None,
            restake: false,
            send_to: None,
        })?,
        funds: vec![],
    });

    //Claimables into String List
    let claimables_string: Vec<String> = res
        .claimables
        .into_iter()
        .map(|claim| claim.to_string())
        .collect::<Vec<String>>();

    Ok(Response::new().add_message(msg).add_attributes(vec![
        attr("method", "claim_fees_for_contract"),
        attr("claimables", format!("{:?}", claimables_string)),
    ]))
}

fn get_allocation_ratios(querier: QuerierWrapper, env: Env, config: Config, recipients: &mut Vec<Recipient>) -> StdResult<Vec<Decimal>> {

    let mut allocation_ratios: Vec<Decimal> = vec![];

    //Get Contract's MBRN staked amount
    let staked_mbrn = querier.query_wasm_smart::<StakerResponse>(
        config.staking_contract, 
        &StakingQueryMsg::UserStake { staker: env.contract.address.to_string() }
    )?
    .total_staked;

    for recipient in recipients.clone() {        

        //Initialize allocation 
        let allocation = recipient.clone().allocation.unwrap();
        
        //Ratio of base Recipient's allocation.amount to total_staked
        allocation_ratios.push(decimal_division(
            Decimal::from_ratio(
                allocation.amount,
                Uint128::new(1u128),
            ),
            Decimal::from_ratio(staked_mbrn, Uint128::new(1u128)),
        ));
    }
    

    Ok(allocation_ratios)
}

fn update_config(
    deps: DepsMut,
    info: MessageInfo,
    owner: Option<String>,
    mbrn_denom: Option<String>,
    osmosis_proxy: Option<String>,
    staking_contract: Option<String>,    
    additional_allocation: Option<Uint128>,
) -> Result<Response, ContractError> {
    let mut config = CONFIG.load(deps.storage)?;

    if info.sender != config.owner {
        return Err(ContractError::Unauthorized {});
    }

    let mut attrs = vec![attr("method", "update_config")];

    if let Some(owner) = owner {
        config.owner = deps.api.addr_validate(&owner)?;
        attrs.push(attr("new_owner", owner));
    };
    if let Some(osmosis_proxy) = osmosis_proxy {
        config.osmosis_proxy = deps.api.addr_validate(&osmosis_proxy)?;
        attrs.push(attr("new_osmosis_proxy", osmosis_proxy));
    };
    if let Some(mbrn_denom) = mbrn_denom {
        config.mbrn_denom = mbrn_denom.clone();
        attrs.push(attr("new_mbrn_denom", mbrn_denom));
    };
    if let Some(staking_contract) = staking_contract {
        config.staking_contract = deps.api.addr_validate(&staking_contract)?;
        attrs.push(attr("new_staking_contract", staking_contract));
    };
    if let Some(additional_allocation) = additional_allocation {
        config.total_allocation += additional_allocation;
        attrs.push(attr("new_allocation", additional_allocation));
    };

    CONFIG.save(deps.storage, &config)?;

    Ok(Response::new().add_attributes(attrs))
}


//Withdraw unvested MBRN
//If there is none to distribute in the contract, the amount will be unstaked
fn withdraw_unlocked(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;

    let recipients = RECIPIENTS.load(deps.storage)?;

    let mut message: CosmosMsg;

    let unlocked_amount: Uint128;
    let mut unstaked_amount: Uint128 = Uint128::zero();
    let new_allocation: Allocation;

    //Find Recipient
    match recipients
        .clone()
        .into_iter()
        .find(|recipient| recipient.recipient == info.sender)
    {
        Some(mut recipient) => {
            if recipient.allocation.is_some() {
                (unlocked_amount, new_allocation) =
                    get_unlocked_amount(recipient.allocation, env.block.time.seconds());

                //Save new allocation
                recipient.allocation = Some(new_allocation);

                let mut new_recipients = recipients
                    .into_iter()
                    .filter(|recipient| recipient.recipient != info.sender)
                    .collect::<Vec<Recipient>>();
                new_recipients.push(recipient.clone());

                RECIPIENTS.save(deps.storage, &new_recipients)?;

                //Mint the unlocked amount
                //Mint will error if 0
                message = CosmosMsg::Wasm(WasmMsg::Execute { 
                    contract_addr: config.osmosis_proxy.to_string(), 
                    msg: to_binary(&OsmoExecuteMsg::MintTokens { 
                        denom: config.mbrn_denom, 
                        amount: unlocked_amount, 
                        mint_to_address: info.sender.to_string(), 
                    })?, 
                    funds: vec![], 
                });
                    
                
                
            } else {
                return Err(ContractError::InvalidAllocation {});
            }
        }
        None => return Err(ContractError::InvalidRecipient {}),
    };


    
    Ok(Response::new()
        .add_message(message)
        .add_attributes(vec![
            attr("method", "withdraw_unlocked"),
            attr("recipient", info.sender),
            attr("withdrawn_amount", String::from(unlocked_amount)),
        ])
    )
    
}

//Get unvested amount 
pub fn get_unlocked_amount(
    //This is an option bc the Recipient's allocation is. Its existence is confirmed beforehand.
    allocation: Option<Allocation>, 
    current_block_time: u64, //in seconds
) -> (Uint128, Allocation) {
    let mut allocation = allocation.unwrap();

    let mut unlocked_amount = Uint128::zero();

    //Calculate unlocked amount
    let time_passed = current_block_time - allocation.clone().start_time_of_allocation;

    let cliff_in_seconds = allocation.clone().vesting_period.cliff * SECONDS_IN_A_DAY;

    //If cliff has been passed then calculate linear unlock
    if time_passed >= cliff_in_seconds {
        let time_passed_cliff = time_passed - cliff_in_seconds;

        let linear_in_seconds = allocation.clone().vesting_period.linear * SECONDS_IN_A_DAY;

        if time_passed_cliff < linear_in_seconds {
            //Unlock amount based off time into linear vesting period
            let ratio_unlocked = decimal_division(
                Decimal::from_ratio(Uint128::new(time_passed_cliff as u128), Uint128::new(1u128)),
                Decimal::from_ratio(Uint128::new(linear_in_seconds as u128), Uint128::new(1u128)),
            );

            let newly_unlocked: Uint128;
            if !ratio_unlocked.is_zero() {
                newly_unlocked = (ratio_unlocked * allocation.clone().amount)
                    - allocation.clone().amount_withdrawn;
            } else {
                newly_unlocked = Uint128::zero();
            }

            unlocked_amount += newly_unlocked;

            //Edit Allocation object
            allocation.amount_withdrawn += newly_unlocked;
        } else {
            //Unlock full amount
            unlocked_amount += allocation.clone().amount - allocation.clone().amount_withdrawn;

            allocation.amount_withdrawn += allocation.clone().amount;
        }
    }

    (unlocked_amount, allocation)
}

//Add allocation to a Recipient
fn add_allocation(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    recipient: String,
    allocation: Uint128,
    vesting_period: Option<VestingPeriod>,
) -> Result<Response, ContractError> {

    let config = CONFIG.load(deps.storage)?;    

    match vesting_period {
        //If Some && from the contract owner, adds new allocation amount to a valid Recipient
        Some(vesting_period) => {
            //Valid contract caller
            if info.sender != config.owner {
                return Err(ContractError::Unauthorized {});
            }

            //Add allocation to a Recipient
            RECIPIENTS.update(
                deps.storage,
                |mut recipients| -> Result<Vec<Recipient>, ContractError> {
                    //Add allocation
                    recipients = recipients
                        .into_iter()
                        .map(|mut stored_recipient| {
                            if stored_recipient.recipient == recipient {
                                stored_recipient.allocation = Some(Allocation {
                                    amount: allocation,
                                    amount_withdrawn: Uint128::zero(),
                                    start_time_of_allocation: env.block.time.seconds(),
                                    vesting_period: vesting_period.clone(),
                                });
                            }

                            stored_recipient
                        })
                        .collect::<Vec<Recipient>>();

                    Ok(recipients)
                },
            )?;

        },
        //If None && called by an existing Recipient, subtract & delegate part of the allocation to the allotted recipient
        //Add new Recipient object for the new recipient 
        None => {

            //Validate recipient
            let valid_recipient = deps.api.addr_validate(&recipient)?;

            //Initialize new_allocation
            let mut new_allocation: Option<Allocation> = None;

            //Add Recipient
            RECIPIENTS.update(
                deps.storage,
                |mut recipients| -> Result<Vec<Recipient>, ContractError> {
                    
                    //Divvy info.sender's allocation
                    recipients = recipients
                        .into_iter()
                        .map(|mut stored_recipient| {
                            //Checking equality to info.sender
                            if stored_recipient.recipient == info.clone().sender && stored_recipient.allocation.is_some(){

                                //Initialize stored_allocation 
                                let mut stored_allocation = stored_recipient.allocation.unwrap();                               

                                //Decrease stored_allocation.amount & set new_allocation
                                stored_allocation.amount = match stored_allocation.amount.checked_sub(allocation){
                                    Ok(diff) => {
                                    
                                        //Set new_allocation
                                        new_allocation = Some(
                                            Allocation { 
                                                amount: allocation, 
                                                amount_withdrawn: Uint128::zero(),
                                                ..stored_allocation.clone()
                                            }
                                        );

                                        diff
                                    },
                                    Err(_err) => {
                                        //Set new_allocation
                                        new_allocation = Some(
                                            Allocation { 
                                                amount: stored_allocation.amount, 
                                                amount_withdrawn: Uint128::zero(),
                                                ..stored_allocation.clone()
                                            }
                                        );
                                    
                                        Uint128::zero()
                                    }
                                };
                                                                
                                
                                stored_recipient.allocation = Some(stored_allocation);

                            }

                            stored_recipient
                        })
                        .collect::<Vec<Recipient>>();

                    
                    if recipients
                        .iter()
                        .any(|recipient| recipient.recipient == valid_recipient)
                    {
                        return Err(ContractError::CustomError {
                            val: String::from("Duplicate Recipient"),
                        });
                    }

                    recipients.push(Recipient {
                        recipient: valid_recipient,
                        allocation: new_allocation,
                        claimables: vec![],
                    });

                    Ok(recipients)
                },
            )?;
            
        },
    };

    //Get allocation total
    let mut allocation_total: Uint128 = Uint128::zero();

    for recipient in RECIPIENTS.load(deps.storage)?.into_iter() {
         if recipient.allocation.is_some() {
             allocation_total += recipient.allocation.unwrap().amount;
         }
    }

    //Error if over allocating
    if allocation_total > config.total_allocation {
        return Err(ContractError::OverAllocated {});
    }

    Ok(Response::new().add_attributes(vec![
        attr("method", "increase_allocation"),
        attr("recipient", recipient),
        attr("allocation_increase", String::from(allocation)),
    ]))
}


//Add new Recipient
fn add_recipient(
    deps: DepsMut,
    info: MessageInfo,
    recipient: String,
) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;

    if info.sender != config.owner {
        return Err(ContractError::Unauthorized {});
    }

    let valid_recipient = deps.api.addr_validate(&recipient)?;

    //Add new Recipient
    RECIPIENTS.update(
        deps.storage,
        |mut recipients| -> Result<Vec<Recipient>, ContractError> {
            if recipients
                .iter()
                .any(|recipient| recipient.recipient == valid_recipient)
            {
                return Err(ContractError::CustomError {
                    val: String::from("Duplicate Recipient"),
                });
            }

            recipients.push(Recipient {
                recipient: valid_recipient,
                allocation: None,
                claimables: vec![],
            });

            Ok(recipients)
        },
    )?;

    Ok(Response::new().add_attributes(vec![
        attr("method", "add_recipient"),
        attr("Recipient", recipient),
    ]))
}

//Remove existing Recipient
fn remove_recipient(
    deps: DepsMut,
    info: MessageInfo,
    recipient: String,
) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;

    if info.sender != config.owner {
        return Err(ContractError::Unauthorized {});
    }

    //Remove Recipient
    RECIPIENTS.update(
        deps.storage,
        |recipients| -> Result<Vec<Recipient>, ContractError> {
            //Filter out Recipient and save
            Ok(recipients
                .into_iter()
                .filter(|stored_recipient| stored_recipient.recipient != recipient)
                .collect::<Vec<Recipient>>())
        },
    )?;

    Ok(Response::new().add_attributes(vec![
        attr("method", "remove_recipient"),
        attr("Recipient", recipient),
    ]))
}

//Mint and stake initial allocation
fn mint_initial_allocation(env: Env, config: Config) -> Result<Response, ContractError> {
    let mut messages: Vec<CosmosMsg> = vec![];

    //Mint token msg in Osmosis Proxy
    messages.push(CosmosMsg::Wasm(WasmMsg::Execute {
        contract_addr: config.osmosis_proxy.to_string(),
        msg: to_binary(&OsmoExecuteMsg::MintTokens {
            denom: config.clone().mbrn_denom,
            amount: config.total_allocation,
            mint_to_address: env.contract.address.to_string(),
        })?,
        funds: vec![],
    }));

    //Stake msg to Staking contract
    messages.push(CosmosMsg::Wasm(WasmMsg::Execute {
        contract_addr: config.staking_contract.to_string(),
        msg: to_binary(&StakingExecuteMsg::Stake { user: None })?,
        funds: vec![coin(config.total_allocation.u128(), config.mbrn_denom)],
    }));

    Ok(Response::new().add_messages(messages).add_attributes(vec![
        attr("action", "mint_initial_allocation"),
        attr("allocation", config.total_allocation.to_string()),
    ]))
}


#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => to_binary(&CONFIG.load(deps.storage)?),
        QueryMsg::Allocation { recipient } => to_binary(&query_allocation(deps, recipient)?),
        QueryMsg::UnlockedTokens { recipient } => to_binary(&query_unlocked(deps, env, recipient)?),
        QueryMsg::Recipients {} => to_binary(&query_recipients(deps)?),
        QueryMsg::Recipient { recipient } => to_binary(&query_recipient(deps, recipient)?),
    }
}


//Helper functions
pub fn withdrawal_msg(asset: Asset, recipient: Addr) -> StdResult<CosmosMsg> {
    match asset.clone().info {
        AssetInfo::Token { address } => {
            let message = CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: address.to_string(),
                msg: to_binary(&Cw20ExecuteMsg::Transfer {
                    recipient: recipient.to_string(),
                    amount: asset.amount,
                })?,
                funds: vec![],
            });
            Ok(message)
        }
        AssetInfo::NativeToken { denom: _ } => {
            let coin: Coin = asset_to_coin(asset)?;
            let message = CosmosMsg::Bank(BankMsg::Send {
                to_address: recipient.to_string(),
                amount: vec![coin],
            });
            Ok(message)
        }
    }
}

pub fn asset_to_coin(asset: Asset) -> StdResult<Coin> {
    match asset.info {
        //
        AssetInfo::Token { address: _ } => {
            Err(StdError::GenericErr {
                msg: String::from("CW20 Assets can't be converted into Coin"),
            })
        }
        AssetInfo::NativeToken { denom } => Ok(Coin {
            denom,
            amount: asset.amount,
        }),
    }
}