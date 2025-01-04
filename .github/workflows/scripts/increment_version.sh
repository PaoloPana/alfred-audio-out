#!/bin/bash
set -xeu

version="$1"
IFS=. read -r v1 v2 v3 <<< "${version}"
((v2++))
v3=0
new_version="${v1}.${v2}.${v3}"
IFS=";"
for crate_path in $CRATE_PATHS; do
    cd "$crate_path"
    sed -i "s/^version \?=.*$/version = \"$new_version\"/g" Cargo.toml
    cd - || exit 1
done