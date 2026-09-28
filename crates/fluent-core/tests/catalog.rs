//! Tool descriptors built from the fixture registrations.
//!
//! The descriptor lists are compared with the reviewed files in
//! `tests/golden/`. After an intended change, regenerate them with
//!
//! ```sh
//! UPDATE_GOLDEN=1 cargo test -p fluent-core --test catalog
//! ```
//!
//! and review the diff. `tests/fixtures/tii/complex.tii` is the SDK spec's
//! `test-vectors/complex-types/complex.tii`, copied verbatim; it declares only
//! a `local` profile, so it is exercised through `input_schema` rather than as
//! a registration.

use std::fs;
use std::path::PathBuf;

use fluent_core::catalog::{
    GET_SKILL_TOOL, INSPECT_ADDRESS_TOOL, ToolAnnotations, ToolDescriptor, all_tools, build_tools,
    fixed_tools, input_schema,
};
use fluent_core::registration::{Registration, load_dir};
use serde_json::{Value, json};
use tx3_sdk::tii::spec::TiiFile;

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

/// Runs a loader future to completion. The fixtures are local bundles, so
/// nothing is fetched.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(future)
}

fn tools_for(bundle: &str) -> Vec<ToolDescriptor> {
    let registration = block_on(Registration::load(fixture(&format!(
        "registrations/valid/{bundle}"
    ))))
    .unwrap_or_else(|err| panic!("{bundle}: {err}"));
    build_tools(&registration).unwrap_or_else(|err| panic!("{bundle}: {err}"))
}

fn complex_tii() -> TiiFile {
    let text = fs::read_to_string(fixture("tii/complex.tii")).expect("complex.tii is readable");
    serde_json::from_str(&text).expect("complex.tii is a TII")
}

fn complex_schema() -> Value {
    input_schema(&complex_tii(), "complex", "local").unwrap_or_else(|err| panic!("complex: {err}"))
}

fn tool<'a>(tools: &'a [ToolDescriptor], name: &str) -> &'a ToolDescriptor {
    tools
        .iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("no tool {name}"))
}

fn assert_golden(name: &str, tools: &[ToolDescriptor]) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.json"));
    let actual = serde_json::to_string_pretty(tools).expect("descriptors serialize") + "\n";
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::write(&path, &actual).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        return;
    }
    let expected =
        fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    assert!(
        actual == expected,
        "{} is out of date; regenerate with UPDATE_GOLDEN=1 and review the diff.\n\
         --- actual ---\n{actual}",
        path.display()
    );
}

#[test]
fn transfer_descriptors_match_golden() {
    assert_golden("transfer_preprod", &tools_for("transfer_preprod"));
}

#[test]
fn strike_descriptors_match_golden() {
    assert_golden(
        "strike_staking_mainnet",
        &tools_for("strike_staking_mainnet"),
    );
}

#[test]
fn fixed_descriptors_match_golden() {
    assert_golden("fixed_tools", &fixed_tools());
}

/// Every JSON Pointer at which `value` has an object key `key`.
fn find_key(value: &Value, key: &str, path: &str, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (name, child) in map {
                let child_path = format!("{path}/{name}");
                if name == key {
                    found.push(child_path.clone());
                }
                find_key(child, key, &child_path, found);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                find_key(child, key, &format!("{path}/{index}"), found);
            }
        }
        _ => {}
    }
}

#[test]
fn no_ref_remains_anywhere() {
    let mut tools = tools_for("transfer_preprod");
    tools.extend(tools_for("strike_staking_mainnet"));
    tools.extend(fixed_tools());
    let mut documents: Vec<(String, Value)> = tools
        .iter()
        .map(|tool| (tool.name.clone(), serde_json::to_value(tool).unwrap()))
        .collect();
    documents.push(("complex".to_string(), complex_schema()));

    // The fixtures do use refs, so this is not vacuous.
    let raw = fs::read_to_string(fixture("tii/complex.tii")).unwrap()
        + &fs::read_to_string(fixture(
            "registrations/valid/strike_staking_mainnet/strike-staking.tii",
        ))
        .unwrap();
    assert!(raw.contains("#/components/schemas/") && raw.contains("tii#/$defs/"));

    for (name, document) in documents {
        for key in ["$ref", "$defs", "definitions"] {
            let mut found = Vec::new();
            find_key(&document, key, "", &mut found);
            assert!(found.is_empty(), "{name} has {key} at {found:?}");
        }
    }
}

fn property_names(schema: &Value) -> Vec<&str> {
    schema["properties"]
        .as_object()
        .expect("properties is an object")
        .keys()
        .map(String::as_str)
        .collect()
}

fn required(schema: &Value) -> Vec<&str> {
    schema["required"]
        .as_array()
        .expect("required is an array")
        .iter()
        .map(|name| name.as_str().expect("required holds names"))
        .collect()
}

#[test]
fn bound_environment_and_parties_are_absent() {
    // preprod binds `tax` and no party.
    let tools = tools_for("transfer_preprod");
    let schema = &tool(&tools, "transfer_preprod_transfer").input_schema;
    assert!(!property_names(schema).contains(&"tax"));
    assert_eq!(
        required(schema),
        ["quantity", "middleman", "receiver", "sender"]
    );

    // mainnet binds every environment field and the `stakingscript` party.
    let bound = [
        "mint_policy_id",
        "staking_asset_name",
        "staking_policy_id",
        "strike_script_ref",
        "tracker_asset_name",
        "stakingscript",
    ];
    let tools = tools_for("strike_staking_mainnet");
    assert_eq!(tools.len(), 3);
    for tool in &tools {
        let schema = &tool.input_schema;
        for key in bound {
            assert!(
                !property_names(schema).contains(&key),
                "{} exposes bound `{key}`",
                tool.name
            );
            assert!(!required(schema).contains(&key));
        }
        assert_eq!(required(schema).last(), Some(&"staker"), "{}", tool.name);
        assert_eq!(
            schema["properties"]["staker"],
            json!({ "type": "string", "description": "bech32 address of the staker party" })
        );
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    // An empty profile binds nothing: parties and environment are arguments.
    let schema = complex_schema();
    for key in ["sender", "receiver", "fee"] {
        assert!(required(&schema).contains(&key), "{key}");
    }
}

#[test]
fn transaction_tools_follow_the_naming_rules() {
    let tools = tools_for("strike_staking_mainnet");
    let stake = tool(&tools, "strike_staking_mainnet_stake");
    assert_eq!(stake.title, "strike-staking 0.1.0 · stake (mainnet)");
    assert_eq!(
        stake.description,
        "Prepares an UNSIGNED mainnet transaction for strike-finance/strike-staking:0.1.0. \
         Nothing is signed or submitted. Call fluent_get_skill for this protocol before using \
         this tool."
    );
    assert_eq!(
        stake.registration_slug.as_deref(),
        Some("strike_staking_mainnet")
    );
    assert_eq!(stake.tx_name.as_deref(), Some("stake"));
    assert_eq!(stake.annotations, ToolAnnotations::TRANSACTION);
    assert_eq!(
        stake.annotations,
        ToolAnnotations {
            read_only_hint: true,
            destructive_hint: false,
            idempotent_hint: false,
            open_world_hint: true,
        }
    );
    assert_eq!(
        stake.output_schema["properties"]["next_steps"]["const"],
        json!(fluent_core::envelope::NEXT_STEPS)
    );

    // Parameter order and TII keywords survive inlining.
    assert_eq!(
        required(&stake.input_schema),
        ["owner_pkh", "amount", "staker"]
    );
    assert_eq!(
        stake.input_schema["properties"]["owner_pkh"]["pattern"],
        json!("^(0x)?[0-9a-fA-F]*$")
    );
}

#[test]
fn complex_types_are_inlined() {
    let schema = complex_schema();
    let properties = &schema["properties"];

    assert_eq!(
        properties["pair"]["prefixItems"],
        json!([
            { "type": "integer" },
            {
                "type": "string",
                "pattern": "^(0x)?[0-9a-fA-F]*$",
                "description": "Hex-encoded byte string, with an optional 0x prefix"
            }
        ])
    );
    assert_eq!(properties["pair"]["items"], json!(false));

    let cases = properties["side"]["oneOf"].as_array().unwrap();
    assert_eq!(cases.len(), 2);
    assert_eq!(cases[0]["required"], json!(["Buy"]));
    assert_eq!(
        cases[1]["properties"]["Sell"]["properties"]["price"],
        json!({ "type": "integer" })
    );

    // The `AssetClass` record, with its `Bytes` fields, in place of the refs.
    assert_eq!(properties["asset"]["type"], json!("object"));
    assert_eq!(properties["asset"]["required"], json!(["policy", "name"]));
    assert_eq!(
        properties["asset"]["properties"]["policy"]["pattern"],
        json!("^(0x)?[0-9a-fA-F]*$")
    );
    assert_eq!(
        properties["bag"]["properties"]["amount"],
        json!({ "type": "integer" })
    );
    assert_eq!(
        properties["recipient"],
        json!({ "type": "string", "description": "bech32 address" })
    );
    assert_eq!(
        properties["source"]["pattern"],
        json!("^(0x)?[0-9a-fA-F]{64}#[0-9]+$")
    );
    assert_eq!(
        properties["labels"]["additionalProperties"],
        json!({ "type": "integer" })
    );
    assert_eq!(
        required(&schema),
        [
            "quantity",
            "flag",
            "nothing",
            "recipient",
            "source",
            "bag",
            "amounts",
            "pair",
            "labels",
            "asset",
            "side",
            "receiver",
            "sender",
            "fee"
        ]
    );
}

#[test]
fn all_tools_lists_fixed_then_registration_tools() {
    let loaded = block_on(load_dir(fixture("registrations/valid"))).unwrap();
    assert!(loaded.rejected.is_empty());
    let names: Vec<String> = all_tools(&loaded.catalog)
        .unwrap()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(
        names,
        [
            GET_SKILL_TOOL,
            INSPECT_ADDRESS_TOOL,
            "strike_staking_mainnet_add_stake",
            "strike_staking_mainnet_stake",
            "strike_staking_mainnet_withdraw_stake",
            "transfer_preprod_transfer",
        ]
    );
}

const ADDRESS: &str = "addr1z9yh4zcqs4gh78ysvh8nqp40fsnxg49nn3h6x25az9k8tms6409492020k6xml8uvwn34wrexagjh5fsk5xk96jyxk2qf3a7kj";
const KEY_HASH: &str = "497a8b0085517f1c9065cf3006af4c266454b39c6fa32a9d116c75ee";
const UTXO_REF: &str = "486c6c010d1518b1e032d2a288483fba55cee4f054b6e97f4e7eadeccb173768#0";

/// One valid argument set per generated schema, with a required field to drop.
fn samples() -> Vec<(String, Value, Value, &'static str)> {
    let mut tools = tools_for("transfer_preprod");
    tools.extend(tools_for("strike_staking_mainnet"));
    tools.extend(fixed_tools());
    let args = |name: &str| -> (Value, &'static str) {
        match name {
            "transfer_preprod_transfer" => (
                json!({
                    "quantity": 5_000_000,
                    "sender": "addr_test1sender",
                    "receiver": "addr_test1receiver",
                    "middleman": "addr_test1middleman"
                }),
                "quantity",
            ),
            "strike_staking_mainnet_stake" => (
                json!({ "owner_pkh": KEY_HASH, "amount": 1_000, "staker": ADDRESS }),
                "owner_pkh",
            ),
            "strike_staking_mainnet_add_stake" => (
                json!({
                    "owner_pkh": format!("0x{KEY_HASH}"),
                    "staking_utxo": UTXO_REF,
                    "additional_amount": 10,
                    "staker": ADDRESS
                }),
                "staking_utxo",
            ),
            "strike_staking_mainnet_withdraw_stake" => (
                json!({
                    "owner_pkh": KEY_HASH,
                    "staking_utxo": format!("0x{UTXO_REF}"),
                    "staker": ADDRESS
                }),
                "staker",
            ),
            GET_SKILL_TOOL => (json!({ "protocol": "strike_staking_mainnet" }), "protocol"),
            INSPECT_ADDRESS_TOOL => (json!({ "address": ADDRESS }), "address"),
            other => panic!("no sample for {other}"),
        }
    };
    let mut samples: Vec<_> = tools
        .into_iter()
        .map(|tool| {
            let (value, drop) = args(&tool.name);
            (tool.name, tool.input_schema, value, drop)
        })
        .collect();
    samples.push((
        "complex".to_string(),
        complex_schema(),
        json!({
            "quantity": 1,
            "flag": true,
            "nothing": null,
            "recipient": "addr_test1recipient",
            "source": UTXO_REF,
            "bag": { "policy": KEY_HASH, "asset_name": "535452494b45", "amount": 3 },
            "amounts": [1, 2, 3],
            "pair": [7, "cafe"],
            "labels": { "a": 1, "b": 2 },
            "asset": { "policy": KEY_HASH, "name": "535452494b45" },
            "side": { "Sell": { "price": 5 } },
            "sender": "addr_test1sender",
            "receiver": "addr_test1receiver",
            "fee": 200_000
        }),
        "side",
    ));
    samples
}

#[test]
fn schemas_accept_valid_arguments_and_reject_missing_ones() {
    let samples = samples();
    assert_eq!(samples.len(), 7);
    for (name, schema, valid, drop) in samples {
        let validator = jsonschema::draft202012::new(&schema)
            .unwrap_or_else(|err| panic!("{name}: invalid schema: {err}"));
        if let Err(err) = validator.validate(&valid) {
            panic!("{name}: valid arguments rejected: {err}");
        }

        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(drop);
        assert!(
            !validator.is_valid(&missing),
            "{name}: accepted arguments without `{drop}`"
        );

        let mut extra = valid.clone();
        extra["unexpected"] = json!(1);
        assert!(
            !validator.is_valid(&extra),
            "{name}: accepted an unknown argument"
        );
    }
}

#[test]
fn schemas_reject_malformed_values() {
    let schema = complex_schema();
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    let valid = samples().pop().unwrap().2;
    for (field, value) in [
        ("source", json!("abcd#0")),
        ("pair", json!([7, "cafe", 8])),
        ("pair", json!(["cafe", 7])),
        ("side", json!({ "Hold": {} })),
        ("side", json!({ "Sell": {} })),
        ("asset", json!({ "policy": "not hex", "name": "00" })),
        ("amounts", json!(["1"])),
    ] {
        let mut args = valid.clone();
        args[field] = value.clone();
        assert!(!validator.is_valid(&args), "accepted {field} = {value}");
    }
}
