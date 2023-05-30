use std::{fs::create_dir_all, env::current_dir};

use cosmwasm_schema::{export_schema, remove_schemas, schema_for};

use membrane::liq_queue::{InstantiateMsg, ExecuteMsg, QueryMsg, Config, BidResponse, QueueResponse, LiquidatibleResponse, SlotResponse, ClaimsResponse};

fn main() {
    let mut out_dir = current_dir().unwrap();
    out_dir.push("schema");
    create_dir_all(&out_dir).unwrap();
    remove_schemas(&out_dir).unwrap();

    export_schema(&schema_for!(InstantiateMsg), &out_dir);
    export_schema(&schema_for!(ExecuteMsg), &out_dir);
    export_schema(&schema_for!(QueryMsg), &out_dir);
    export_schema(&schema_for!(Config), &out_dir);
    export_schema(&schema_for!(BidResponse), &out_dir);
    export_schema(&schema_for!(QueueResponse), &out_dir);
    export_schema(&schema_for!(LiquidatibleResponse), &out_dir);
    export_schema(&schema_for!(SlotResponse), &out_dir);
    export_schema(&schema_for!(ClaimsResponse), &out_dir);    
}
