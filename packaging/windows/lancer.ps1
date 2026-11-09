# Euhpen pour Windows : installe ce qui manque, compile, puis ouvre l'application.
# Appelé par « Lancer Euhpen.bat ». Fichier enregistré en UTF-8 avec BOM pour Windows PowerShell 5.1.

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent (Split-Path -Parent $PSScriptRoot))
try { [Console]::OutputEncoding = [Text.Encoding]::UTF8 } catch { }

$Venv = ".venv"
$Py = Join-Path $Venv "Scripts\python.exe"
$Gui = "target\release\euhpen-gui.exe"
$Stamp = ".euhpen-install"
$RustToolchain = "stable-x86_64-pc-windows-gnullvm"
$CudaIndex = "https://download.pytorch.org/whl/cu128"

function Say($Message) { Write-Host "`n$Message" -ForegroundColor Cyan }
function Fail($Message) {
    Write-Host "`n$Message" -ForegroundColor Red
    Read-Host "Appuyez sur Entrée pour fermer" | Out-Null
    exit 1
}
function Ask($Question) { return (Read-Host "$Question [o/N]") -match '^[oOyY]' }
function Has($Command) { return [bool](Get-Command $Command -ErrorAction SilentlyContinue) }
function Refresh-Path {
    # Ce que winget vient d'installer n'est pas encore dans le PATH de cette fenêtre.
    $env:Path = $env:Path + ";" +
                [Environment]::GetEnvironmentVariable("Path", "Machine") + ";" +
                [Environment]::GetEnvironmentVariable("Path", "User") + ";" +
                (Join-Path $env:USERPROFILE ".cargo\bin") + ";" +
                (Join-Path $env:USERPROFILE ".local\bin")
}
function Run($File, [string[]]$Arguments) {
    & $File @Arguments
    if ($LASTEXITCODE -ne 0) { Fail "Échec de : $File $($Arguments -join ' ')" }
}
function Pip([string[]]$Arguments) {
    if (Has "uv") { Run "uv" (@("pip", "install", "--python", $Py) + $Arguments) }
    else { Run $Py (@("-m", "pip", "install") + $Arguments) }
}
function Cuda-Available {
    $Previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        & $Py -c "import sys, torch; sys.exit(0 if torch.cuda.is_available() else 3)" 2>$null | Out-Null
        $Available = ($LASTEXITCODE -eq 0)
    } catch { $Available = $false }
    $ErrorActionPreference = $Previous
    return $Available
}
function Find-Python {
    foreach ($Line in @("py -3.12", "py -3.11", "py -3.13", "py -3.10", "python")) {
        $Candidate = $Line -split " "
        if (-not (Has $Candidate[0])) { continue }
        $Arguments = @($Candidate | Select-Object -Skip 1) + @("-c", "import sys; sys.exit(sys.version_info < (3, 10))")
        $Previous = $ErrorActionPreference
        $ErrorActionPreference = "Continue"
        try { & $Candidate[0] @Arguments 2>$null | Out-Null; $Found = ($LASTEXITCODE -eq 0) } catch { $Found = $false }
        $ErrorActionPreference = $Previous
        if ($Found) { return $Candidate }
    }
    return $null
}

Refresh-Path

# 1. ffmpeg --------------------------------------------------------------------------------
if (-not (Has "ffmpeg") -or -not (Has "ffprobe")) {
    Say "Euhpen a besoin de ffmpeg, qui n'est pas installé."
    if ((Has "winget") -and (Ask "L'installer maintenant avec winget (paquet Gyan.FFmpeg) ?")) {
        winget install --id Gyan.FFmpeg -e
        Refresh-Path
        if (-not (Has "ffmpeg")) { Fail "ffmpeg est installé : fermez cette fenêtre et relancez Euhpen." }
    } else {
        Fail "Installez ffmpeg (winget install Gyan.FFmpeg, ou https://www.gyan.dev/ffmpeg/builds/ en l'ajoutant au PATH), puis relancez Euhpen."
    }
}

# 2. Rust et compilation -------------------------------------------------------------------
if (-not (Has "cargo")) {
    Say "Euhpen se compile avec Rust, qui n'est pas installé."
    if ((Has "winget") -and (Ask "L'installer maintenant avec winget (paquet Rustlang.Rustup) ?")) {
        winget install --id Rustlang.Rustup -e
        Refresh-Path
    }
    if (-not (Has "cargo")) { Fail "Installez Rust depuis https://rustup.rs, puis relancez Euhpen." }
}
$Cargo = @()
if (Has "rustup") {
    # Cette chaîne lie avec rust-lld (voir .cargo/config.toml) : ni Visual Studio ni MinGW.
    if (-not ((& rustup toolchain list) -match [regex]::Escape($RustToolchain))) {
        Say "Installation de la chaîne de compilation Rust…"
        Run "rustup" @("toolchain", "install", $RustToolchain, "--profile", "minimal", "--no-self-update")
    }
    $Cargo = @("+$RustToolchain")
}
if (-not (Test-Path $Gui)) { Say "Compilation d'Euhpen (quelques minutes, la première fois seulement)…" }
Run "cargo" ($Cargo + @("build", "--release", "--locked", "--bins"))

# 3. Python du modèle (refait si python\requirements.txt a changé) --------------------------
$Signature = (Get-FileHash python\requirements.txt -Algorithm SHA1).Hash.ToLower()
$WantHes = ""
$Installed = ""
$Known = @($Stamp, (Join-Path $Venv ".euhpen-install")) | Where-Object { Test-Path $_ } | Select-Object -First 1
if ($Known) {
    $Lines = Get-Content $Known
    $WantHes = [string](($Lines | Where-Object { $_ -like "hesitations=*" }) -replace "^hesitations=", "")
    $Installed = [string](($Lines | Where-Object { $_ -like "requirements=*" }) -replace "^requirements=", "")
}
if (-not $WantHes) {
    Write-Host ""
    Write-Host "La détection des « euh » utilise Whisper (OpenAI) et une tête entraînée sur le corpus Rhapsodie,"
    Write-Host "sous licence CC BY-NC-SA 3.0 FR : usage non commercial. Elle ajoute environ"
    Write-Host "3 Go (PyTorch) ; Whisper est téléchargé au premier usage (1,5 Go)."
    Write-Host "Sans elle, Euhpen coupe les silences normalement."
    $WantHes = if (Ask "Installer la détection des hésitations ?") { "1" } else { "0" }
}

if ($WantHes -eq "1" -and (-not (Test-Path $Py) -or $Installed -ne $Signature)) {
    Say "Installation du modèle (première fois ou mise à jour)…"
    if (-not (Test-Path $Py)) {
        if (Has "uv") {
            Run "uv" @("venv", "--python", "3.12", $Venv)
        } else {
            $Base = Find-Python
            if (-not $Base) {
                Fail "Il faut Python 3.10 ou plus récent : winget install Python.Python.3.12 (ou installez uv : https://docs.astral.sh/uv/), puis relancez Euhpen."
            }
            Run $Base[0] (@($Base | Select-Object -Skip 1) + @("-m", "venv", $Venv))
        }
    }
    if (-not (Has "uv")) { Run $Py @("-m", "pip", "install", "--upgrade", "pip") }

    $Nvidia = Has "nvidia-smi"
    if ($Nvidia) {
        Say "Carte NVIDIA détectée : installation de PyTorch pour CUDA 12.8 (cartes RTX 20 à RTX 50)…"
        Pip @("torch", "--index-url", $CudaIndex)
    }
    Pip @("-r", "python\requirements.txt")

    # Une version sans CUDA déjà présente satisfait les commandes ci-dessus : on vérifie pour de vrai.
    if ($Nvidia -and -not (Cuda-Available)) {
        Say "PyTorch ne voit pas la carte NVIDIA : réinstallation de la version CUDA…"
        $Force = if (Has "uv") { @("--reinstall-package", "torch") } else { @("--force-reinstall", "--no-deps") }
        Pip (@("torch", "--index-url", $CudaIndex) + $Force)
        if (Cuda-Available) {
            Say "CUDA est maintenant disponible."
        } else {
            Write-Host "`nPyTorch ne voit toujours pas la carte NVIDIA : la transcription tournera sur le processeur, plus lentement." -ForegroundColor Yellow
            Write-Host "Mettez à jour le pilote NVIDIA, puis supprimez le fichier .euhpen-install et relancez Euhpen." -ForegroundColor Yellow
        }
    }
    $Installed = $Signature
}
Set-Content -Path $Stamp -Value @("requirements=$Installed", "hesitations=$WantHes") -Encoding ASCII

# 4. Lancement -----------------------------------------------------------------------------
Say "Ouverture d'Euhpen."
Start-Process -FilePath (Resolve-Path $Gui).Path -WorkingDirectory (Get-Location).Path
exit 0
