
# Sistema Contracts

Sistema is a community-owned DAO that carries the torch of decentralization. The main protocol is a pegged token (stablecoin) stabilization system that uses collateralized debt in the form of collateralized debt positions (CDPs) to mint. A CDP is a loan that holds collateral and mints debt within the set LTV limits of the position. Typical CDPs are 1 collateral per position but Sistema enables bundles to mix-n-match collateral enabling further risk control for the position owners. Prices are sourced from the oracle contract which currently queries Osmosis TWAPs & Pyth Network (for FURY/USD). As new robust oracles are deployed Sistema should diversify.

Loan liquidations are used to keep the debt token collateralized and typically auction off the full amount to the contract caller in a first come first serve manner. Sistema’s liquidations are a 4-part filtration system that liquidate collateral at market driven fees until ultimately being backed by the Sistema network token. 

From 1-4: 
- Liquidation Queue for single collateral w/ dynamic fees 
- Stability Pool for all collateral at a fixed fee
- Market sales through a DEX router
- Bad debt recapitalization through Sistema Debt Auctions

Pegged token mints are handled by the Osmosis Proxy in a way that allows for multiple versions of the CDP contract to run in tandem. All external user facing contracts that hold funds should be immutable long term to allow the market to choose its upgrades. 

## Core CDP Contracts

| Name                                                       | Description                                  |
| ---------------------------------------------------------- | -------------------------------------------- |
| [`positions`](contracts/cdp)                               | Credit position manager                      |
| [`liquidation queue`](contracts/liq-queue)                   | Debt liquidation queue                       |
| [`stability pool`](contracts/stability-pool)               | Position stability pool                      |
| [`debt auction`](contracts/debt_auction)                   | Last-resort TEMA auction for bad debt        |
| [`oracle`](contracts/oracle)                               | TWAP oracles for approved assets             |
| [`liquidity check`](contracts/liquidity_check)             | Checks for acceptable AMM liquidity of collateral assets ‎ ‎  ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ |

## Governance Contracts

| Name                                                       | Description                                  |
| ---------------------------------------------------------- | -------------------------------------------- |
| [`governance`](contracts/governance)                       | Decentralized governance contract for updating protocol params and contract versions |
| [`staking`](contracts/staking)                             | Manages staked TEMA functionality            |
| [`vesting`](contracts/vesting)                             | Manages vesting TEMA functionality           |

## Periphery Contracts

| Name                                                       | Description                                  |
| ---------------------------------------------------------- | -------------------------------------------- |
| [`osmosis proxy`](contracts/osmosis-proxy)                 | Proxy to Osmosis SDK module functions        |
| [`margin proxy`](contracts/margin-proxy)                   | Proxy for cleaner looped margin functionality    ‎ ‎  ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎  ‎ ‎  ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎ ‎  |

## Audit
[Oak Github](https://github.com/oak-security/audit-reports/blob/master/Sistema/2023-06-15%20Audit%20Report%20-%20Sistema%20v1.0.pdf)

## Docs
[Documentation](https://membrane-finance.gitbook.io/membrane-docs-1/)

[Documentation Github](https://github.com/triccs/membrane-docs)

## Osmosis Mainnet Addresses
launch: "osmo1g6hgj3eu9ju4vuaprjxdzj97ecnuczytve3junulgnwlamnndl5q6k73w6",
discount_vault: "osmo1v8wckds5lvsdd0xrragvleu8srxprjpwdl7mga5uygnwmz5e7qzsl5zexw",
governance_v1: "osmo19h8huy2hz4q7detxzv2r2erlsvlq8hzlsquu6n5x83775va4qgkskf20kq",
governance_v2: "osmo1wk0zlag50ufu5wrsfyelrylykfe3cw68fgv9s8xqj20qznhfm44qgdnq86",
liq_queue: "furya1kppp8ywzdgtmqlup25ws33fdlhwwuu8he2yk3vgkuldzcfr5a2jqp8qcsa",
liquidity_check: "osmo1xxx0yuqhmwekt44q00jrf3rwvfa70rpeu622q0x56yaf423vq93q3qpzux",
tema_auction: "osmo1qwdlg9le9kdrvgyp35jxz53m8zhdssyvxvyevmdxcn852h6dq9gqknf2aa",
oracle: "osmo160t4k7x8axfd335s0rj5jdffzag684tjrzchlwmqk23xte32alvq6nfz6k",
osmosis_proxy: "furya1qte5mlv06nwu7wqewxg9066xjvlmlyghs9d5udkf8el5vvn50sfscgnylk",
positions: "furya1nes05572upvgkyz9ctsz787qt9r3pzy5nfe0r0n49m2fh002ytmspeuw8u",
stability_pool: "osmo1326cxlzftxklgf92vdep2nvmqffrme0knh8dvugcn9w308ya9wpqv03vk8",
staking: "osmo1fty83rfxqs86jm5fmlql5e340e8pe0v9j8ez0lcc6zwt2amegwvsfp3gxj",
system_discounts: "osmo1p0hvtat69dash8f0w340n2kjdkdfq0ggyp77mr426wpnfwp3tjyqq6a8vr",
vesting: "osmo1flwr85scpcsdqa8uyh0acgxeqlg2ln8tlklzwzdn4u68n3p5wegsgspjf6"
