# Builds yui-live-kit.dll against an installed OBS, with no OBS rebuild and no Qt.
# Run from an x64 Visual Studio developer shell (cl.exe, lib.exe, dumpbin.exe on PATH).
param(
    [string]$ObsVersion = '33.0.1',
    [string]$ObsBin = 'C:\Program Files\obs-studio\bin\64bit'
)

$ErrorActionPreference = 'Stop'
if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
    throw 'Run this script from an x64 Visual Studio developer shell.'
}
$root = $PSScriptRoot
$sdk = Join-Path $root 'obs-sdk\src'
$out = New-Item -ItemType Directory -Force (Join-Path $root 'build')

# Headers only, from the OBS release that matches the installed DLLs.
if (-not (Test-Path (Join-Path $sdk 'libobs\obs-module.h'))) {
    git clone --quiet --depth 1 --branch $ObsVersion --filter=blob:none --sparse https://github.com/obsproject/obs-studio.git $sdk
    if ($LASTEXITCODE) { throw 'Fetching OBS headers failed' }
    git -C $sdk sparse-checkout set libobs frontend/api
    if ($LASTEXITCODE) { throw 'Fetching OBS headers failed' }
}

Push-Location -LiteralPath $out
try {
    # Import libraries generated from the installed DLLs.
    foreach ($name in @('obs', 'obs-frontend-api')) {
        $symbols = & dumpbin.exe /nologo /exports (Join-Path $ObsBin "$name.dll")
        if ($LASTEXITCODE) { throw "dumpbin failed for $name.dll" }
        $exports = foreach ($line in $symbols) {
            if ($line -match '^\s+\d+\s+[\da-fA-F]+\s+[\da-fA-F]+\s+(\S+)') { $Matches[1] }
        }
        if (-not $exports) { throw "No exports found in $name.dll" }
        @("LIBRARY $name.dll", 'EXPORTS') + $exports | Set-Content -Encoding ascii "$name.def"
        & lib.exe /nologo /machine:x64 "/def:$name.def" "/out:$name.lib"
        if ($LASTEXITCODE) { throw "Import library generation failed for $name.dll" }
    }

    @('#pragma once', '#define OBS_RELEASE_CANDIDATE 0', '#define OBS_BETA 0') | Set-Content -Encoding ascii obsconfig.h

    & cl.exe /nologo /O2 /MD /utf-8 /std:c11 /W3 "/I$out" "/I$sdk\libobs" "/I$sdk\frontend\api" /c "$root\yui-live-kit.c"
    if ($LASTEXITCODE) { throw 'Compilation failed' }
    & link.exe /nologo /dll /out:yui-live-kit.dll yui-live-kit.obj obs.lib obs-frontend-api.lib
    if ($LASTEXITCODE) { throw 'Linking failed' }
    Write-Host "Built $out\yui-live-kit.dll"
} finally {
    Pop-Location
}
