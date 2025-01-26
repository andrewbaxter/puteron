#!/usr/bin/bash -xeu
rm -f generated/jsonschema/*.json
cd puteron-bin
cargo run --bin generate_jsonschema