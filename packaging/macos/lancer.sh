#!/bin/bash
# Appelé par « Lancer Euhpen.command » : installe ce qui manque, compile, puis ouvre l'application.

cd "$(dirname "$0")/../.." || exit 1
VENV=".venv"
STAMP=".euhpen-install"
export PATH="/opt/homebrew/bin:/usr/local/bin:$HOME/.local/bin:$HOME/.cargo/bin:$PATH"

say() { printf '\n\033[1m%s\033[0m\n' "$1"; }
fail() {
  printf '\n\033[31m%s\033[0m\n' "$1"
  read -r -p "Appuyez sur Entrée pour fermer." _
  exit 1
}
ask() {
  local reply
  read -r -p "$1 [o/N] " reply
  [[ "$reply" =~ ^[oOyY] ]]
}
python_ok() { "$1" -c 'import sys; sys.exit(sys.version_info < (3, 10))' 2>/dev/null; }

# 1. ffmpeg -------------------------------------------------------------------------------
if ! command -v ffmpeg >/dev/null || ! command -v ffprobe >/dev/null; then
  say "Euhpen a besoin de ffmpeg, qui n'est pas installé."
  if command -v brew >/dev/null; then
    if ask "L'installer maintenant avec Homebrew (brew install ffmpeg) ?"; then
      brew install ffmpeg || fail "L'installation de ffmpeg a échoué."
    else
      fail "Installez ffmpeg (brew install ffmpeg), puis relancez Euhpen."
    fi
  else
    fail "Installez Homebrew (https://brew.sh), puis dans un terminal : brew install ffmpeg. Relancez ensuite Euhpen."
  fi
fi

# 2. Rust et compilation ------------------------------------------------------------------
if ! xcode-select -p >/dev/null 2>&1; then
  xcode-select --install
  fail "Installez les outils de développement d'Apple dans la fenêtre qui vient de s'ouvrir, puis relancez Euhpen."
fi
if ! command -v cargo >/dev/null; then
  say "Euhpen se compile avec Rust, qui n'est pas installé."
  if command -v brew >/dev/null && ask "L'installer maintenant avec Homebrew (brew install rust) ?"; then
    brew install rust || fail "L'installation de Rust a échoué."
  else
    fail "Installez Rust depuis https://rustup.rs, puis relancez Euhpen."
  fi
fi
[ -x target/release/euhpen-gui ] || say "Compilation d'Euhpen (quelques minutes, la première fois seulement)…"
cargo build --release --locked --bins || fail "La compilation a échoué (voir les messages ci-dessus)."

# 3. Python du modèle (refait si python/requirements.txt a changé) -------------------------
signature="$(shasum python/requirements.txt | cut -d' ' -f1)"
known="$STAMP"
[ -f "$known" ] || known="$VENV/.euhpen-install"
want_hes="$(sed -n 's/^hesitations=//p' "$known" 2>/dev/null)"
installed="$(sed -n 's/^requirements=//p' "$known" 2>/dev/null)"
if [ -z "$want_hes" ]; then
  echo
  echo "La détection des « euh » utilise Whisper (OpenAI) et une tête entraînée sur le corpus Rhapsodie,"
  echo "sous licence CC BY-NC-SA 3.0 FR : usage non commercial. Elle ajoute environ"
  echo "1,5 Go (PyTorch) ; Whisper est téléchargé au premier usage (1,5 Go)."
  echo "Sans elle, Euhpen coupe les silences normalement."
  if ask "Installer la détection des hésitations ?"; then want_hes=1; else want_hes=0; fi
fi

if [ "$want_hes" = 1 ] && { [ ! -x "$VENV/bin/python" ] || [ "$installed" != "$signature" ]; }; then
  say "Installation du modèle (première fois ou mise à jour)…"
  if command -v uv >/dev/null; then
    if [ ! -x "$VENV/bin/python" ]; then
      uv venv --python 3.12 "$VENV" || fail "Impossible de créer l'environnement Python."
    fi
    uv pip install --python "$VENV/bin/python" -r python/requirements.txt || fail "L'installation a échoué (voir les messages ci-dessus)."
  else
    PY=""
    for candidate in python3.13 python3.12 python3.11 python3.10 python3; do
      if command -v "$candidate" >/dev/null && python_ok "$candidate"; then PY="$candidate"; break; fi
    done
    [ -n "$PY" ] || fail "Il faut Python 3.10 ou plus récent : brew install python@3.12 (ou installez uv : https://docs.astral.sh/uv/), puis relancez Euhpen."
    if [ ! -x "$VENV/bin/python" ]; then
      "$PY" -m venv "$VENV" || fail "Impossible de créer l'environnement Python."
    fi
    "$VENV/bin/python" -m pip --version >/dev/null 2>&1 || "$VENV/bin/python" -m ensurepip --upgrade >/dev/null
    "$VENV/bin/python" -m pip install --upgrade pip >/dev/null
    "$VENV/bin/python" -m pip install -r python/requirements.txt || fail "L'installation a échoué (voir les messages ci-dessus)."
  fi
  installed="$signature"
fi
printf 'requirements=%s\nhesitations=%s\n' "$installed" "$want_hes" > "$STAMP"

# 4. Lancement ----------------------------------------------------------------------------
if [ -d "$HOME/Library/Mobile Documents/com~apple~CloudDocs/Desktop" ]; then
  case "$PWD/" in
    "$HOME/Desktop/"*|"$HOME/Documents/"*)
      echo "Remarque : ce dossier est synchronisé par iCloud, qui va aussi envoyer .venv et target (plusieurs Go)"
      echo "dans le cloud. Pour l'éviter, déplacez le dossier Euhpen hors du Bureau et de Documents." ;;
  esac
fi
say "Ouverture d'Euhpen."
exec target/release/euhpen-gui
