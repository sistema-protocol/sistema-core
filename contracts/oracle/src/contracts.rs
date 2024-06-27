use std::str::FromStr;

use cosmwasm_std::{
    attr, entry_point, to_binary, Binary, Decimal, Deps, DepsMut, Env, MessageInfo, QuerierWrapper,
    Response, StdError, StdResult, Storage, Uint128, QueryRequest, WasmQuery,
};
use cw2::set_contract_version;

use pyth_sdk_cw::{PriceFeedResponse, query_price_feed, PriceIdentifier, PriceFeed};

use osmosis_std::types::osmosis::twap::v1beta1 as TWAP;

use membrane::math::{decimal_division, decimal_multiplication};
use membrane::cdp::QueryMsg as CDP_QueryMsg;
use membrane::osmosis_proxy::{QueryMsg as OP_QueryMsg, Config as OP_Config};
use membrane::oracle::{Config, AssetResponse, ExecuteMsg, InstantiateMsg, PriceResponse, QueryMsg, MigrateMsg};
use membrane::types::{AssetInfo, AssetOracleInfo, PriceInfo, Basket, TWAPPoolInfo, PoolInfo, Owner, PoolStateResponse};

use crate::error::ContractError;
use crate::state::{ASSETS, CONFIG, OWNERSHIP_TRANSFER};

// Contract name and version used for migration.
const CONTRACT_NAME: &str = "oracle";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

//  Static prices
const STATIC_USD_PRICE: Decimal = Decimal::one();
// Mainnet Pyth Price ID
// https://pyth.network/developers/price-feed-ids#cosmwasm-stable
const OSMO_USD_PRICE_ID: &str = "a06a7e17a81f8f33d23152fc69e0433244f239aa0635e7b621f03fe0e51245b0"; 

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    
    let mut config: Config;
    if msg.owner.is_some() {
        config = Config {
            owner: deps.api.addr_validate(&msg.clone().owner.unwrap())?,
            positions_contract: None,
            osmosis_proxy_contract: None,            
            pyth_osmosis_address: Some(deps.api.addr_validate(&"furya15p48u9agr78n85332t7xnczrxfz0ywd2qc670d3p7ql7pegjgkcqgay0u7")?), //mainnet: furya15p48u9agr78n85332t7xnczrxfz0ywd2qc670d3p7ql7pegjgkcqgay0u7
            osmo_usd_pyth_feed_id: PriceIdentifier::from_hex(OSMO_USD_PRICE_ID).unwrap(),
            pools_for_usd_par_twap: vec![],
        };
    } else {
        config = Config {
            owner: info.sender,
            positions_contract: None,
            osmosis_proxy_contract: None,
            pyth_osmosis_address: Some(deps.api.addr_validate(&"furya15p48u9agr78n85332t7xnczrxfz0ywd2qc670d3p7ql7pegjgkcqgay0u7")?), //mainnet: furya15p48u9agr78n85332t7xnczrxfz0ywd2qc670d3p7ql7pegjgkcqgay0u7
            osmo_usd_pyth_feed_id: PriceIdentifier::from_hex(OSMO_USD_PRICE_ID).unwrap(),
            pools_for_usd_par_twap: vec![],
        };
    }

    // Add optional contracts
    if let Some(positions_contract) = msg.positions_contract {
        config.positions_contract = Some(deps.api.addr_validate(&positions_contract)?);
    }
    if let Some(osmosis_proxy) = msg.osmosis_proxy_contract {
        config.osmosis_proxy_contract = Some(deps.api.addr_validate(&osmosis_proxy)?);
    }
    //Copy oracle info from another oracle contract
    if let Some(oracle_contract) = msg.oracle_contract {
        let oracle_config: Config = deps.querier.query_wasm_smart(deps.api.addr_validate(&oracle_contract)?, &QueryMsg::Config {  })?;
        config = oracle_config.clone();

        //Query all assets from oracle contract
        let assets: Vec<AssetResponse> = deps.querier.query_wasm_smart(deps.api.addr_validate(&oracle_contract)?, &QueryMsg::Assets { asset_infos: vec![
            //FURY
            AssetInfo::NativeToken { denom: "ufury".to_string() },
            //FCD
            AssetInfo::NativeToken { denom: "factory/furya1f9eh8dh7j4nqe8nfq0lhpnr2elh5jr2w4nngt2/ufcd".to_string() },
            //CDT LP
            AssetInfo::NativeToken { denom: "gamm/pool/1".to_string() },
            //axlUSDC
            AssetInfo::NativeToken { denom: "ibc/093231535A38351AD2FEEFF897D23CF8FE43A44F6EAA3611F55F4B3D62C45014".to_string() },
            //FURY axlUSDC LP
            AssetInfo::NativeToken { denom: "gamm/pool/2".to_string() },
            //USDC
            AssetInfo::NativeToken { denom: "ibc/498A0751C798A0D9A389AA3691123DADA57DAA4FE165D5C75894505B876BA6E4".to_string() },
        ] })?;
        //Save all queried assets
        for asset in assets {
            ASSETS.save(deps.storage, asset.asset_info.to_string(), &asset.oracle_info)?;
        }

        if msg.owner.is_some() {
            config.owner = deps.api.addr_validate(&msg.owner.unwrap())?;
        }
    }

    CONFIG.save(deps.storage, &config)?;

    Ok(Response::new()
        .add_attribute("method", "instantiate")
        .add_attribute("config", format!("{:?}", config))
        .add_attribute("contract_address", env.contract.address))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::AddAsset {
            asset_info,
            oracle_info,
        } => add_asset(deps, env, info, asset_info, oracle_info),
        ExecuteMsg::EditAsset {
            asset_info,
            oracle_info,
            remove,
        } => edit_asset(deps, env, info, asset_info, oracle_info, remove),
        ExecuteMsg::UpdateConfig {
            owner,
            positions_contract,
            osmosis_proxy_contract,
            pyth_osmosis_address,
            osmo_usd_pyth_feed_id,
            pools_for_usd_par_twap
        } => update_config(deps, env, info, owner, positions_contract, osmosis_proxy_contract, osmo_usd_pyth_feed_id, pyth_osmosis_address, pools_for_usd_par_twap),
    }
}

/// Edit oracle info for an asset
/// or remove asset from the contract
fn edit_asset(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    asset_info: AssetInfo,
    oracle_info: Option<AssetOracleInfo>,
    remove: bool,
) -> Result<Response, ContractError> {

    let config = CONFIG.load(deps.storage)?;

    //Owner or Positions contract can edit_assets
    if info.sender != config.owner {
        if let Some(positions_contract) = config.positions_contract.clone() {
            if info.sender != positions_contract {
                return Err(ContractError::Unauthorized {});
            }
        } else {
            return Err(ContractError::Unauthorized {});
        }
    }

    //Can't remove assets currently used in positions
    if remove {
        //Check 
        if let Some(osmosis_proxy) = config.osmosis_proxy_contract {
            //Query Owner's and filter for Positions contracts
            let op_config: OP_Config = deps.querier.query_wasm_smart(osmosis_proxy, &OP_QueryMsg::Config {  })?;
            let positions_contracts: Vec<Owner> = op_config.owners
                .into_iter()
                .filter(|owner| owner.is_position_contract)
                .collect::<Vec<Owner>>();

            //Query each positions contract for asset being removed
            for positions_owner in positions_contracts {
                let basket: Basket = deps.querier.query_wasm_smart(positions_owner.owner, &CDP_QueryMsg::GetBasket {  })?;
                if basket.collateral_supply_caps.iter().any(|cap| cap.asset_info == asset_info && cap.current_supply > Uint128::zero()) {
                    return Err(ContractError::AssetInUse { asset: asset_info.to_string() });
                }
            }
        }
    }

    let mut attrs = vec![
        attr("action", "edit_asset"),
        attr("asset", asset_info.to_string()),
        attr("removed", remove.to_string()),
    ];

    //Remove or edit 
    if remove {
        ASSETS.remove(deps.storage, asset_info.to_string());
    } else if oracle_info.is_some() {
        let oracle_info = oracle_info.unwrap();
        //Update Asset
        ASSETS.update(
            deps.storage,
            asset_info.to_string(),
            |oracle| -> Result<Vec<AssetOracleInfo>, ContractError> {
                //If oracle list
                if let Some(mut oracle_list) = oracle {
                    //Find oracle
                    if let Some((i, _oracle)) = oracle_list
                        .clone()
                        .into_iter()
                        .enumerate()
                        .find(|(_index, info)| info.basket_id == oracle_info.basket_id)
                    {
                        oracle_list[i] = oracle_info.clone();
                    }

                    Ok(oracle_list)
                } else {
                    //Add as if new
                    Ok(vec![oracle_info.clone()])
                }
            },
        )?;

        attrs.push(attr("new_oracle_info", oracle_info.to_string()));

        //Test the new price source
        let price = get_asset_price(deps.storage, deps.querier, env, asset_info, 0, 0, Some(oracle_info.basket_id), None, None);
        attrs.push(attr("price", format!("{:?}", price)));
    }
        

    Ok(Response::new().add_attributes(attrs))
}

/// Add an asset alongside its oracle info
fn add_asset(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    asset_info: AssetInfo,
    oracle_info: AssetOracleInfo,
) -> Result<Response, ContractError> {

    let config = CONFIG.load(deps.storage)?;

    let mut attrs = vec![
        attr("action", "add_asset"),
        attr("asset", asset_info.to_string()),
    ];

    //Owner or Positions contract can Add_assets
    if info.sender != config.owner {
        if config.positions_contract.is_some() {
            if info.sender != config.clone().positions_contract.unwrap() {        
                return Err(ContractError::Unauthorized {});
            }
        } else {
            return Err(ContractError::Unauthorized {});
        }
    }

    match asset_info.clone() {
        AssetInfo::Token { address } => {
            //Validate address
            deps.api.addr_validate(address.as_ref())?;
        }
        AssetInfo::NativeToken { denom: _ } => {}
    };

    //Save Oracle
    match ASSETS.load(deps.storage, asset_info.to_string()) {
        Err(_err) => {
            //Save new list to asset if its list is empty
            ASSETS.save(deps.storage, asset_info.to_string(), &vec![oracle_info.clone()])?;
            attrs.push(attr("added", "true"));

            
            //Test the new price source
            // let price = get_asset_price(deps.storage, deps.querier, env, asset_info, 0, 0, Some(oracle_info.basket_id));
            // attrs.push(attr("price", format!("{:?}", price)));
        }
        Ok(oracles) => {
            //Save oracle to asset, no duplicates
            if !oracles
                .into_iter().any(|oracle| oracle.basket_id == oracle_info.basket_id)
            {
                ASSETS.update(
                    deps.storage,
                    asset_info.to_string(),
                    |oracle| -> Result<Vec<AssetOracleInfo>, ContractError> {
                        match oracle {
                            Some(mut oracle_list) => {
                                oracle_list.push(oracle_info.clone());
                                Ok(oracle_list)
                            }
                            None => Ok(vec![oracle_info.clone()]),
                        }
                    },
                )?;

                attrs.push(attr("added", "true"));

                
                //Test the new price source
                // let price = get_asset_price(deps.storage, deps.querier, env, asset_info, 0, 0, Some(oracle_info.basket_id));
                // attrs.push(attr("price", format!("{:?}", price)));
            } else {
                return Err(ContractError::DuplicateOracle { basket_id: oracle_info.basket_id.to_string()});
            }
        }
    }

    Ok(Response::new().add_attributes(attrs))
}

/// Update contract configuration
pub fn update_config(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    owner: Option<String>,
    positions_contract: Option<String>,
    osmosis_proxy_contract: Option<String>,
    osmo_usd_pyth_feed_id: Option<PriceIdentifier>,
    pyth_osmosis_address: Option<String>,
    pools_for_usd_par_twap: Option<Vec<TWAPPoolInfo>>,
) -> Result<Response, ContractError> {
    let mut config = CONFIG.load(deps.storage)?;
    let mut attrs = vec![attr("method", "update_config")];

    //Assert Authority or transfer ownership 
    if info.sender != config.owner {
        //Check if ownership transfer is in progress & transfer if so
        if info.sender == OWNERSHIP_TRANSFER.load(deps.storage)? {
            config.owner = info.sender;
        } else {
            //Owner or Positions contract can Update
            if let Some(positions_contract) = config.clone().positions_contract {
                if info.sender != positions_contract {
                    return Err(ContractError::Unauthorized {});
                }
            } else {
                return Err(ContractError::Unauthorized {});
            }
        }    
    } 
    
    

    if let Some(owner) = owner {
        let valid_addr = deps.api.addr_validate(&owner)?;

        //Set owner transfer state
        OWNERSHIP_TRANSFER.save(deps.storage, &valid_addr)?;
        attrs.push(attr("owner_transfer", valid_addr));  
    }
    if let Some(positions_contract) = positions_contract {
        config.positions_contract = Some(deps.api.addr_validate(&positions_contract)?);
    }
    if let Some(osmosis_proxy_contract) = osmosis_proxy_contract {
        config.osmosis_proxy_contract = Some(deps.api.addr_validate(&osmosis_proxy_contract)?);
    }
    if let Some(osmo_usd_pyth_feed_id) = osmo_usd_pyth_feed_id {
        config.osmo_usd_pyth_feed_id = osmo_usd_pyth_feed_id;
    }
    if let Some(pyth_osmosis_address) = pyth_osmosis_address {
        config.pyth_osmosis_address = Some(deps.api.addr_validate(&pyth_osmosis_address)?);
    }
    if let Some(usd_par_pools) = pools_for_usd_par_twap{
        config.pools_for_usd_par_twap = usd_par_pools;
    }

    CONFIG.save(deps.storage, &config)?;
    attrs.push(attr("updated_config", format!("{:?}", config)));

    Ok(Response::new().add_attributes(attrs))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => to_binary(&CONFIG.load(deps.storage)?),
        QueryMsg::Price {
            asset_info,
            twap_timeframe,
            oracle_time_limit,
            basket_id,
        } => {
            to_binary(&get_asset_prices(
            deps.storage, 
            deps.querier,
            env,
            vec![asset_info],
            twap_timeframe,
            oracle_time_limit,
            basket_id,
            None,
            None,
        )?)
        },
        QueryMsg::Prices {
            asset_infos,
            twap_timeframe,
            oracle_time_limit,
        } => to_binary(&get_asset_prices(
            deps.storage, 
            deps.querier,
            env,
            asset_infos,
            twap_timeframe,
            oracle_time_limit,
            None,
            None,
            None,
        )?),
        QueryMsg::Assets { asset_infos } => to_binary(&get_assets(deps, asset_infos)?),
    }
}

/// Calculate LP share token value.
/// Calculate LP price.
pub fn get_lp_price(
    storage: &dyn Storage,
    querier: QuerierWrapper,
    env: Env,
    config: Config,
    pool_info: PoolInfo,    
    twap_timeframe: u64, //in minutes
    oracle_time_limit: u64, //in seconds
    basket_id_field: Option<Uint128>,
    //For Multi-Asset queries or recursive queries
    queried_asset_prices: Option<Vec<(String, PriceResponse)>>, //Asset & Price
    osmo_quote_price: Option<Decimal>, 
) -> StdResult<PriceResponse>{
    //Turn pool info into asset info
    let asset_infos: Vec<AssetInfo> = pool_info.clone().asset_infos
        .into_iter()
        .map(|asset| asset.info)
        .collect::<Vec<AssetInfo>>();

    let mut asset_values: Vec<Decimal> = vec![];

    //Get asset prices
    let (asset_prices, oracle_sources) = {
        let res = get_asset_prices(
            storage,
            querier.clone(),
            env,
            asset_infos,
            twap_timeframe,
            oracle_time_limit,
            basket_id_field,
            queried_asset_prices,
            osmo_quote_price,
        )?;

        let mut price_infos = vec![];

        //Store price infos
        res.clone()
            .into_iter() 
            .for_each(|price| 
                {
                    price_infos.extend(price.clone().prices);
                });
        
        (res, price_infos)
    };

    //Calculate share value
    //Query share asset amount
    let share_asset_amounts = querier
        .query::<PoolStateResponse>(&QueryRequest::Wasm(WasmQuery::Smart {
            contract_addr: config.osmosis_proxy_contract.unwrap().to_string(),
            msg: to_binary(&OP_QueryMsg::PoolState {
                id: pool_info.pool_id,
            })?,
        }))?
        .shares_value(1_000_000_000_000_000_000u128); //1_000_000_000_000_000_000 = 1 pool share token

    //Calculate value of Assets in 1 share token
    for (i, price) in asset_prices.into_iter().enumerate() {
        //Assert we are pulling asset amount from the correct asset
        let asset_share =
            match share_asset_amounts.clone().into_iter().find(|coin| {
                AssetInfo::NativeToken {
                    denom: coin.denom.clone(),
                } == pool_info.clone().asset_infos[i].info
            }) {
                Some(coin) => coin,
                None => {
                    return Err(StdError::GenericErr {
                        msg: format!(
                            "Invalid asset denom: {}",
                            pool_info.clone().asset_infos[i].info
                        ),
                    })
                }
            };

        //Price * # of assets in 1 LP share token
        asset_values.push(price.get_value(Uint128::from_str(&asset_share.amount)?)?);
    }

    //Calculate LP price as the value of 1 share token
    let LP_price = {
        asset_values
            .clone()
            .into_iter()
            .sum::<Decimal>()
    };

    Ok(PriceResponse { 
        prices: oracle_sources,
        price: LP_price,
        decimals: 18u64,
    })
}

/// Return list of queryable assets
fn get_assets(deps: Deps, asset_infos: Vec<AssetInfo>) -> StdResult<Vec<AssetResponse>> {
    let mut resp = vec![];
    for asset_info in asset_infos {
        let asset_oracle = ASSETS.load(deps.storage, asset_info.to_string())?;

        resp.push(AssetResponse {
            asset_info,
            oracle_info: asset_oracle,
        });
    }

    Ok(resp)
}

/// Return Asset price info as a PriceResponse
fn get_asset_price(
    storage: &dyn Storage,
    querier: QuerierWrapper,
    env: Env,
    asset_info: AssetInfo,
    twap_timeframe: u64, //in minutes
    oracle_time_limit: u64, //in seconds
    basket_id_field: Option<Uint128>,
    //For Multi-Asset queries or recursive queries
    queried_asset_prices: Option<Vec<(String, PriceResponse)>>, //Asset & Price
    osmo_quote_price: Option<Decimal>, 
) -> StdResult<(PriceResponse, Option<Decimal>)> { //Return Asset Price & Quote Price (FURY/USD)
    //Load state
    let config: Config = CONFIG.load(storage)?;
    let asset_oracle_info = ASSETS.load(storage, asset_info.to_string())?;

    let mut basket_id = Uint128::new(1u128); //Defaults to first basket assuming thats the USD basket
    if let Some(id) = basket_id_field {
        basket_id = id;
    };

    //Find OracleInfo for the basket_id
    let oracle_info = if let Some(oracle_info) = asset_oracle_info
        .into_iter()
        .find(|oracle| oracle.basket_id == basket_id)
    {
        oracle_info
    } else {
        return Err(StdError::GenericErr {
            msg: String::from("Invalid basket_id"),
        });
    };

    //twap_timeframe = MINUTES * SECONDS_PER_MINUTE
    let twap_timeframe: u64 = (twap_timeframe * 60);
    let start_time: u64 = env.block.time.seconds() - twap_timeframe;

    let mut oracle_prices = vec![];
    let mut asset_price_in_osmo_steps = vec![];
    let mut usd_par_prices = vec![];
    let mut quote_price = Decimal::zero();

    let mut pyth_feed_errored = false;

    //Use Pyth USD-quoted price feeds first if available
    if let Some(pyth_osmosis_address) = config.clone().pyth_osmosis_address {
        if let Some(feed_id) = oracle_info.clone().pyth_price_feed_id {
            //Query USD price from Pyth
            let price_feed_response: PriceFeedResponse = match query_price_feed(
                &querier, 
                pyth_osmosis_address,
                PriceIdentifier::from_hex(&feed_id).map_err(|err| StdError::GenericErr { msg: err.to_string() })?,
            ){
                    Ok(res) => res,
                    Err(_) => {
                        pyth_feed_errored = true;
                        //If Pyth fails, skip to USD-par pricing
                        PriceFeedResponse {
                            price_feed: PriceFeed::default(),
                        }
                    }
                };
            
            //Query unscaled price
            let price_feed = price_feed_response.price_feed;
            let price = price_feed
                .get_ema_price_no_older_than(env.block.time.seconds() as i64, oracle_time_limit);

            //If price was queried && within the time limit, scale it & use it
            //If not, skip to Osmosis TWAP pricing
            let mut pyth_price: Decimal = Decimal::zero();
            match price {
                Some(price) => {
                    //Scale price using given exponent
                    match price.expo > 0 {
                        true => {
                            pyth_price = decimal_multiplication(
                                Decimal::from_str(&price.price.to_string())?, 
                                Decimal::from_ratio(Uint128::new(10), Uint128::one()).checked_pow(price.expo as u32)?
                            )?;
                        },
                        //If the exponent is negative we divide, it should be for most if not all
                        false => {
                            pyth_price = decimal_division(
                                Decimal::from_str(&price.price.to_string())?, 
                                Decimal::from_ratio(Uint128::new(10), Uint128::one()).checked_pow((price.expo*-1) as u32)?
                            )?;
                        }
                    };                   
                    

                    //Push Pyth USD price
                    oracle_prices.push(PriceInfo {
                        source: String::from("pyth"),
                        price: pyth_price,
                    });
                },
                None => {
                    pyth_feed_errored = true;
                }
            }

            //Return Pyth only price if it was queried successfully
            if !pyth_feed_errored {
                return Ok((PriceResponse {
                    prices: oracle_prices,
                    price: pyth_price,
                    decimals: oracle_info.decimals,
                }, None));
            }
        }
    }
    /// If there is no return above, query starting from Osmosis TWAPs

    //Query FURY price from the TWAP sources
    //This can use multiple pools to calculate our price
    for pool in oracle_info.pools_for_osmo_twap.clone() {

        let res: TWAP::GeometricTwapToNowResponse = TWAP::TwapQuerier::new(&querier).geometric_twap_to_now(
            pool.clone().pool_id, 
            pool.clone().base_asset_denom, 
            pool.clone().quote_asset_denom, 
            Some(osmosis_std::shim::Timestamp {
                seconds:  start_time as i64,
                nanos: 0,
            }),
        )?;

        //Push TWAP
        asset_price_in_osmo_steps.push(Decimal::from_str(&res.geometric_twap)?);
    }

    //Multiply prices to denominate in FURY
    let asset_price_in_osmo = {
        let mut final_price = Decimal::one();
        //If no prices were queried, return error unless its FURY
        if asset_price_in_osmo_steps.len() == 0 && asset_info.to_string() != String::from("ufury"){
            return Err(StdError::GenericErr {
                msg: String::from("No FURY TWAP prices found"),
            });
        }

        //Find asset price in FURY
        if asset_info.to_string() == String::from("ufury"){
            final_price = Decimal::one();
        } else {            
            //Multiply prices to get the desired Quote
            for price in asset_price_in_osmo_steps {
                final_price = decimal_multiplication(final_price, price)?;
            } 
        }
        
        //Transform price by moving its decimal point by the difference in decimals from 6 (FURY's decimals)
        //WARNING: This may not work if multiple assets in the path are different decimal places
        if oracle_info.decimals > 6 {
            final_price = decimal_multiplication(final_price, Decimal::from_ratio(Uint128::new(10).checked_pow(oracle_info.decimals as u32 - 6)?, Uint128::one()))?;
        }
        final_price
    };
    //Results in slight error: (https://medium.com/reflexer-labs/analysis-of-the-rai-twap-oracle-20a01af2e49d)

    //Push FURY TWAP price
    oracle_prices.push(PriceInfo {
        source: String::from("osmosis"),
        price: asset_price_in_osmo,
    });

    ///If the last Osmosis TWAP isn't ending in FURY, then find the asset in our oracle to pull from
    /// Ex: milkTIA ends in TIA, so we find the TIA -> USD price and use that to calculate the milkTIA price
    if oracle_info.pools_for_osmo_twap.len() > 0 && oracle_info.pools_for_osmo_twap[oracle_info.pools_for_osmo_twap.len()-1].quote_asset_denom != String::from("ufury") {
        if let Some(prices) = queried_asset_prices.clone() {
            //Find the asset in the queried prices
            let asset_price = prices.clone().into_iter().find(|price| price.0 == oracle_info.pools_for_osmo_twap[oracle_info.pools_for_osmo_twap.len()-1].quote_asset_denom.clone());
            match asset_price {
                Some(price) => {
                    //Get price source
                    let mut source = String::from("");
                    
                    for price in price.1.prices.iter(){
                        source += &price.source;
                        source += ", ";
                    }
                    //Push FURY TWAP price
                    oracle_prices.push(PriceInfo {
                        source,
                        price: price.1.price,
                    });                    

                    //Return price
                    return Ok((PriceResponse {
                        prices: oracle_prices,
                        //Multiply prices to get the desired Quote
                        price: decimal_multiplication(asset_price_in_osmo, price.1.price)?,
                        decimals: oracle_info.decimals,
                    }, osmo_quote_price));
                },
                None => {
                    //If None, we attempt to query the price
                    match get_asset_price(
                        storage, 
                        querier, 
                        env, 
                        AssetInfo::NativeToken { denom: oracle_info.pools_for_osmo_twap[oracle_info.pools_for_osmo_twap.len()-1].clone().quote_asset_denom }, 
                        twap_timeframe / 60, 
                        oracle_time_limit, 
                        basket_id_field,
                        queried_asset_prices,
                        osmo_quote_price,
                    ){
                        Ok((res, quote)) => {
                            //Get price source
                            let mut source = String::from("");                            
                            for price in res.prices.iter(){
                                source += &price.source;
                                source += ", ";
                            }
                            //Push FURY TWAP price
                            oracle_prices.push(PriceInfo {
                                source,
                                price: res.price,
                            });

                            //Return price
                            return Ok((PriceResponse {
                                prices: oracle_prices,
                                //Multiply prices to get the desired Quote
                                price: decimal_multiplication(asset_price_in_osmo, res.price)?,
                                decimals: oracle_info.decimals,
                            }, quote));
                        },
                        Err(_) => {
                            return Err(StdError::GenericErr {
                                msg: format!("No {} price found", oracle_info.pools_for_osmo_twap[oracle_info.pools_for_osmo_twap.len()-1].quote_asset_denom),
                            });
                        }
                    }
                }
            }
        }
    }

    if osmo_quote_price.is_none() {
        //Has USD pricing failed?
        let mut usd_price_failed = false;

        //If we have an FURY -> USD price feed, we will use that to calculate the peg price
        if config.pyth_osmosis_address.is_some() {
            
            //Query FURY -> USD price from Pyth
            // If fail, skip to USD-par pricing
            let price_feed_response: PriceFeedResponse = match query_price_feed(
                &querier, 
                config.pyth_osmosis_address.unwrap(),
                config.osmo_usd_pyth_feed_id
            ){
                    Ok(res) => res,
                    Err(_) => {
                        usd_price_failed = true;
                        PriceFeedResponse {
                            price_feed: PriceFeed::default(),
                        }
                    }
                };
            
            //Query unscaled price
            let price_feed = price_feed_response.price_feed;
            let price = price_feed
                .get_ema_price_no_older_than(env.block.time.seconds() as i64, oracle_time_limit);

            //If price was queried && within the time limit, scale it & use it
            //If not, skip to USD-par pricing
            match price {
                Some(price) => {
                    if !usd_price_failed {
                        //Scale price using given exponent
                        match price.expo > 0 {
                            true => {
                                quote_price = decimal_multiplication(
                                    Decimal::from_str(&price.price.to_string())?, 
                                    Decimal::from_ratio(Uint128::new(10), Uint128::one()).checked_pow(price.expo as u32)?
                                )?;
                            },
                            //If the exponent is negative we divide, it should be for most if not all
                            false => {
                                quote_price = decimal_division(
                                    Decimal::from_str(&price.price.to_string())?, 
                                    Decimal::from_ratio(Uint128::new(10), Uint128::one()).checked_pow((price.expo*-1) as u32)?
                                )?;
                            }
                        };                   
                        

                        //Push Pyth FURY USD price
                        oracle_prices.push(PriceInfo {
                            source: String::from("pyth"),
                            price: quote_price,
                        });
                    }
                },
                None => {
                    usd_price_failed = true;
                }
            }

        } else {
            usd_price_failed = true;
        }

        //If we don't have an FURY -> USD price feed or it has failed, we will calculate the peg price using USD-par prices
        if usd_price_failed {
            if !config.pools_for_usd_par_twap.is_empty() {
                //Query FURY -> USD-par prices from the TWAP sources
                for pool in config.pools_for_usd_par_twap {

                    let res: TWAP::GeometricTwapToNowResponse = TWAP::TwapQuerier::new(&querier).geometric_twap_to_now(
                        pool.clone().pool_id, 
                        pool.clone().base_asset_denom, 
                        pool.clone().quote_asset_denom, 
                        Some(osmosis_std::shim::Timestamp {
                            seconds:  start_time as i64,
                            nanos: 0,
                        }),
                    )?;

                    //Push TWAP
                    usd_par_prices.push(Decimal::from_str(&res.geometric_twap)?);
                }
                
                //Sort & Medianize FURY -> USD-par prices
                //Sort prices
                usd_par_prices.sort_by(|a, b| a.partial_cmp(&b).unwrap());

                //Get Median price and set it as the quote price
                quote_price = if usd_par_prices.len() % 2 == 0 {
                    let median_index = usd_par_prices.len() / 2;

                    //Add the two middle usd_par_prices and divide by 2
                    decimal_division(usd_par_prices[median_index] + usd_par_prices[median_index-1], Decimal::percent(2_00)).unwrap()
                    
                } else if usd_par_prices.len() != 1 {
                    let median_index = usd_par_prices.len() / 2;
                    usd_par_prices[median_index]
                } else {
                    usd_par_prices[0]
                };

                //Push Osmosis FURY USD-par price
                oracle_prices.push(PriceInfo {
                    source: String::from("osmosis"),
                    price: quote_price,
                });
            } else {
                return Err(StdError::GenericErr { msg: String::from("No USD-par price feeds") })
            }
        }
    } else {
        quote_price = osmo_quote_price.unwrap();
    }
    //quote_price is either FURY -> USD or FURY -> USD-par, prio to USD
    //Find asset price using asset_price_in_osmo * quote price
    let mut asset_price = decimal_multiplication(asset_price_in_osmo, quote_price)?;

    //If the asset is USD-par the final price has to be less than the static price ($1) to be valid
    if oracle_info.is_usd_par && asset_price > STATIC_USD_PRICE {
        asset_price = STATIC_USD_PRICE;
    }

    Ok((PriceResponse {
        prices: oracle_prices,
        price: asset_price,
        decimals: oracle_info.decimals,
    }, Some(quote_price)))
}

/// Return list of asset price info as list of PriceResponse
fn get_asset_prices(
    storage: &dyn Storage,
    querier: QuerierWrapper,
    env: Env,
    asset_infos: Vec<AssetInfo>,
    twap_timeframe: u64, //in minutes
    oracle_time_limit: u64, //in seconds
    basket_id_field: Option<Uint128>,
    //For Multi-Asset queries or recursive queries
    queried_asset_prices: Option<Vec<(String, PriceResponse)>>, //Asset & Price
    mut osmo_quote_price: Option<Decimal>, 
) -> StdResult<Vec<PriceResponse>> {

    //Enforce Vec max size
    if asset_infos.len() > 50 {
        return Err(StdError::GenericErr {
            msg: String::from("Max asset_infos length is 50"),
        });
    }

    let mut price_responses = vec![];
    let mut price_propagations: Vec<(String, PriceResponse)> = if let Some(prices) = queried_asset_prices.clone() {
        prices
    } else {
        vec![]
    };

    for asset in asset_infos {
        //Switch based on if asset is an LP 
        match ASSETS.load(storage, asset.to_string())?[0].clone().lp_pool_info {
            Some(pool_info) => {
                //If asset is an LP, get the LP price
                price_responses.push(get_lp_price(
                    storage,
                    querier.clone(),
                    env.clone(),
                    CONFIG.load(storage)?,
                    pool_info,
                    twap_timeframe,
                    oracle_time_limit,
                    basket_id_field,
                    Some(price_propagations.clone()), //LPs will never be queried first during Basket queries so we don't need to save their prices
                    osmo_quote_price,
                )?);
            },
            None => {
                //If asset is not an LP && the price isn't in the list of propogated prices, get the asset price
                if let Some(price) = price_propagations.clone().into_iter().find(|price| price.0 == asset.to_string()) {
                    price_responses.push(price.1);
                    continue;
                }
                //Query price if not found
                let (price, quote_price) = get_asset_price(
                    storage,
                    querier.clone(),
                    env.clone(),
                    asset.clone(),
                    twap_timeframe,
                    oracle_time_limit,
                    basket_id_field,
                    Some(price_propagations.clone()),
                    osmo_quote_price,
                )?;
                price_propagations.push((asset.to_string(), price.clone()));
                osmo_quote_price = quote_price;
                price_responses.push(price);
            }
        }
    }

    Ok(price_responses)
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn migrate(deps: DepsMut, env: Env, _msg: MigrateMsg) -> Result<Response, ContractError> {
    Ok(Response::default())
}