# Poids de la tête de détection des hésitations

Ce dossier contient `head.pt` et `head.json` : un petit réseau qui repère les « euh » et les syllabes
étirées à partir des états de l'encodeur de Whisper.

**La licence MIT du code d'Euhpen ne s'applique pas à ces deux fichiers.**

## Licence des poids

Ces poids ont été entraînés sur le corpus **Rhapsodie** et sont distribués, comme lui, sous licence
**Creative Commons Attribution – Pas d'utilisation commerciale – Partage dans les mêmes conditions
3.0 France (CC BY-NC-SA 3.0 FR)** :
<https://creativecommons.org/licenses/by-nc-sa/3.0/fr/>

- **Attribution** : créditez le corpus Rhapsodie (<https://rhapsodie.modyco.fr>) et Euhpen.
- **Pas d'utilisation commerciale** : ni ces poids, ni un modèle qui en dérive, ne peuvent servir
  à un usage commercial.
- **Partage dans les mêmes conditions** : un modèle qui en dérive doit être diffusé sous la même
  licence.

Aucun extrait du corpus ne figure dans ces fichiers. `head.json` indique les identifiants des
enregistrements utilisés, les seuils de détection et l'historique de l'entraînement.

## Ce qui n'est pas inclus

Whisper (`openai/whisper-large-v3-turbo`, OpenAI, licence MIT) n'est pas redistribué ici. Euhpen le
télécharge depuis Hugging Face au premier usage.
