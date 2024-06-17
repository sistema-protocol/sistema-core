use std::str::FromStr;

use crate::contract::{execute, instantiate, query};

use membrane::liq_queue::{BidResponse, ExecuteMsg, InstantiateMsg, QueryMsg};
use membrane::math::{Decimal256, Uint256};
use membrane::types::{AssetInfo, BidInput};
use membrane::oracle::PriceResponse;

use cosmwasm_std::testing::{mock_dependencies, mock_env, mock_info, MockApi, MockQuerier};
use cosmwasm_std::{from_binary, Coin, Decimal, MemoryStorage, OwnedDeps, Uint128};

const TOLERANCE: &str = "0.00001"; // 0.001%
const ITERATIONS: u32 = 100u32;

#[test]
fn stress_tests() {
    // submit bids and execute liquidations repeatedly
    // we can alternate larger and smaller executions to decrease the bid_pool product at different rates

    // with very tight liquidations, constatly resetting product
    // 1M USD bids
    simulate_bids_with_2_liq_amounts(
        ITERATIONS, PriceResponse {
            prices: vec![],
            price: Decimal::percent(2000),
            decimals: 6u64,
        },
        1000000000000u128,
        49999999999,
        49999999990,
    );
    // 10 USD bids
    simulate_bids_with_2_liq_amounts(
        ITERATIONS,
        PriceResponse {
            prices: vec![],
            price: Decimal::percent(2000),
            decimals: 6u64,
        },
        10000000u128,
        499999,
        499999,
    );

    // with greater asset price (10k USD per collateral)
    // 1M USD bids
    simulate_bids_with_2_liq_amounts(
        ITERATIONS,
        PriceResponse {
            prices: vec![],
            price: Decimal::percent(1000000),
            decimals: 6u64,
        },
        1000000000000u128,
        99999999,
        99999999,
    );
    // 10,001 USD bids
    simulate_bids_with_2_liq_amounts(
        ITERATIONS,
        PriceResponse {
            prices: vec![],
            price: Decimal::percent(1000000),
            decimals: 6u64,
        },
        10001000000u128,
        1000000,
        1000000,
    );

    // alternate tight executions, to simulate some bids claiming from 2 scales
    // 1M USD bids
    simulate_bids_with_2_liq_amounts(
        ITERATIONS,
        PriceResponse {
            prices: vec![],
            price: Decimal::percent(5000),
            decimals: 6u64,
        },
        1000000000000u128,
        19999999999,
        19900000000,
    );
    // 100 USD bids
    simulate_bids_with_2_liq_amounts(
        ITERATIONS,
        PriceResponse {
            prices: vec![],
            price: Decimal::percent(5000),
            decimals: 6u64,
        },
        100000000u128,
        1999999,
        1900000,
    );

    // 100k USD bids with very tight liquidations
    simulate_bids_with_2_liq_amounts(
        ITERATIONS,
        PriceResponse {
            prices: vec![],
            price: Decimal::percent(10000),
            decimals: 6u64,
        },
        100000000000u128,
        999999999,
        999999999,
    );

    // 100k USD bids with very small asset price, so even tighter liquidations
    simulate_bids_with_2_liq_amounts(
        ITERATIONS,
        PriceResponse {
            prices: vec![],
            price: Decimal::percent(10),
            decimals: 6u64,
        }, // 0.1 USD/asset
        100000000000u128,
        999999999900, // 10 micros of residue
        999999999999, // no residue
    );
}

fn instantiate_and_whitelist(deps: &mut OwnedDeps<MemoryStorage, MockApi, MockQuerier>) {
    let msg = InstantiateMsg {
        owner: None, //Defaults to sender
        positions_contract: String::from("positions_contract"),
        waiting_period: 60u64,
    };

    let info = mock_info("owner0000", &[]);
    let _res = instantiate(deps.as_mut(), mock_env(), info.clone(), msg).unwrap();

    let msg = ExecuteMsg::AddQueue {
        bid_for: AssetInfo::NativeToken {
            denom: "fury".to_string(),
        },
        max_premium: Uint128::new(10u128), //A slot for each premium is created when queue is created
        bid_threshold: Uint256::from(10_000_000_000_000u128),
    };
    execute(deps.as_mut(), mock_env(), info, msg).unwrap();
}

fn simulate_bids_with_2_liq_amounts(
    iterations: u32,
    asset_price: Decimal,
    bid_amount: u128,
    liq_amount_1: u128,
    liq_amount_2: u128,
) {
    let mut deps = mock_dependencies();
    instantiate_and_whitelist(&mut deps);

    let env = mock_env();
    let info = mock_info("positions_contract", &[]);

    let mut total_liquidated = Uint256::zero();
    let mut total_consumed = Uint256::zero();

    for i in 0..iterations {
        //Bidders
        let msg = ExecuteMsg::SubmitBid {
            bid_input: BidInput {
                bid_for: AssetInfo::NativeToken {
                    denom: "fury".to_string(),
                },
                liq_premium: 0u8,
            },
            bid_owner: None,
        };
        let submit_info = mock_info(
            "owner0000",
            &[Coin {
                denom: "cdt".to_string(),
                amount: Uint128::from(bid_amount),
            }],
        );
        execute(deps.as_mut(), mock_env(), submit_info.clone(), msg).unwrap();

        if i % 2 == 0 {
            // EXECUTE ALL EXCEPT 1uusd
            let liq_msg = ExecuteMsg::Liquidate {
                credit_price: PriceResponse {
            prices: vec![],
            price: Decimal::one(),
            decimals: 6u64,
        },
                collateral_price: asset_price,
                collateral_amount: Uint256::from(liq_amount_1),
                bid_for: AssetInfo::NativeToken {
                    denom: "fury".to_string(),
                },
                position_id: Uint128::new(1u128),
                position_owner: "owner01".to_string(),
            };
            total_liquidated += Uint256::from(liq_amount_1);
            total_consumed += Uint256::from(liq_amount_1 * asset_price.atomics().u128());

            
            execute(deps.as_mut(), mock_env(), info.clone(), liq_msg).unwrap();
        } else {
            // EXECUTE ALL EXCEPT 1uusd
            let liq_msg = ExecuteMsg::Liquidate {
                credit_price: PriceResponse {
            prices: vec![],
            price: Decimal::one(),
            decimals: 6u64,
        },
                collateral_price: asset_price,
                collateral_amount: Uint256::from(liq_amount_2),
                bid_for: AssetInfo::NativeToken {
                    denom: "fury".to_string(),
                },
                position_id: Uint128::new(1u128),
                position_owner: "owner01".to_string(),
            };
            total_liquidated += Uint256::from(liq_amount_2);
            total_consumed += Uint256::from(liq_amount_2 * asset_price.atomics().u128());

            
            execute(deps.as_mut(), mock_env(), info.clone(), liq_msg).unwrap();
        }
    }

    let mut queried_bids: u32 = 0u32;
    let mut total_claimed = Uint256::zero();
    let mut total_retracted = Uint256::zero();

    while queried_bids < iterations {
        let bids_res: Vec<BidResponse> = from_binary(
            &query(
                deps.as_ref(),
                mock_env(),
                QueryMsg::BidsByUser {
                    bid_for: AssetInfo::NativeToken {
                        denom: "fury".to_string(),
                    },
                    user: "owner0000".to_string(),
                    limit: Some(30u32),
                    start_after: Some(Uint128::from(queried_bids)),
                },
            )
            .unwrap(),
        )
        .unwrap();

        for bid in bids_res.iter() {
            queried_bids += 1u32;
            println!(
                "claim idx: {} - pending: {} remaining: {}",
                bid.id, bid.pending_liquidated_collateral, bid.amount
            );
            total_claimed += bid.pending_liquidated_collateral;
            total_retracted += bid.amount;
        }

        println!("total claimed:    {}", total_claimed);
        println!("total liquidated: {}", total_liquidated);
        assert!(total_claimed < total_liquidated);
    }

    let error: Decimal256 = Decimal256::one()
        - Decimal256::from_uint256(total_claimed) / Decimal256::from_uint256(total_liquidated);
    println!("error: {}", error);
    assert!(error < Decimal256::from_str(TOLERANCE).unwrap());
}
