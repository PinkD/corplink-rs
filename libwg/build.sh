#!/bin/bash

git submodule update --init --recursive
cd wireguard-go
make libwg
mv libwg.* ../
