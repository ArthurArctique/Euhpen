#!/bin/bash
# Euhpen : double-cliquez sur ce fichier pour ouvrir l'application.
# La première fois, il installe ce qui manque et compile Euhpen (quelques minutes).
# Si macOS refuse de l'ouvrir : clic droit > Ouvrir, ou dans un terminal :
#   chmod +x "Lancer Euhpen.command"

exec bash "$(dirname "$0")/packaging/macos/lancer.sh"
