use crate::contracts::{execute, instantiate, query, SECONDS_PER_DAY};
use crate::error::ContractError;

use cosmwasm_std::testing::{mock_dependencies, mock_env, mock_info};
use cosmwasm_std::{
    attr, coin, coins, from_binary, to_binary, BankMsg, CosmosMsg, SubMsg, Uint128,
    WasmMsg, Addr
};

use membrane::staking::ExecuteMsg as StakingExecuteMsg;
use membrane::osmosis_proxy::ExecuteMsg as OsmoExecuteMsg;
use membrane::launch::{
    Config, ExecuteMsg, InstantiateMsg, QueryMsg, UpdateConfig
};
use membrane::types::{LockedUser, Lock};


#[test]
fn update_config(){

    let mut deps = mock_dependencies();

    let msg = InstantiateMsg {
        pre_launch_contributors: String::from("labs"),
        pre_launch_community: vec![],
        apollo_router: String::from("router"),
        //Contract IDs
        osmosis_proxy_id: 0,
        oracle_id: 0,
        staking_id: 0,
        vesting_id: 0,
        governance_id: 0,
        positions_id: 0,
        stability_pool_id: 0,
        liq_queue_id: 0,
        liquidity_check_id: 0,
        tema_auction_id: 0,
        system_discounts_id: 0,
        discount_vault_id: 0,
    };
    //Instantiating contract
    let info = mock_info("sender88", &[coin(20_000_000, "ufury")]);
    let _res = instantiate(deps.as_mut(), mock_env(), info, msg).unwrap();
    
    let msg = ExecuteMsg::UpdateConfig(UpdateConfig {
        credit_denom: Some(String::from("new_credit_denom")),
        tema_denom: Some(String::from("new_tema_denom")),
        osmo_denom: Some(String::from("new_osmo_denom")),
        usdc_denom: Some(String::from("new_usdc_denom")),
    });

    execute(
        deps.as_mut(),
        mock_env(),
        mock_info("labs", &vec![]),
        msg,
    )
    .unwrap();

    //Query Config
    let res = query(
        deps.as_ref(),
        mock_env(),
        QueryMsg::Config {},
    )
    .unwrap();
    let config: Config = from_binary(&res).unwrap();

    assert_eq!(
        config.tema_denom,        
        String::from("new_tema_denom"),
    );
    assert_eq!(
        config.credit_denom,        
        String::from("new_credit_denom"),
    );
    assert_eq!(
        config.osmo_denom,        
        String::from("new_osmo_denom"),
    );
    assert_eq!(
        config.usdc_denom,    
        String::from("new_usdc_denom"),
    );
}


#[test]
fn lock() {
    let mut deps = mock_dependencies();

    let msg = InstantiateMsg {
        pre_launch_contributors: String::from("labs"),
        pre_launch_community: vec![],
        apollo_router: String::from("router"),
        //Contract IDs
        osmosis_proxy_id: 0,
        oracle_id: 0,
        staking_id: 0,
        vesting_id: 0,
        governance_id: 0,
        positions_id: 0,
        stability_pool_id: 0,
        liq_queue_id: 0,
        liquidity_check_id: 0,
        tema_auction_id: 0,    
        system_discounts_id: 0,
        discount_vault_id: 0,
    };

    //Instantiating contract
    let info = mock_info("sender88", &[coin(20_000_000, "ufury")]);
    let _res = instantiate(deps.as_mut(), mock_env(), info, msg).unwrap();

    //Invalid lock asset
    let msg = ExecuteMsg::Lock { lock_up_duration: 0u64 };
    let info = mock_info("user1", &[coin(10_000_000, "not_ufury")]);
    let err = execute(deps.as_mut(), mock_env(), info, msg).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Generic error: No valid lockdrop asset, looking for ufury".to_string()
    ); 

    //Invalid lock duration
    let msg = ExecuteMsg::Lock { lock_up_duration: 366u64 };
    let info = mock_info("user1", &[coin(10_000_000, "not_ufury")]);
    let err = execute(deps.as_mut(), mock_env(), info, msg).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Custom Error val: Can't lock that long".to_string()
    ); 
    
    //Lock ufury for 7 days
    let msg = ExecuteMsg::Lock { lock_up_duration: 7u64 };
    let info = mock_info("user1", &[coin(10_000_000, "ufury")]);
    let res = execute(deps.as_mut(), mock_env(), info, msg).unwrap();
    assert_eq!(
        res.attributes,
        vec![
            attr("method", "deposit"),
            attr("user", "user1"),
            attr("lock_up_duration", "7"),
            attr("deposit", "10000000 ufury"),
        ]
    ); 

    //Lock ufury for 7 days & assert its added to the same deposit
    let msg = ExecuteMsg::Lock { lock_up_duration: 7u64 };
    let info = mock_info("user1", &[coin(10_000_000, "ufury")]);
    execute(deps.as_mut(), mock_env(), info, msg).unwrap();

    //Query and Assert lock
    let res = query(deps.as_ref(), mock_env(), QueryMsg::UserInfo { user: String::from("user1") }).unwrap();
    let resp: LockedUser = from_binary(&res).unwrap();

    assert_eq!(resp, 
        LockedUser { 
            user: Addr::unchecked("user1"), 
            deposits: vec![
                Lock { 
                    deposit: Uint128::new(20_000_000), 
                    lock_up_duration: 7u64, 
                }],
            total_tickets: Uint128::zero(),
            incentives_withdrawn: Uint128::zero(),
        }
    );
    
    //Error at lock under minimum
    let msg = ExecuteMsg::Lock { lock_up_duration: 7u64 };
    let info = mock_info("user1", &[coin(10, "ufury")]);
    let err = execute(deps.as_mut(), mock_env(), info, msg).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Generic error: Minimum deposit is 1_000_000 ufury".to_string()
    );

    //Lock attempt after deposit period
    let msg = ExecuteMsg::Lock { lock_up_duration: 7u64 };
    let info = mock_info("user1", &[coin(10_000_000, "ufury")]);
    
    let mut env = mock_env();
    env.block.time = env.block.time.plus_seconds(5 * SECONDS_PER_DAY + 1); // 5 days + 1
    let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Deposit period over".to_string()
    ); 

}

#[test]
fn edit_lockup_duration() {
    let mut deps = mock_dependencies();

    let msg = InstantiateMsg {
        pre_launch_contributors: String::from("labs"),
        pre_launch_community: vec![],
        apollo_router: String::from("router"),
        //Contract IDs
        osmosis_proxy_id: 0,
        oracle_id: 0,
        staking_id: 0,
        vesting_id: 0,
        governance_id: 0,
        positions_id: 0,
        stability_pool_id: 0,
        liq_queue_id: 0,
        liquidity_check_id: 0,
        tema_auction_id: 0,    
        system_discounts_id: 0,
        discount_vault_id: 0,
    };

    //Instantiating contract
    let info = mock_info("sender88", &[coin(20_000_000, "ufury")]);
    let _res = instantiate(deps.as_mut(), mock_env(), info, msg).unwrap();
    
    //Lock ufury for 7 days
    let msg = ExecuteMsg::Lock { lock_up_duration: 7u64 };
    let info = mock_info("user1", &[coin(10_000_000, "ufury")]);
    let res = execute(deps.as_mut(), mock_env(), info, msg).unwrap();
    assert_eq!(
        res.attributes,
        vec![
            attr("method", "deposit"),
            attr("user", "user1"),
            attr("lock_up_duration", "7"),
            attr("deposit", "10000000 ufury"),
        ]
    ); 

    //Split lock up duration to a 14 day
    let msg = ExecuteMsg::ChangeLockDuration {
        ufury_amount: Some(Uint128::new(5_000_000)),
        old_lock_up_duration: 7u64,
        new_lock_up_duration: 14u64, 
        };    
    let info = mock_info("user1", &[]);
    execute(deps.as_mut(), mock_env(), info, msg).unwrap();

    //Query and Assert lock
    let res = query(deps.as_ref(), mock_env(), QueryMsg::UserInfo { user: String::from("user1") }).unwrap();
    let resp: LockedUser = from_binary(&res).unwrap();

    assert_eq!(resp, 
        LockedUser { 
            user: Addr::unchecked("user1"), 
            deposits: vec![
                Lock { 
                    deposit: Uint128::new(5_000_000), 
                    lock_up_duration: 7u64, 
                },
                Lock { 
                    deposit: Uint128::new(5_000_000), 
                    lock_up_duration: 14u64, 
                }],
            total_tickets: Uint128::zero(),
            incentives_withdrawn: Uint128::zero(),
        }
    );

    //Change lock up duration to a 30 day
    let msg = ExecuteMsg::ChangeLockDuration {
        ufury_amount: Some(Uint128::new(5_000_001)), //over allo is set to the minimum
        old_lock_up_duration: 7u64,
        new_lock_up_duration: 30u64, 
        };    
    let info = mock_info("user1", &[]);
    execute(deps.as_mut(), mock_env(), info, msg).unwrap();

    //Query and Assert lock
    let res = query(deps.as_ref(), mock_env(), QueryMsg::UserInfo { user: String::from("user1") }).unwrap();
    let resp: LockedUser = from_binary(&res).unwrap();

    assert_eq!(resp, 
        LockedUser { 
            user: Addr::unchecked("user1"), 
            deposits: vec![
                Lock { 
                    deposit: Uint128::new(5_000_000), 
                    lock_up_duration: 14u64, 
                },
                Lock { 
                    deposit: Uint128::new(5_000_000), 
                    lock_up_duration: 30u64, 
                }],
            total_tickets: Uint128::zero(),
            incentives_withdrawn: Uint128::zero(),
        }
    );
    
    //Change attempt after deposit period
    let msg = ExecuteMsg::ChangeLockDuration {
        ufury_amount: Some(Uint128::new(5_000_000)),
        old_lock_up_duration: 7u64,
        new_lock_up_duration: 14u64, 
        };    
    let info = mock_info("user1", &[coin(10_000_000, "ufury")]);
    
    let mut env = mock_env();
    env.block.time = env.block.time.plus_seconds(5 * SECONDS_PER_DAY + 1); // 5 days + 1
    let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Deposit period over".to_string()
    ); 

}


#[test]
fn withdraw() {
    let mut deps = mock_dependencies();

    let msg = InstantiateMsg {
        pre_launch_contributors: String::from("labs"),
        pre_launch_community: vec![],
        apollo_router: String::from("router"),
        //Contract IDs
        osmosis_proxy_id: 0,
        oracle_id: 0,
        staking_id: 0,
        vesting_id: 0,
        governance_id: 0,
        positions_id: 0,
        stability_pool_id: 0,
        liq_queue_id: 0,
        liquidity_check_id: 0,
        tema_auction_id: 0,    
        system_discounts_id: 0,
        discount_vault_id: 0,
    };

    //Instantiating contract
    let info = mock_info("sender88", &[coin(20_000_000, "ufury")]);
    let _res = instantiate(deps.as_mut(), mock_env(), info, msg).unwrap();

    //Lock ufury for 7 days
    let msg = ExecuteMsg::Lock { lock_up_duration: 7u64 };
    let info = mock_info("user1", &[coin(10_000_000, "ufury")]);
    execute(deps.as_mut(), mock_env(), info, msg).unwrap();

    //Withdraw during deposit period: Error
    let msg = ExecuteMsg::Withdraw { withdrawal_amount: Uint128::new(5), lock_up_duration: 7u64 };
    let info = mock_info("user1", &[]);
    let err = execute(deps.as_mut(), mock_env(), info, msg).unwrap_err();
    assert_eq!(
        err,
        ContractError::WithdrawalsOver {  }
    );

    //Withdraw after deposit period: Success
    let msg = ExecuteMsg::Withdraw { withdrawal_amount: Uint128::new(5_000_000), lock_up_duration: 7u64 };
    let info = mock_info("user1", &[]);
    let mut env = mock_env();
    env.block.time = env.block.time.plus_seconds(5 * SECONDS_PER_DAY + 1); // 5 days + 1
    let res = execute(deps.as_mut(), env.clone(), info, msg).unwrap();
    assert_eq!(
        res.attributes,
        vec![
            attr("method", "withdraw"),
            attr("user", "user1"),
            attr("lock_up_duration", "7"),
            attr("withdraw", "5000000"),
        ]
    ); 
    assert_eq!(res.messages, vec![
        SubMsg::new(CosmosMsg::Bank(BankMsg::Send {
            to_address: String::from("user1"),
            amount: coins(5_000_000, "ufury"),
        }))
    ] );    

    //Withdraw as a non-user: Error
    let msg = ExecuteMsg::Withdraw { withdrawal_amount: Uint128::new(5_000_000), lock_up_duration: 7u64 };
    let info = mock_info("non-user", &[]);
    let err = execute(deps.as_mut(), env.clone(), info, msg).unwrap_err();
    assert_eq!(
        err,
        ContractError::NotAUser {  },
    );
    
    //Withdraw more than deposited: Error
    let msg = ExecuteMsg::Withdraw { withdrawal_amount: Uint128::new(11_000_000), lock_up_duration: 7u64 };
    let info = mock_info("user1", &[]);
    let err = execute(deps.as_mut(), env.clone(), info, msg).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Custom Error val: This user only owns 5000000 of the locked asset in this lockup duration: 7, retry withdrawal at or below that amount".to_string()
    );

    //Withdraw after withdraw period: Error
    let msg = ExecuteMsg::Withdraw { withdrawal_amount: Uint128::new(1), lock_up_duration: 7u64 };
    let info = mock_info("user1", &[]);
    let err = execute(deps.as_mut(), mock_env(), info, msg).unwrap_err();
    assert_eq!(
        err,
        ContractError::WithdrawalsOver {  }
    );
}

#[test]
fn claim() {
    let mut deps = mock_dependencies();

    let msg = InstantiateMsg {
        pre_launch_contributors: String::from("labs"),
        pre_launch_community: vec![],
        apollo_router: String::from("router"),
        //Contract IDs
        osmosis_proxy_id: 0,
        oracle_id: 0,
        staking_id: 0,
        vesting_id: 0,
        governance_id: 0,
        positions_id: 0,
        stability_pool_id: 0,
        liq_queue_id: 0,
        liquidity_check_id: 0,
        tema_auction_id: 0,    
        system_discounts_id: 0,
        discount_vault_id: 0,
    };

    //Instantiating contract
    let info = mock_info("sender88", &[coin(20_000_000, "ufury")]);
    let _res = instantiate(deps.as_mut(), mock_env(), info, msg).unwrap();

    //Lock ufury for 7 days
    let msg = ExecuteMsg::Lock { lock_up_duration: 7u64 };
    let info = mock_info("user1", &[coin(10_000_000, "ufury")]);
    execute(deps.as_mut(), mock_env(), info, msg).unwrap();

    //Lock ufury for 14 days
    let msg = ExecuteMsg::Lock { lock_up_duration: 14u64 };
    let info = mock_info("user1", &[coin(10_000_000, "ufury")]);
    execute(deps.as_mut(), mock_env(), info, msg).unwrap();

    //Overload user calculations
    for i in 0..90000{
        //Lock ufury for 7 days
        let msg = ExecuteMsg::Lock { lock_up_duration: 7u64 };
        let info = mock_info(&i.to_string(), &[coin(1_000_000, "ufury")]);
        execute(deps.as_mut(), mock_env(), info, msg).unwrap();
    }    

    //Claim before lockdrop has ended: Error
    let msg = ExecuteMsg::Claim {  };
    let info = mock_info("user1", &[]);
    let err = execute(deps.as_mut(), mock_env(), info, msg).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Custom Error val: Lockdrop hasn't ended yet".to_string()
    );

    
    let mut env = mock_env();
    env.block.time = env.block.time.plus_seconds(7 * SECONDS_PER_DAY + 1); // 7 days + 1sec to end of lockdrop
    
    //Claim as a non-user: Error
    let msg = ExecuteMsg::Claim {  };
    let info = mock_info("non-user", &[]);
    let err = execute(deps.as_mut(), env.clone(), info, msg).unwrap_err();
    assert_eq!(
        err.to_string(),
        String::from("Generic error: User didn't participate in the lockdrop"),
    );

    //Claim before lock time ends: Partial linear mints
    let msg = ExecuteMsg::Claim {  };
    let info = mock_info("user1", &[]);
    let res = execute(deps.as_mut(), env.clone(), info, msg).unwrap();
    assert_eq!(
        res.attributes,
        vec![
            attr("method", "claim"),
            attr("staked_ownership", "3554"),
        ]
    );
    assert_eq!(
        res.messages,
        vec![
            SubMsg::new(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: String::from(""),
                funds: vec![],
                msg: to_binary(&OsmoExecuteMsg::MintTokens {
                    denom: String::from(""),
                    amount: Uint128::new(3554),
                    mint_to_address: String::from("cosmos2contract")
                })
                .unwrap()
            })),
            SubMsg::new(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: String::from(""),
                funds: vec![coin(3554, "")],
                msg: to_binary(&StakingExecuteMsg::Stake {
                    user: Some(String::from("user1"))
                })
                .unwrap()
            }))
        ]
    );


    //Claim after lock time of first deposit: Partial Mint
    let msg = ExecuteMsg::Claim {  };
    let info = mock_info("user1", &[]);
    env.block.time = env.block.time.plus_seconds(7 * SECONDS_PER_DAY); // 7 days + 1sec past the end of lockdrop
    let res = execute(deps.as_mut(), env.clone(), info, msg).unwrap();
    assert_eq!(
        res.attributes,
        vec![
            attr("method", "claim"),
            attr("staked_ownership", "2152088471"), //2_152_088_471
        ]
    );
    assert_eq!(
        res.messages,
        vec![
            SubMsg::new(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: String::from(""),
                funds: vec![],
                msg: to_binary(&OsmoExecuteMsg::MintTokens {
                    denom: String::from(""),
                    amount: Uint128::new(2152088471),
                    mint_to_address: String::from("cosmos2contract")
                })
                .unwrap()
            })),
            SubMsg::new(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: String::from(""),
                funds: vec![coin(2152088471, "")],
                msg: to_binary(&StakingExecuteMsg::Stake {
                    user: Some(String::from("user1"))
                })
                .unwrap()
            }))
        ]
    );

    //Query and Assert incentive tracking
    let res = query(deps.as_ref(), mock_env(), QueryMsg::UserInfo { user: String::from("user1") }).unwrap();
    let resp: LockedUser = from_binary(&res).unwrap();

    assert_eq!(resp, 
        LockedUser { 
            user: Addr::unchecked("user1"), 
            deposits: vec![
                Lock { 
                    deposit: Uint128::new(10_000_000), 
                    lock_up_duration: 7u64, 
                },
                Lock { 
                    deposit: Uint128::new(10_000_000), 
                    lock_up_duration: 14u64, 
                }],
            total_tickets: Uint128::new(230000000),
            incentives_withdrawn: Uint128::new(2152088471+3554),
        }
    );

    //Claim near the end of the lock time of 2nd deposit: Error bc ur leaving below the minimum stake left to unlock & stake
    let msg = ExecuteMsg::Claim {  };
    let info = mock_info("user1", &[]);
    env.block.time = env.block.time.plus_seconds(6 * SECONDS_PER_DAY + 86000);
    let err = execute(deps.as_mut(), env.clone(), info, msg).unwrap_err();
    assert_eq!(err.to_string(), "Custom Error val: If you leave less than 1 TEMA still unlocking, it'll get stuck due to the minimum stake amount".to_string());

    //Claim after lock time of 2nd deposit: Rest of Mint
    let msg = ExecuteMsg::Claim {  };
    let info = mock_info("user1", &[]);
    env.block.time = env.block.time.plus_seconds(7 * SECONDS_PER_DAY); // 14 days + 1sec past the end of lockdrop
    let res = execute(deps.as_mut(), env.clone(), info, msg).unwrap();
    assert_eq!(
        res.attributes,
        vec![
            attr("method", "claim"),
            attr("staked_ownership", "1041332297"),
        ]
    );
    assert_eq!(
        res.messages,
        vec![
            SubMsg::new(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: String::from(""),
                funds: vec![],
                msg: to_binary(&OsmoExecuteMsg::MintTokens {
                    denom: String::from(""),
                    amount: Uint128::new(1041332297), //1_041_332_297
                    mint_to_address: String::from("cosmos2contract")
                })
                .unwrap()
            })),
            SubMsg::new(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: String::from(""),
                funds: vec![coin(1041332297, "")],
                msg: to_binary(&StakingExecuteMsg::Stake {
                    user: Some(String::from("user1"))
                })
                .unwrap()
            }))
        ]
    );

    //Claim after lock time of both deposit after claims: No mint
    let msg = ExecuteMsg::Claim {  };
    let info = mock_info("user1", &[]);
    env.block.time = env.block.time.plus_seconds(7 * SECONDS_PER_DAY); // 21 days + 1sec past the end of lockdrop
    let res = execute(deps.as_mut(), env.clone(), info, msg).unwrap_err();
    assert_eq!(
        res.to_string(),
        "Custom Error val: No incentives to claim".to_string());

    //Query and Assert incentive tracking
    let res = query(deps.as_ref(), mock_env(), QueryMsg::UserInfo { user: String::from("user1") }).unwrap();
    let resp: LockedUser = from_binary(&res).unwrap();

    assert_eq!(resp, 
        LockedUser { 
            user: Addr::unchecked("user1"), 
            deposits: vec![
                Lock { 
                    deposit: Uint128::new(10_000_000), 
                    lock_up_duration: 7u64, 
                },
                Lock { 
                    deposit: Uint128::new(10_000_000), 
                    lock_up_duration: 14u64, 
                }],
            total_tickets: Uint128::new(230000000),
            incentives_withdrawn: Uint128::new(3193424322), //3_193_424_322
        }
    );
}


