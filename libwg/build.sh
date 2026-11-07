#!/bin/bash

set -euo pipefail
cd -- "$(dirname -- "$0")"
git submodule update --init --recursive
cd wireguard-go
make libwg
cp libwg.a libwg.h ../
