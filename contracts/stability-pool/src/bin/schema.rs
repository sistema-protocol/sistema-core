use std::env::current_dir;
use std::fs::create_dir_all;

use cosmwasm_schema::write_api;

use membrane::stability_pool::{ InstantiateMsg, ExecuteMsg, QueryMsg };
fn main() {
    write_api! {
        instantiate: InstantiateMsg,
        execute: ExecuteMsg,
        query: QueryMsg,
    }
}
