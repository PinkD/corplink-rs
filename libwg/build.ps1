$ErrorActionPreference = "Stop"
Push-Location $PSScriptRoot
try {
    git submodule update --init --recursive
    if ($LASTEXITCODE -ne 0) { throw "Submodule checkout failed" }

    Set-Location wireguard-go
    make libwg
    if ($LASTEXITCODE -ne 0) { throw "libwg build failed" }

    Copy-Item -Path libwg.a,libwg.h -Destination .. -Force
} finally {
    Pop-Location
}
