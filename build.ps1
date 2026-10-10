# Builds sk_mario.dll on Windows (adapted from deltarooo/er-mario's build.ps1).
# With -Dist <folder>, also copies it into that folder (default: .\dist).
# Needs: Rust (rustup), Visual Studio Build Tools (C++), LLVM (clang-cl for libsm64's C code).
param(
    # the mod folder me3 loads the DLL from (defaults to .\dist)
    [string]$Dist = ""
)
$ErrorActionPreference = 'Stop'
$env:Path = [System.Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [System.Environment]::GetEnvironmentVariable('Path', 'User')
$env:Path += ";$env:USERPROFILE\.cargo\bin;C:\Program Files\LLVM\bin"
# libsm64 is decompiled N64 C code: clang-cl handles it like the Linux cross build did
$env:CC_x86_64_pc_windows_msvc = 'clang-cl'
# no local paths (user name) in the DLL's panic messages
$reg = Get-ChildItem "$env:USERPROFILE\.cargo\registry\src" -Directory -ErrorAction SilentlyContinue | Select-Object -First 1
$flags = @("--remap-path-prefix=$(Split-Path $PSScriptRoot)=src", "--remap-path-prefix=$env:USERPROFILE=~")
if ($reg) { $flags = @("--remap-path-prefix=$($reg.FullName)=crates") + $flags }
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = $flags -join ' '

Push-Location $PSScriptRoot
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} finally {
    Pop-Location
}
$dll = "$PSScriptRoot\target\x86_64-pc-windows-msvc\release\sk_mario.dll"
if (-not $Dist) { $Dist = "$PSScriptRoot\dist" }
New-Item -ItemType Directory -Force -Path $Dist | Out-Null
# swap in by rename: overwriting a DLL a running game has loaded can crash the game
Copy-Item $dll "$Dist\sk_mario.dll.new" -Force
Get-ChildItem "$Dist\sk_mario.dll.old*" -ErrorAction SilentlyContinue | ForEach-Object { try { Remove-Item $_ -Force -ErrorAction Stop } catch {} }
if (Test-Path "$Dist\sk_mario.dll") {
    Move-Item "$Dist\sk_mario.dll" "$Dist\sk_mario.dll.old$(Get-Date -Format HHmmss)" -Force
}
Move-Item "$Dist\sk_mario.dll.new" "$Dist\sk_mario.dll" -Force
Write-Host "built and installed into $Dist"
