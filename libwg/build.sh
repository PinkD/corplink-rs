#!/bin/bash

git clone https://github.com/PinkD/wireguard-go
cd wireguard-go
make libwg
mv libwg.* ../
