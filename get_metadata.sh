#!/bin/bash -xe

cd $(dirname $0)

curl -sX POST -H "Content-Type: application/json" --data \
    '{"jsonrpc":"2.0","method":"state_getMetadata", "id": 1}' \
    localhost:9944 | jq .result | cut -d '"' -f 2 | xxd -r -p > runtime/metadata-ggx-dev.scale
