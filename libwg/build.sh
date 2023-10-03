#!/bin/bash

git clone --depth=1 https://github.com/PinkD/wireguard-go
cd wireguard-go
make libwg
