#[cfg(test)]
#[allow(unused_variables)]
mod tests {

    use crate::helpers::VaultContract;

    use membrane::apollo_router::SwapToAssetsInput;
    use membrane::discount_vault::{ExecuteMsg, InstantiateMsg, QueryMsg};
    use membrane::positions::{PositionsResponse, PositionResponse};
    use membrane::types::{AssetInfo, Position, cAsset, Asset, Basket, PoolStateResponse, LPPoolInfo};

    use cosmwasm_std::{
        coin, to_binary, Addr, Binary, Empty, Response, StdResult, Uint128, Decimal, attr,
    };
    use cw_multi_test::{App, AppBuilder, BankKeeper, Contract, ContractWrapper, Executor};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    const USER: &str = "user";
    const ADMIN: &str = "admin";

    //Vault Contract
    pub fn vault_contract() -> Box<dyn Contract<Empty>> {
        let contract = ContractWrapper::new_with_empty(
            crate::contracts::execute,
            crate::contracts::instantiate,
            crate::contracts::query,
        );

        Box::new(contract)
    }

    //Mock Positions Contract
    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub enum CDP_MockExecuteMsg {}

    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub struct CDP_MockInstantiateMsg {}

    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub enum CDP_MockQueryMsg {
        GetBasket {},
    }

    pub fn cdp_contract() -> Box<dyn Contract<Empty>> {
        let contract = ContractWrapper::new(
            |deps, _, info, msg: CDP_MockExecuteMsg| -> StdResult<Response> {
                Ok(Response::default())
            },
            |_, _, _, _: CDP_MockInstantiateMsg| -> StdResult<Response> { Ok(Response::default()) },
            |_, _, msg: CDP_MockQueryMsg| -> StdResult<Binary> { 
                match msg {
                    
                    CDP_MockQueryMsg::GetBasket { } => {
                        Ok(to_binary(&Basket {
                            basket_id: Uint128::zero(),
                            current_position_id: Uint128::zero(),
                            collateral_types: vec![],
                            collateral_supply_caps: vec![],
                            credit_asset: Asset { info: AssetInfo::NativeToken { denom: String::from("cdt") }, amount: Uint128::zero() },
                            credit_price: Decimal::one(),
                            liq_queue: None,
                            base_interest_rate: Decimal::zero(),
                            liquidity_multiplier: Decimal::zero(),
                            pending_revenue: Uint128::zero(),
                            negative_rates: false,
                            cpc_margin_of_error: Decimal::zero(),
                            multi_asset_supply_caps: vec![],
                            frozen: false,
                            rev_to_stakers: true,
                            credit_last_accrued: 0,
                            rates_last_accrued: 0,
                            oracle_set: true,
                        })?)
                    },
                }
            },
        );
        Box::new(contract)
    }

    //Mock Osmo Proxy Contract
    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub enum Osmo_MockExecuteMsg {
        MintTokens {
            denom: String,
            amount: Uint128,
            mint_to_address: String,
        }
    }

    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub struct Osmo_MockInstantiateMsg {}

    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub enum Osmo_MockQueryMsg { 
       PoolState { id: u64 }
    }

    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub struct MockResponse {  }

    pub fn osmosis_proxy_contract() -> Box<dyn Contract<Empty>> {
        let contract = ContractWrapper::new(
            |deps, _, info, msg: Osmo_MockExecuteMsg| -> StdResult<Response> {
                Ok(Response::new())
            },
            |_, _, _, _: Osmo_MockInstantiateMsg| -> StdResult<Response> {
                Ok(Response::default())
            },
            |_, _, msg: Osmo_MockQueryMsg| -> StdResult<Binary> {
                match msg {
                    Osmo_MockQueryMsg::PoolState { id } => {
                        Ok(to_binary(&PoolStateResponse {
                            assets: vec![coin(100, "uosmo").into(), coin(100, "cdt").into()],
                            shares: coin(100, format!("gamm/pool/{}", id)).into(),
                        })?)
                    }
                }
            },
        );
        Box::new(contract)
    }

    fn mock_app() -> App {
        AppBuilder::new().build(|router, _, storage| {
            let bank = BankKeeper::new();

            bank.init_balance(
                storage,
                &Addr::unchecked(USER),
                vec![coin(100_000, "debit")],
            )
            .unwrap();
            bank.init_balance(
                storage,
                &Addr::unchecked("contract2"),
                vec![coin(100_000_000, "credit")],
            )
            .unwrap();

            router.bank = bank;
        })
    }

    fn proper_instantiate() -> (App, VaultContract) {
        let mut app = mock_app();

        //Instaniate CDP
        let cdp_id = app.store_code(cdp_contract());

        let cdp_contract_addr = app
            .instantiate_contract(
                cdp_id,
                Addr::unchecked(ADMIN),
                &CDP_MockInstantiateMsg {},
                &[],
                "test",
                None,
            )
            .unwrap();

        //Instantiate OP contract
        let op_id = app.store_code(osmosis_proxy_contract());

        let op_contract_addr = app
            .instantiate_contract(
                op_id,
                Addr::unchecked(ADMIN),
                &Osmo_MockInstantiateMsg {},
                &[],
                "test",
                None,
            )
            .unwrap();

        //Instantiate Vault contract
        let vault_id = app.store_code(vault_contract());

        let msg = InstantiateMsg {        
            owner: None,
            positions_contract: String::from(cdp_contract_addr),
            osmosis_proxy: String::from(op_contract_addr),
            accepted_LPs: vec![1],
        };

        let vault_contract_addr = app
            .instantiate_contract(
                vault_id,
                Addr::unchecked(ADMIN),
                &msg,
                &[],
                "test",
                None,
            )
            .unwrap();

        let vault_contract = VaultContract(vault_contract_addr);

        (app, vault_contract)
    }

    mod vault {

        use cosmwasm_std::coins;
        use membrane::discount_vault::Config;

        use super::*;

        #[test]
        fn change_owner() {
            let (mut app, vault_contract) = proper_instantiate();

            //ChangeOwner
            let msg = ExecuteMsg::ChangeOwner {
                owner: String::from("different_owner"),
            };
            let cosmos_msg = vault_contract.call(msg, vec![]).unwrap();
            app.execute(Addr::unchecked(ADMIN), cosmos_msg).unwrap();

            //Query Config
            let config: Config = app
                .wrap()
                .query_wasm_smart(
                    vault_contract.addr(),
                    &QueryMsg::Config {},
                )
                .unwrap();
            assert_eq!(
                config.owner.to_string(),        
                String::from("different_owner"),
            );
        }

        #[test]
        fn edit_LPs() {
            let (mut app, vault_contract) = proper_instantiate();

            //Add an LP
            let msg = ExecuteMsg::EditAcceptedLPs { pool_id: 2, remove: false };
            let cosmos_msg = vault_contract.call(msg, vec![]).unwrap();
            app.execute(Addr::unchecked(ADMIN), cosmos_msg).unwrap();

            //Query Config
            let config: Config = app
                .wrap()
                .query_wasm_smart(
                    vault_contract.addr(),
                    &QueryMsg::Config {},
                )
                .unwrap();
            assert_eq!(
                config.accepted_LPs,        
                vec![
                    LPPoolInfo { share_token: AssetInfo::NativeToken { denom: String::from("gamm/pool/1")}, pool_id: 1 },
                    LPPoolInfo { share_token: AssetInfo::NativeToken { denom: String::from("gamm/pool/2")}, pool_id: 2 },
                ]
            );

            //Remove an LP
            let msg = ExecuteMsg::EditAcceptedLPs { pool_id: 1, remove: true };
            let cosmos_msg = vault_contract.call(msg, vec![]).unwrap();
            app.execute(Addr::unchecked(ADMIN), cosmos_msg).unwrap();

            //Query Config
            let config: Config = app
                .wrap()
                .query_wasm_smart(
                    vault_contract.addr(),
                    &QueryMsg::Config {},
                )
                .unwrap();
            assert_eq!(
                config.accepted_LPs,        
                vec![
                    LPPoolInfo { share_token: AssetInfo::NativeToken { denom: String::from("gamm/pool/2")}, pool_id: 2 },
                ]
            );

        }
    }
}