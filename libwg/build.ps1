git submodule update --init --recursive

Set-Location wireguard-go

make libwg

Move-Item -Path "libwg.*" -Destination ".."
