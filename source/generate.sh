#!/usr/bin/bash -xeu
rm -f generated/jsonschema/*.json
cd puteron
cargo run --bin generate_jsonschema