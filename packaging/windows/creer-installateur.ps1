# Construit dist\Euhpen-Setup-<version>.exe : un seul fichier, qui contient toute l'application.
# Fichier enregistré en UTF-8 avec BOM pour Windows PowerShell 5.1.

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent (Split-Path -Parent $PSScriptRoot))

# Cette chaîne lie avec rust-lld (voir .cargo/config.toml) : ni Visual Studio ni MinGW.
$Toolchain = "stable-x86_64-pc-windows-gnullvm"
$Version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$Target = (Join-Path (Get-Location).Path "target")
if ($env:CARGO_TARGET_DIR) { $Target = $env:CARGO_TARGET_DIR }
$Work = Join-Path $Target "installer"
$Stage = Join-Path $Work "application"
$Archive = Join-Path $Work "application.zip"

function Run($File, [string[]]$Arguments) {
    & $File @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Échec de : $File $($Arguments -join ' ')" }
}

if (-not ((& rustup toolchain list) -match [regex]::Escape($Toolchain))) {
    Run "rustup" @("toolchain", "install", $Toolchain, "--profile", "minimal", "--no-self-update")
}

Write-Host "1/3  Compilation d'Euhpen" -ForegroundColor Cyan
Run "cargo" @("+$Toolchain", "build", "--release", "--locked", "--bins")

Write-Host "2/3  Préparation de l'application" -ForegroundColor Cyan
if (Test-Path $Work) { Remove-Item -Recurse -Force $Work }
New-Item -ItemType Directory -Force (Join-Path $Stage "python") | Out-Null
foreach ($Name in @("euhpen.exe", "euhpen-gui.exe")) {
    Copy-Item (Join-Path (Join-Path $Target "release") $Name) $Stage
}
Copy-Item LICENSE $Stage
Copy-Item python\requirements.txt (Join-Path $Stage "python")
Copy-Item -Recurse python\euhpen (Join-Path $Stage "python")
Get-ChildItem (Join-Path $Stage "python") -Recurse -Directory -Filter "__pycache__" | Remove-Item -Recurse -Force
# Le tar de Windows, car celui de Git ne sait pas écrire de zip.
Run (Join-Path $env:SystemRoot "System32\tar.exe") @("-a", "-c", "-f", $Archive, "-C", $Stage, "euhpen.exe", "euhpen-gui.exe", "LICENSE", "python")

Write-Host "3/3  Compilation de l'installateur" -ForegroundColor Cyan
$env:EUHPEN_PAYLOAD = $Archive
Run "cargo" @("+$Toolchain", "build", "--release", "--locked", "--features", "installer", "--bin", "euhpen-setup")
Remove-Item Env:EUHPEN_PAYLOAD

New-Item -ItemType Directory -Force dist | Out-Null
$Output = Join-Path (Resolve-Path dist).Path "Euhpen-Setup-$Version.exe"
Copy-Item (Join-Path (Join-Path $Target "release") "euhpen-setup.exe") $Output
$Size = [math]::Round((Get-Item $Output).Length / 1MB, 1)
Write-Host "Installateur prêt : $Output ($Size Mo)" -ForegroundColor Green
